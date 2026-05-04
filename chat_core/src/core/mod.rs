use std::{collections::HashMap, num::NonZeroUsize, path::PathBuf, sync::Arc, time::Instant};

use libp2p::{PeerId, Swarm};
use lru::LruCache;
use tokio::sync::mpsc;
use tokio::try_join;
use zeroize::Zeroizing;

/// 消息通道容量
const CHANNEL_CAPACITY: usize = 64;
/// mDNS 缓存大小
const MDNS_CACHE_SIZE: usize = 2000;
/// DHT 定期注册间隔（秒）- 5分钟
pub(crate) const DHT_REGISTRATION_INTERVAL_SECS: u64 = 300;

use crate::{
    command::{ChatCommand, ChatcoreEvent, MessageEvent},
    coreconfig::CoreConfig,
    corehandle::CoreHandle,
    identity,
    log::init_logger,
    message::{ChatMessage, ChatMessageType},
    p2p, storage,
    transfer::FileTransferState,
};

pub mod command_handler;
pub mod dht_ops;
pub mod event_loop;
pub mod file_transfer;
pub mod identity_ops;

/// 聊天核心：管理 P2P 网络、命令处理、消息分发
pub struct ChatCore {
    /// libp2p 网络 swarm，管理所有连接和协议
    pub(crate) swarm: Swarm<p2p::MyBehaviour>,
    /// DHT 记录验证器（基于签名验证）
    pub(crate) validator: Arc<std::sync::RwLock<p2p::RecordValidator>>,
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
    pub(crate) mdns_cache: LruCache<PeerId, Instant>,
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
    /// 缓存的 DHT 数据库连接（避免每次发送消息都打开/关闭数据库）
    pub(crate) dht_db: Option<std::sync::Arc<redb::Database>>,
    /// 已建立连接的 PeerID 集合（用于在线状态计数）
    pub(crate) connected_peers: std::collections::HashSet<PeerId>,
}

impl ChatCore {
    /// 异步初始化核心
    pub async fn try_init(cfg: CoreConfig) -> anyhow::Result<Self> {
        if let Err(e) = init_logger(&cfg) {
            return Err(anyhow::anyhow!("Failed to init logger:{}", e));
        };

        // 确保数据目录存在，防止因目录缺失导致身份文件无法读写而反复生成新身份
        if let Err(e) = std::fs::create_dir_all(&cfg.data_dir) {
            tracing::warn!("Failed to create data directory {:?}: {}", cfg.data_dir, e);
        }
        tracing::info!("Initializing chat core with data dir: {:?}", cfg.data_dir);

        let _ = try_join!(storage::init(&cfg))?;

        // 加载或生成完整身份（ML-DSA 持久化 + ML-KEM 临时）
        let (mldsa_public_key, mlkem_public_key) =
            identity::load_or_generate_complete_identity(&cfg).await?;
        let mldsa_public_key = mldsa_public_key.to_vec();
        let mldsa_identity_id = hex::encode(&mldsa_public_key);
        let mlkem_pubkey_hex = hex::encode(&mlkem_public_key);
        tracing::info!(
            "Loaded identity: ML-DSA={}, ML-KEM={} (ephemeral)",
            &mldsa_identity_id[..16],
            &mlkem_pubkey_hex[..16]
        );

        // 加载 ML-DSA 私钥并缓存到内存（避免每次发送消息都访问 Keyring）
        // 使用 Zeroizing 包装，确保私钥在内存中可被自动清零
        let mldsa_private_key = {
            let mut handle = rootcell::identity::PrivateKeyHandle::load(
                &cfg.data_dir.to_string_lossy(),
                &format!("{}_mldsa", mldsa_identity_id),
                cfg.passwd.as_deref(),
            )?;

            // 如果当前是密码派生模式但 Keyring 可用，自动升级到 Keyring 存储
            if let Err(e) = handle.try_upgrade_to_keyring() {
                tracing::warn!(
                    "Failed to upgrade private key for {} to Keyring: {}",
                    &mldsa_identity_id[..16],
                    e
                );
            }

            Zeroizing::new(handle.get_private_key().to_vec())
        };

        // 为当前会话生成临时的 libp2p PeerID（不持久化）
        let keypair = identity::generate_temporary_peerid()?;
        let peer_id = keypair.public().to_peer_id();
        tracing::info!("Generated temporary PeerID for transport: {}", peer_id);

        let p2p::SwarmWithValidator { swarm, validator } =
            p2p::swarm_init(&cfg.data_dir, keypair.clone())?;

        // 创建消息通道：容量 32，背压控制防止内存溢出
        let (tx, rx) = mpsc::channel(CHANNEL_CAPACITY);
        let mdns_cache = LruCache::new(NonZeroUsize::new(MDNS_CACHE_SIZE).unwrap());

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

        // 初始化 DHT 数据库连接（缓存，避免每次发送消息都打开/关闭）
        let dht_db = {
            let dht_path = cfg.data_dir.join("dht.redb");
            match redb::Database::create(&dht_path) {
                Ok(db) => Some(Arc::new(db)),
                Err(e) => {
                    tracing::warn!("Failed to open DHT database for caching: {}", e);
                    None
                }
            }
        };

        Ok(ChatCore {
            swarm,
            validator,
            identity_keypair: keypair,
            tx_message: tx,
            rx_message: Some(rx),
            rx_cmd: cmd_rx,
            mdns_cache,
            data_dir: cfg.data_dir.clone(),
            core_handle: CoreHandle { cmd_tx },
            mldsa_pubkey_hex: Some(mldsa_pubkey_hex_for_dht),
            current_peer_id: Some(peer_id_for_dht),
            mldsa_identity_id: Some(mldsa_identity_id),
            mlkem_pubkey_hex: Some(mlkem_pubkey_hex_for_dht),
            mldsa_private_key: Some(mldsa_private_key),
            download_dir,
            file_transfers: HashMap::new(),
            file_path_map: HashMap::new(),
            dht_db,
            connected_peers: std::collections::HashSet::new(),
        })
    }

    /// 获取缓存的 DHT 数据库连接，返回 RedbRecordStore
    /// 如果数据库连接不可用，返回错误
    pub(crate) fn get_dht_store(&self) -> anyhow::Result<p2p::dht::RedbRecordStore> {
        match self.dht_db {
            Some(ref db) => Ok(p2p::dht::RedbRecordStore::new(db.clone())),
            None => Err(anyhow::anyhow!("DHT 数据库连接未初始化")),
        }
    }

    /// 获取消息接收通道（用于外部 UI 接收核心事件）
    pub fn take_rx_message(&mut self) -> Option<mpsc::Receiver<ChatcoreEvent>> {
        self.rx_message.take()
    }

    pub fn handler(&self) -> CoreHandle {
        self.core_handle.clone()
    }

    /// 发送消息到网络
    pub(crate) fn send_message(&mut self, peerid: PeerId, message: ChatMessage) {
        self.swarm
            .behaviour_mut()
            .rr_msg
            .send_request(&peerid, message);
    }

    pub(crate) async fn build_signed_message(
        &self,
        msgtype: ChatMessageType,
        data: Vec<u8>,
    ) -> anyhow::Result<ChatMessage> {
        // data 已由调用方预处理（FileStream 类型在 FileStreamChunk::from_file 中压缩）
        let processed_data = data;

        // 第二步：同步签名（使用内存缓存的 ML-DSA 私钥）
        let mldsa_private_key = self
            .mldsa_private_key
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("ML-DSA private key not cached in memory"))?;
        let mldsa_public_key =
            crate::identity::extract_public_key_from_private(mldsa_private_key, true)?;
        ChatMessage::new_signed(
            msgtype,
            processed_data,
            mldsa_private_key,
            &mldsa_public_key,
        )
    }

    /// 发送日志事件到外部通道
    pub(crate) async fn send_log_mpsc(&mut self, data: String) {
        if let Err(e) = self.tx_message.send(MessageEvent::Log(data)).await {
            tracing::error!("Failed to send log message: {e}");
        }
    }

    /// 发送警告事件到外部通道
    pub(crate) async fn send_warning_mpsc(&mut self, data: String) {
        if let Err(e) = self.tx_message.send(MessageEvent::Warning(data)).await {
            tracing::error!("Failed to send warning message: {e}");
        }
    }

    /// 发送新消息事件到外部通道
    pub async fn send_message_mpsc(&mut self, msg: crate::command::IncomingMessage) {
        if let Err(e) = self
            .tx_message
            .send(MessageEvent::ReceiveMessage(msg))
            .await
        {
            tracing::error!("Failed to send message event: {e}");
        }
    }
}

impl Drop for ChatCore {
    fn drop(&mut self) {
        let _ = self.core_handle.cmd_tx.try_send(ChatCommand::Shutdown);
    }
}
