use std::{collections::HashMap, path::PathBuf, sync::Arc};

use aws_lc_rs::kem::DecapsulationKey;
use libp2p::PeerId;
use tokio::sync::mpsc;
use tokio::try_join;
use tokio_util::sync::CancellationToken;
use zeroize::Zeroizing;

/// 消息通道容量（背压控制，防止内存溢出）
const CHANNEL_CAPACITY: usize = 64;
/// DHT 定期注册间隔（秒）- 5分钟
pub(crate) const DHT_REGISTRATION_INTERVAL_SECS: u64 = 300;

use crate::{
    actor::p2p::{P2pActor, P2pActorHandle, P2pCommand, P2pEvent},
    command::{ChatCommand, ChatcoreEvent, MessageEvent},
    coreconfig::CoreConfig,
    corehandle::CoreHandle,
    error::{CoreError, CoreResult},
    identity,
    log::init_logger,
    message::{ChatMessage, ChatMessageType},
    p2p, storage,
    p2p::dht_cache::DhtCache,
    transfer::FileTransferState,
};
/// 命令处理
pub mod command_handler;
/// 联系人操作
pub mod contact_ops;
/// DHT 操作（身份发布至 Kademlia 网络）
pub mod dht_ops;
/// 事件处理
pub mod event_loop;
///（未稳定标记）文件传输
pub mod file_transfer;
/// 联系人操作
pub mod identity_ops;
/// 消息操作
pub mod message_ops;
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
    /// 临时传输层 PeerID 密钥对（Ed25519，每次启动重新生成）
    #[allow(dead_code)]
    pub(crate) identity_keypair: libp2p::identity::Keypair,
    /// 消息发送通道：向外部（UI）发送事件
    pub(crate) tx_message: mpsc::Sender<ChatcoreEvent>,
    /// 消息接收通道：外部可取走事件（Option 用于 run() 时 take）
    #[allow(dead_code)]
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
    /// 缓存的 ML-DSA 私钥（避免每次发送消息都从 Keyring 加载）
    /// 仅在内存中保留，不持久化
    /// 使用 Zeroizing 包装，确保 drop 时自动清零内存
    pub(crate) mldsa_private_key: Option<Zeroizing<Vec<u8>>>,
    /// 文件下载目录
    pub(crate) download_dir: PathBuf,
    /// 活跃的文件传输状态（file_id -> FileTransferState）
    pub(crate) file_transfers: HashMap<String, FileTransferState>,
    /// 文件路径映射（file_id -> 本地文件路径），用于发送方查找文件
    pub(crate) file_path_map: HashMap<[u8; 32], PathBuf>,
    /// 上次文件传输超时扫描时间（限制扫描频率，避免每分片触发）
    pub(crate) last_file_timeout_scan: std::time::Instant,
    /// 内存 DHT 缓存
    pub(crate) dht_cache: Arc<DhtCache>,
    /// 已建立连接的 PeerID 及其连接数（用于在线状态计数）
    /// 使用 HashMap<PeerId, usize> 以支持每个 Peer 有多条连接（如 mDNS 双端互拨）
    /// ConnectionEstablished 时 +1，ConnectionClosed 时 -1，减到 0 才移除
    pub(crate) connected_peers: std::collections::HashMap<PeerId, usize>,
    /// PeerID → ML-DSA 公钥 hex 的内存缓存
    ///
    /// 在 ConnectionEstablished 时从 DHT 反向查找并缓存，
    /// 在 handle_incoming_request 中 set_pubkey_peerid 后更新。
    /// 避免因 DHT 写入延迟导致在线状态无法正确显示。
    pub(crate) peerid_to_pubkey: HashMap<PeerId, String>,
    /// 当前会话的 ML-KEM 解封装密钥对象（缓存，避免序列化/反序列化问题）
    ///
    /// # 设计说明
    /// aws-lc-rs 的 `DecapsulationKey::key_bytes()` 输出格式与
    /// `DecapsulationKey::new()` 输入格式不兼容（已知的库限制），
    /// 因此无法通过序列化/反序列化私钥字节来重建 DecapsulationKey。
    /// 解决方案是在 ChatCore 中缓存 DecapsulationKey 对象，直接传入引用。
    ///
    /// 此字段在 try_init() 中初始化，生命周期与 ChatCore 实例相同。
    /// 每次会话重新生成 ML-KEM 密钥对时，此字段也会更新。
    pub(crate) mlkem_decap_key: Option<DecapsulationKey>,
    /// 配置的中继节点列表 [(PeerId, Multiaddr)]
    pub(crate) relay_nodes: Vec<(String, String)>,
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
        tracing::info!(
            "Loaded identity: ML-DSA={}, ML-KEM={} (ephemeral)",
            &mldsa_identity_id[..16],
            &mlkem_pubkey_hex[..16]
        );

        // 加载 ML-DSA 私钥并缓存到内存（避免每次发送消息都访问 Keyring）
        // 使用 Zeroizing 包装，确保私钥在内存中可被自动清零
        let mldsa_private_key = {
            let handle = rootcell::identity::PrivateKeyHandle::load(
                &cfg.data_dir.to_string_lossy(),
                &format!("{}_mldsa", mldsa_identity_id),
            )
            .map_err(|e| {
                CoreError::InitFailed(format!("Failed to load ML-DSA private key: {}", e))
            })?;

            Zeroizing::new(handle.get_private_key().to_vec())
        };

        // 为当前会话生成临时的 libp2p PeerID（不持久化）
        let keypair = identity::generate_temporary_peerid().map_err(|e| {
            CoreError::InitFailed(format!("Failed to generate temporary PeerID: {}", e))
        })?;
        let peer_id = keypair.public().to_peer_id();
        tracing::info!("Generated temporary PeerID for transport: {}", peer_id);

        // 初始化内存 DHT 缓存
        let dht_cache = DhtCache::new();

        // 启动时清理 contacts 表中过时的 ML-KEM 公钥
        // 每次启动都会生成新的临时 ML-KEM 密钥对，旧的 ML-KEM 公钥已失效。
        // 如果不清理，lookup_mlkem_pubkey 在 DHT 本地数据库未命中时会回退到
        // contacts 表获取过时的公钥，导致加密消息后接收方解密失败。
        if let Some(pool) = storage::pool() {
            if let Err(e) = storage::clear_all_mlkem_pubkeys(pool).await {
                tracing::warn!("启动时清理过时 ML-KEM 公钥失败: {}", e);
            } else {
                tracing::info!("启动时已清理 contacts 表中所有过时的 ML-KEM 公钥");
            }
        }

        // 加载节点配置（relay 和 bootstrap 节点）
        let relay_nodes: Vec<(String, String)> = cfg.relay_nodes.clone();
        let bootstrap_nodes: Vec<(String, String)> = cfg.bootstrap_nodes.clone();

        let swarm = p2p::swarm_init(
            &cfg.data_dir,
            keypair.clone(),
            &bootstrap_nodes,
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

        let download_dir = cfg
            .download_dir
            .clone()
            .unwrap_or_else(|| cfg.data_dir.join("downloads"));

        // 确保下载目录存在
        if let Err(e) = std::fs::create_dir_all(&download_dir) {
            tracing::warn!(
                "Failed to create download directory {:?}: {}",
                download_dir,
                e
            );
        }

        let shutdown_token = CancellationToken::new();

        // 创建 P2pActor 事件通道
        let (p2p_event_tx, p2p_event_rx) = mpsc::channel::<P2pEvent>(CHANNEL_CAPACITY);

        // 创建 P2pActor 并启动
        let p2p_actor = P2pActor::new(swarm, dht_cache.clone(), cfg.data_dir.clone(), p2p_event_tx, cfg.relay_nodes.clone());
        let p2p_handle =
            crate::actor::p2p::start_p2p_actor(p2p_actor, CHANNEL_CAPACITY, shutdown_token.clone());

        // 可选：启动 WebSocket 信令 Actor
        if let Some(sig_host) = &cfg.signaling_server {
            let room = cfg.signaling_room.clone()
                .unwrap_or_else(|| mldsa_identity_id[..16].to_string());
            tracing::info!("SignalingActor starting: server={sig_host}, room={room}");
            let (_sig_event_tx, _sig_event_rx) = mpsc::channel::<crate::actor::signaling::SignalingEvent>(8);
            let signal_actor = crate::actor::signaling::SignalingActor::new(
                sig_host.clone(),
                room,
                peer_id,
                p2p_handle.tx.clone(),
                _sig_event_tx,
                shutdown_token.clone(),
            );
            signal_actor.start();
            tracing::info!("SignalingActor started");
        }

        Ok(ChatCore {
            p2p_handle,
            rx_p2p_event: p2p_event_rx,
            identity_keypair: keypair,
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
            download_dir,
            file_transfers: HashMap::new(),
            file_path_map: HashMap::new(),
            last_file_timeout_scan: std::time::Instant::now(),
            dht_cache,
            connected_peers: std::collections::HashMap::new(),
            peerid_to_pubkey: HashMap::new(),
            mlkem_decap_key: Some(mlkem_decap_key),
            relay_nodes: cfg.relay_nodes.clone(),
        })
    }

    /// 获取 DHT 缓存
    pub(crate) fn get_dht_store(&self) -> &DhtCache {
        &self.dht_cache
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
    pub(crate) async fn send_message(&mut self, peerid: PeerId, message: ChatMessage) {
        let _ = self
            .p2p_handle
            .send(crate::actor::ActorCommand::Custom(
                P2pCommand::SendMessage {
                    peer_id: peerid,
                    message,
                },
            ))
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
        let _ = self
            .tx_message
            .try_send(MessageEvent::ReceiveMessage(msg));
    }

    /// 发送在线状态更新事件（独立事件，不混入消息历史）
    ///
    /// 将当前所有已连接 PeerID 反向解析为 ML-DSA 公钥 hex，
    /// 发送给上层 UI 以便显示每个联系人的在线/离线状态。
    pub(crate) async fn send_online_status(&mut self) {
        let online_contacts = self.resolve_online_contacts();
        let _ = self.tx_message.try_send(MessageEvent::OnlineStatus { online_contacts });
    }

    /// 解析当前所有已连接 PeerID 对应的 ML-DSA 公钥 hex
    ///
    /// 优先使用内存缓存 `peerid_to_pubkey`，如果缓存中没有则回退到 DHT 查询。
    /// 找到后自动写入缓存，避免后续重复查询。
    fn resolve_online_contacts(&self) -> Vec<String> {
        let store = self.get_dht_store();
        let mut online = Vec::with_capacity(self.connected_peers.len());
        for peer_id in self.connected_peers.keys() {
            // 1. 优先使用内存缓存
            if let Some(pubkey_hex) = self.peerid_to_pubkey.get(peer_id) {
                online.push(pubkey_hex.clone());
                continue;
            }
            // 2. 回退到 DHT 查询
            match store.get_pubkey_by_peerid(peer_id) {
                Ok(Some(pubkey_hex)) => {
                    online.push(pubkey_hex);
                }
                _ => {
                    tracing::trace!("Peer {peer_id} has no pubkey mapping yet");
                }
            }
        }
        online
    }

    /// 更新 PeerID → ML-DSA 公钥 hex 的内存缓存
    ///
    /// 当 `set_pubkey_peerid` 被调用时（如收到入站请求后），
    /// 同步更新内存缓存，确保后续在线状态查询能立即反映新映射。
    /// 如果该 PeerID 当前已连接，则触发一次在线状态刷新。
    pub(crate) async fn update_peerid_pubkey_mapping(
        &mut self,
        peer_id: PeerId,
        pubkey_hex: String,
    ) {
        let is_new = self.peerid_to_pubkey.insert(peer_id, pubkey_hex).is_none();
        // 如果这是新映射且该 PeerID 当前已连接，刷新在线状态
        if is_new && self.connected_peers.contains_key(&peer_id) {
            self.send_online_status().await;
        }
    }
}

impl Drop for ChatCore {
    fn drop(&mut self) {
        // 先触发取消信号，让 DHT 注册循环等后台任务优雅退出
        self.core_handle.shutdown_token.cancel();
        // 再发送关闭命令，让主事件循环退出
        let _ = self.core_handle.cmd_tx.try_send(ChatCommand::Shutdown);

        // 通知 P2pActor 保存路由表并关闭
        // 注意：drop 是同步上下文，不能使用 send_blocking（会 panic），
        // 使用 try_send 非阻塞发送，如果通道满则丢弃（P2pActor 即将关闭）
        let _ = self
            .p2p_handle
            .tx
            .try_send(crate::actor::ActorCommand::Custom(
                P2pCommand::SaveRoutingTable,
            ));
        let _ = self
            .p2p_handle
            .tx
            .try_send(crate::actor::ActorCommand::Custom(P2pCommand::Shutdown));
    }
}
