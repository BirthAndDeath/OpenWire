use std::{collections::HashMap, path::PathBuf, time::Instant};

use aws_lc_rs::kem::DecapsulationKey;
use libp2p::PeerId;
use tokio::sync::mpsc;
use tokio::try_join;
use tokio_util::sync::CancellationToken;
use zeroize::Zeroizing;

/// 消息通道容量（背压控制，防止内存溢出）
pub(crate) const CHANNEL_CAPACITY: usize = 64;
/// DHT 定期注册间隔（秒）- 5分钟
pub(crate) const DHT_REGISTRATION_INTERVAL_SECS: u64 = 300;

use crate::{
    actor::p2p::{P2pActorHandle, P2pCommand, P2pEvent},
    command::{ChatCommand, ChatcoreEvent, MessageEvent},
    coreconfig::CoreConfig,
    corehandle::CoreHandle,
    error::{CoreError, CoreResult},
    identity,
    log::init_logger,
    message::{ChatMessage, ChatMessageType},
    p2p,
    storage,
};
/// 命令处理 + 事件循环
pub mod handle;
/// 联系人操作
pub mod contact;
/// DHT 操作（身份发布至 Kademlia 网络）
pub mod dht;
/// 文件传输
pub mod file_transfer_manager;
/// Peer 缓存
pub mod peer_cache;
/// 身份操作（生成、切换、删除）
pub mod identity_ops;
/// 消息操作
pub mod message;
/// 定时器任务（独立 tokio::spawn）
pub mod timers;

/// 聊天核心：管理 P2P 网络、命令处理、消息分发
///
/// # 架构变更说明
/// ChatCore 不再直接持有 libp2p Swarm，而是通过 P2pActorHandle 与 P2pActor 通信。
/// P2pActor 拥有 Swarm 的所有权，独立运行事件循环。
/// ChatCore 从 P2pEvent 接收通道获取网络事件。
pub struct ChatCore {
    /// P2pActor 句柄：用于向 P2pActor 发送命令
    pub(crate) p2p_handle: P2pActorHandle,
    /// P2pActor 事件接收通道：接收网络事件
    pub(crate) rx_p2p_event: mpsc::Receiver<P2pEvent>,
    /// 消息发送通道：向外部（UI）发送事件
    pub(crate) tx_message: mpsc::Sender<ChatcoreEvent>,
    /// 消息接收通道：外部可取走事件（Option 用于 run() 时 take）
    pub(crate) rx_message: Option<mpsc::Receiver<ChatcoreEvent>>,
    /// 命令接收通道：接收外部控制指令
    pub(crate) rx_cmd: mpsc::Receiver<ChatCommand>,
    pub(crate) data_dir: PathBuf,
    /// 核心句柄：用于外部控制核心
    pub core_handle: CoreHandle,
    /// 保存 ML-DSA 公钥十六进制字符串（唯一身份标识），用于 DHT 注册
    pub(crate) mldsa_pubkey_hex: Option<String>,
    /// 保存当前临时 PeerID，用于后续 DHT 注册
    pub(crate) current_peer_id: Option<PeerId>,
    /// ML-DSA 身份 ID（公钥 hex），用于加载持久化签名密钥
    pub(crate) mldsa_identity_id: Option<String>,
    /// 当前会话的 ML-KEM 公钥 hex（临时，用于 DHT 发布和前端显示）
    pub mlkem_pubkey_hex: Option<String>,
    /// 持久化的 PeerID 配置（Ed25519 密钥 + 端口偏好，8h~24h 随机 TTL）
    /// 在身份切换时复用，保持设备级 PeerID 稳定
    pub(crate) peerid_config: Option<crate::peerid_store::PeerIdConfig>,
    /// 缓存的 ML-DSA 私钥（避免每次发送消息都从 Keyring 加载）
    /// 仅在内存中保留，不持久化
    /// 使用 Zeroizing 包装，确保 drop 时自动清零内存
    pub(crate) mldsa_private_key: Option<Zeroizing<Vec<u8>>>,
    /// 文件传输状态管理
    pub(crate) file_transfer: file_transfer_manager::FileTransferManager,
    /// Peer 缓存（DHT + 映射）
    pub(crate) peer_cache: peer_cache::PeerCache,
    /// 已建立连接的 PeerID 及其连接数（用于在线状态计数）
    pub(crate) connected_peers: std::collections::HashMap<PeerId, usize>,
    /// 当前会话的 ML-KEM 解封装密钥对象（缓存，避免每次解密重建）
    pub(crate) mlkem_decap_key: Option<DecapsulationKey>,
    /// 配置的中继节点列表 [(PeerId, Multiaddr)]
    pub(crate) relay_nodes: Vec<(String, String)>,
    /// 配置的 bootstrap 节点列表 [(PeerId, Multiaddr)]
    pub(crate) bootstrap_nodes: Vec<(String, String)>,
    /// 每个联系人的最近发现时间（用于防重复发现冷却）
    pub(crate) last_discovery_time: HashMap<String, Instant>,
}

impl ChatCore {
    /// 异步初始化核心
    pub async fn try_init(cfg: CoreConfig) -> CoreResult<Self> {
        if let Err(e) = init_logger(&cfg) {
            return Err(CoreError::InitFailed(format!(
                "Failed to init logger: {}",
                e
            )));
        };

        // 确保数据目录存在，防止因目录缺失导致身份文件无法读写而反复生成新身份
        if let Err(e) = std::fs::create_dir_all(&cfg.data_dir) {
            tracing::warn!("Failed to create data directory {:?}: {}", cfg.data_dir, e);
        }
        tracing::info!("Initializing chat core with data dir: {:?}", cfg.data_dir);

        let _ = try_join!(storage::init(&cfg))
            .map_err(|e| CoreError::InitFailed(format!("Storage init failed: {}", e)))?;

        // 加载或生成完整身份（ML-DSA 持久化 + ML-KEM 临时）
        let identity = identity::load_or_generate_complete_identity(&cfg)
            .await
            .map_err(|e| CoreError::InitFailed(format!("Identity init failed: {}", e)))?;
        let mldsa_public_key = identity.mldsa_public_key.to_vec();
        let mldsa_identity_id = hex::encode(&mldsa_public_key);
        let mlkem_pubkey_hex = hex::encode(&identity.mlkem_public_key);
        let mlkem_decap_key = identity.mlkem_decap_key;
        let mldsa_private_key = identity.mldsa_private_key;
        tracing::info!(
            "Loaded identity: ML-DSA={}, ML-KEM={} (ephemeral)",
            &mldsa_identity_id[..16],
            &mlkem_pubkey_hex[..16]
        );

        // 加载或创建持久化的 PeerID
        let (keypair, peerid_config) = identity::load_or_create_peerid(&cfg.data_dir)
            .map_err(|e| CoreError::InitFailed(format!("PeerID 初始化失败: {e}")))?;
        let peer_id = keypair.public().to_peer_id();
        tracing::info!("PeerID for transport: {}", peer_id);

        // 初始化内存 DHT 缓存
        let dht_cache = p2p::dht_cache::DhtCache::new();

        // 加载节点配置（bootstrap 节点）
        let bootstrap_nodes: Vec<(String, String)> = cfg.bootstrap_nodes.clone();

        let swarm = p2p::swarm_init(
            &cfg.data_dir,
            keypair.clone(),
            &bootstrap_nodes,
            Some(&peerid_config),
        )
        .map_err(|e| CoreError::InitFailed(format!("Swarm init failed: {}", e)))?;

        // 创建消息通道：容量 32，背压控制防止内存溢出
        let (tx, rx) = mpsc::channel(CHANNEL_CAPACITY);

        let (cmd_tx, cmd_rx) = mpsc::channel::<ChatCommand>(CHANNEL_CAPACITY);

        // 保存 ML-DSA pubkey 用于后续 DHT 注册
        let mldsa_pubkey_hex_for_dht = mldsa_identity_id.clone();
        let peer_id_for_dht = peer_id;
        // 保存 ML-KEM pubkey 用于后续 DHT 发布
        let mlkem_pubkey_hex_for_dht = mlkem_pubkey_hex.clone();

        let shutdown_token = CancellationToken::new();

        // 使用构建器创建并启动 P2pActor
        let (p2p_handle, rx_p2p_event) = crate::actor::p2p::P2pActorBuilder::new()
            .swarm(swarm)
            .dht_cache(dht_cache.clone())
            .data_dir(cfg.data_dir.clone())
            .relay_nodes(cfg.relay_nodes.clone())
            .bootstrap_nodes(cfg.bootstrap_nodes.clone())
            .channel_size(CHANNEL_CAPACITY)
            .cancellation_token(shutdown_token.clone())
            .start();

        let file_transfer = file_transfer_manager::FileTransferManager::new(cfg.data_dir.clone(), tx.clone());
        let peer_cache = peer_cache::PeerCache::new(dht_cache);
        Ok(ChatCore {
            p2p_handle,
            rx_p2p_event,
            peerid_config: Some(peerid_config),
            tx_message: tx,
            rx_message: Some(rx),
            rx_cmd: cmd_rx,
            data_dir: cfg.data_dir.clone(),
            core_handle: CoreHandle {
                cmd_tx,
                shutdown_token,
            },
            mldsa_pubkey_hex: Some(mldsa_pubkey_hex_for_dht),
            current_peer_id: Some(peer_id_for_dht),
            mldsa_identity_id: Some(mldsa_identity_id),
            mlkem_pubkey_hex: Some(mlkem_pubkey_hex_for_dht),
            mldsa_private_key: Some(mldsa_private_key),
            file_transfer,
            peer_cache,
            connected_peers: std::collections::HashMap::new(),
            mlkem_decap_key: Some(mlkem_decap_key),
            relay_nodes: cfg.relay_nodes.clone(),
            bootstrap_nodes: cfg.bootstrap_nodes.clone(),
            last_discovery_time: HashMap::new(),
        })
    }

    /// 获取 DHT 缓存
    pub(crate) fn get_dht_store(&self) -> &p2p::dht_cache::DhtCache {
        self.peer_cache.dht()
    }

    /// 获取消息接收通道（用于外部 UI 接收核心事件）
    pub fn take_rx_message(&mut self) -> Option<mpsc::Receiver<ChatcoreEvent>> {
        self.rx_message.take()
    }

    /// 获取核心句柄，用于外部控制核心（发送命令、关闭等）
    pub fn handler(&self) -> CoreHandle {
        self.core_handle.clone()
    }

    /// 通过 P2pActor 发送消息到网络
    pub(crate) async fn send_message(
        &mut self,
        peerid: PeerId,
        message: ChatMessage,
        message_hash: Option<&str>,
    ) {
        let _ = self
            .p2p_handle
            .send(
                P2pCommand::SendMessage {
                    peer_id: peerid,
                    message,
                    message_hash: message_hash.unwrap_or("").to_string(),
                },
            )
            .await;
    }

    /// 构建签名消息（加密后数据 + ML-DSA 签名）
    /// 构建签名消息（加密数据 + ML-DSA 签名 + 时间戳/nonce）
    pub(crate) async fn build_signed_message(
        &self,
        msgtype: ChatMessageType,
        data: Vec<u8>,
    ) -> CoreResult<ChatMessage> {
        // data 已由调用方预处理（FileStream 类型在 FileStreamChunk::from_file 中压缩）
        let processed_data = data;

        // 第二步：同步签名（使用内存缓存的 ML-DSA 私钥）
        let mldsa_private_key = self
            .mldsa_private_key
            .as_ref()
            .ok_or(CoreError::MlDsaPrivateKeyNotCached)?;
        let mldsa_public_key =
            crate::identity::extract_public_key_from_private(mldsa_private_key, true).map_err(
                |e| CoreError::InitFailed(format!("Failed to extract public key: {}", e)),
            )?;
        ChatMessage::new_signed(
            msgtype,
            processed_data,
            mldsa_private_key,
            &mldsa_public_key,
        )
        .map_err(CoreError::MessageError)
    }

    /// 发送日志事件到外部通道（try_send 避免阻塞事件循环）
    pub(crate) async fn send_log_mpsc(&mut self, data: String) {
        let _ = self.tx_message.try_send(MessageEvent::Log(data));
    }

    /// 发送警告事件到外部通道（try_send 避免阻塞事件循环）
    pub(crate) async fn send_warning_mpsc(&mut self, data: String) {
        let _ = self.tx_message.try_send(MessageEvent::Warning(data));
    }

    /// 发送新消息事件到外部通道（try_send 避免阻塞事件循环）
    pub async fn send_message_mpsc(&mut self, msg: crate::command::IncomingMessage) {
        let _ = self.tx_message.try_send(MessageEvent::ReceiveMessage(msg));
    }

    /// 发送在线状态更新事件（独立事件，不混入消息历史）
    ///
    /// 将当前所有已连接 PeerID 反向解析为 ML-DSA 公钥 hex，
    /// 发送给上层 UI 以便显示每个联系人的在线/离线状态。
    pub(crate) async fn send_online_status(&mut self) {
        let online_contacts = self.resolve_online_contacts();
        let _ = self
            .tx_message
            .try_send(MessageEvent::OnlineStatus { online_contacts });
    }

    /// 解析当前所有已连接 PeerID 对应的 ML-DSA 公钥 hex
    fn resolve_online_contacts(&self) -> Vec<String> {
        self.peer_cache.resolve_online(&self.connected_peers)
    }

    /// 更新 PeerID → ML-DSA 公钥 hex 的内存缓存
    pub(crate) async fn update_peerid_pubkey_mapping(
        &mut self,
        peer_id: PeerId,
        pubkey_hex: String,
    ) {
        let is_new = self.peer_cache.cache_pubkey(peer_id, pubkey_hex);
        if is_new && self.connected_peers.contains_key(&peer_id) {
            self.send_online_status().await;
        }
    }
}

impl Drop for ChatCore {
    fn drop(&mut self) {
        self.core_handle.shutdown_token.cancel();
    }
}
