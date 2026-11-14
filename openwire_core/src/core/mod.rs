use std::{collections::HashMap, path::PathBuf, sync::Arc, time::Instant};

use aws_lc_rs::kem::DecapsulationKey;
use libp2p::PeerId;
use sha2::Digest;
use tokio::sync::mpsc;
use tokio::try_join;
use tokio_util::sync::CancellationToken;
use zeroize::Zeroizing;

/// 消息通道容量（背压控制，防止内存溢出）
pub(crate) const CHANNEL_CAPACITY: usize = 64;
/// 入站消息限流：每 peer 时间窗
const INBOUND_MSG_WINDOW: std::time::Duration = std::time::Duration::from_secs(10);
/// 入站消息限流：每窗口每 peer 上限（远超人类对话频率，仅拦发送风暴）
const INBOUND_MSG_MAX_PER_WINDOW: u32 = 40;
/// 入站消息限流：新连接每 peer 首个时间窗的突发上限。
/// 好友重连时可能一次性推送大量离线积压消息，放宽首窗突发避免误伤合法积压
/// （消息被拒后 sender 会反复重试，可能造成送达饥饿）。
const INBOUND_MSG_BURST: u32 = 200;
/// 入站文件分片限流：每 peer 时间窗
/// 分片按 256KB 到达且受 rr_file 请求-响应 Ack 节流，合法传输远低于此峰值；
/// 独立于消息限流，避免大文件突发被消息窗口误伤。
const INBOUND_CHUNK_WINDOW: std::time::Duration = std::time::Duration::from_secs(10);
/// 入站文件分片限流：每窗口每 peer 上限（约 200 片/秒，远超合法传输但拦住冲刷）
const INBOUND_CHUNK_MAX_PER_WINDOW: u32 = 2000;
/// 入站加密失配（ML-KEM 指纹未命中 / 解密失败）限流：每 peer 时间窗
/// 正常重启/轮换后仅发生一两次（触发 FriendOnline 交换后自愈）；
/// 若同 peer 反复失配（攻击者用错误公钥持续加密轰炸，诱使我方反复回发
/// FriendOnline——签名+网络请求，被打爆同样是 DoS），超限即停止回发。
const INBOUND_MISMATCH_WINDOW: std::time::Duration = std::time::Duration::from_secs(30);
/// 入站加密失配限流：每窗口每 peer 上限（远超正常轮换，仅拦持续骚扰）
const INBOUND_MISMATCH_MAX_PER_WINDOW: u32 = 5;
/// 出站文件发送最大并发数（同一时刻后台发送任务数上限，防资源耗尽）
pub(crate) const MAX_OUTBOUND_FILE_SENDS: usize = 3;
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
    /// 正在进行的出站文件发送（"接收方:file_hash" 复合去重键集合）
    ///
    /// 由后台发送任务共享：spawn 前检查插入、任务结束移除，
    /// 防止同一文件对同一接收方并发 spawn 多个发送任务（资源耗尽面），
    /// 同时允许同一文件并发发给不同接收方。
    pub(crate) active_file_sends: Arc<std::sync::Mutex<std::collections::HashSet<String>>>,
    /// 文件传输状态管理
    pub(crate) file_transfer: file_transfer_manager::FileTransferManager,
    /// Peer 缓存（DHT + 映射）
    pub(crate) peer_cache: peer_cache::PeerCache,
    /// 已建立连接的 PeerID 及其连接数（用于在线状态计数）
    pub(crate) connected_peers: std::collections::HashMap<PeerId, usize>,
    /// 联系人屏蔽状态的最近已知结果缓存（查询失败时 fail-closed 兜底，
    /// 防止 DB 短暂故障把被屏蔽联系人的消息/文件请求误放行）
    pub(crate) last_known_blocked: std::collections::HashMap<String, bool>,
    /// 每 peer 入站消息限流滑窗：(窗口起点, 窗口内消息数, 是否首窗突发期)。
    /// 防止已连接对端高频冲刷签名验证与 SQLite 写入；
    /// 首窗突发期（INBOUND_MSG_BURST）覆盖好友重连的离线积压。
    pub(crate) inbound_msg_windows: std::collections::HashMap<
        PeerId,
        (std::time::Instant, u32, bool),
    >,
    /// 每 peer 入站文件分片限流滑窗：(窗口起点, 窗口内分片数, 是否首窗突发期)。
    /// 分片到达受 rr_file 请求-响应 Ack 节流，窗口上限取宽松值以防冲刷；
    /// 独立于消息限流，避免大文件突发被消息窗口误伤。
    pub(crate) inbound_chunk_windows: std::collections::HashMap<
        PeerId,
        (std::time::Instant, u32, bool),
    >,
    /// 每 peer 入站加密失配（指纹未命中/解密失败）限流滑窗：按**账号（pubkey）**计数。
    /// key = 发送方 ML-DSA 公钥 hex，而非 PeerID——因为多设备下攻击者控制目标账号
    /// 一台设备（持有账号私钥）即可生成任意多个 PeerID 逐个触发失配绕过 per-peer
    /// 限流；按账号计数则换 PeerID 无效。此限流只控制「是否回发 FriendOnline」，
    /// **不阻断连接建立/拨号**，因此不会成为"阻止我方连接某目标"的面。
    pub(crate) inbound_mismatch_windows: std::collections::HashMap<
        String,
        (std::time::Instant, u32),
    >,
    /// 当前会话的 ML-KEM 解封装密钥对象（缓存，避免每次解密重建）
    pub(crate) mlkem_decap_key: Option<DecapsulationKey>,
    /// 本节点的监听地址缓存（由 ConnectionEstablished 事件携带，用于重发 FriendOnline）
    pub(crate) own_listen_addrs: Vec<libp2p::Multiaddr>,
    /// 配置的中继节点列表 [(PeerId, Multiaddr)]
    pub(crate) relay_nodes: Vec<(String, String)>,
    /// 配置的 bootstrap 节点列表 [(PeerId, Multiaddr)]
    pub(crate) bootstrap_nodes: Vec<(String, String)>,
    /// 每个联系人的最近发现时间（用于防重复发现冷却）
    pub(crate) last_discovery_time: HashMap<String, Instant>,
    /// DHT bootstrap 是否已完成（就绪前最多缓存一条身份发布命令）
    pub(crate) bootstrap_ready: bool,
    /// 等待 bootstrap 完成后发布的身份公钥（新发布覆盖旧缓存，不重复累积）
    pub(crate) pending_publish_pubkey: Option<String>,
    /// 启动时的"自己设备查询"标记：(DHT key, 本机 PeerID)。
    /// 非 None 表示正在确认同账号设备在线状态，GetProvidersResult 命中该 key 时
    /// 据 providers 决定是否跳过联系人全量发现。仅启动时短暂存在。
    pub(crate) pending_own_device_query: Option<(String, PeerId)>,
    /// 多设备转发的已见消息键集合（`source_device:seq`）。
    /// 转发消息密文与原消息不同（message_hash 不同），按 `(source_device, seq)`
    /// 去重；满上限清空（偶发重收由内容 hash 兜底，可容忍）。
    pub(crate) device_seen_seqs: std::collections::HashSet<String>,
    /// 多设备同步单调序号（本机作为转发源/真源时生成 routing.seq 与 origin_hash）。
    /// 每次本机生成一条新的设备同步消息 +1，保证 (source_device, seq) 账号内唯一。
    pub(crate) device_sync_seq: u64,
    /// 已通过与当前身份联系人校验的公钥 → 校验时间（短 TTL 缓存）。
    /// 文件分片按 256KB 到达，每片都要判定"发送者是否为联系人"；
    /// 联系人集合极少变化，用缓存避免每分片一次 SQLite COUNT 查询。
    pub(crate) contact_verified_cache: std::collections::HashMap<String, std::time::Instant>,
}

/// 联系人鉴权缓存的 TTL：一次文件传输（分片流）期间有效即可
const CONTACT_VERIFIED_TTL: std::time::Duration = std::time::Duration::from_secs(120);
/// 联系人鉴权缓存容量上限（防膨胀）
const CONTACT_VERIFIED_MAX: usize = 1024;

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
            active_file_sends: Arc::new(std::sync::Mutex::new(std::collections::HashSet::new())),
            file_transfer,
            peer_cache,
            connected_peers: std::collections::HashMap::new(),
            last_known_blocked: std::collections::HashMap::new(),
            inbound_msg_windows: std::collections::HashMap::new(),
            inbound_chunk_windows: std::collections::HashMap::new(),
            inbound_mismatch_windows: std::collections::HashMap::new(),
            mlkem_decap_key: Some(mlkem_decap_key),
            own_listen_addrs: Vec::new(),
            relay_nodes: cfg.relay_nodes.clone(),
            bootstrap_nodes: cfg.bootstrap_nodes.clone(),
            last_discovery_time: HashMap::new(),
            bootstrap_ready: false,
            pending_publish_pubkey: None,
            pending_own_device_query: None,
            device_seen_seqs: std::collections::HashSet::new(),
            device_sync_seq: 0,
            contact_verified_cache: HashMap::new(),
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
    ///
    /// 不能 send().await 阻塞：本方法可能在事件循环内被调用（如回传送达回执），
    /// 若阻塞在命令通道而 P2pActor 又阻塞在向事件循环投递事件上，两个有界通道
    /// 同时占满即死锁（同 event_loop.rs 文件分片 Ack 的解法）。try_send + 短暂重试，
    /// 失败时丢弃：出站消息已存 pending 表，后续定时重试会补发。
    pub(crate) async fn send_message(
        &mut self,
        peerid: PeerId,
        message: ChatMessage,
        message_hash: Option<&str>,
    ) {
        let mut cmd = P2pCommand::SendMessage {
            peer_id: peerid,
            message,
            message_hash: message_hash.unwrap_or("").to_string(),
        };
        for attempt in 0..3 {
            match self.p2p_handle.tx.try_send(cmd) {
                Ok(()) => return,
                Err(mpsc::error::TrySendError::Full(c)) => {
                    cmd = c;
                    tokio::task::yield_now().await;
                    if attempt == 2 {
                        tracing::warn!(
                            "发送消息失败：P2pActor 命令通道持续满载，丢弃（pending 表将在后续定时重试补发）"
                        );
                    }
                }
                Err(mpsc::error::TrySendError::Closed(c)) => {
                    tracing::warn!("发送消息失败：P2pActor 命令通道已关闭");
                    drop(c);
                    return;
                }
            }
        }
    }

    /// 构建签名消息（加密后数据 + ML-DSA 签名）
    /// 构建签名消息（加密数据 + ML-DSA 签名 + 时间戳/nonce）
    pub(crate) async fn build_signed_message(
        &self,
        msgtype: ChatMessageType,
        data: Vec<u8>,
    ) -> CoreResult<ChatMessage> {
        self.build_signed_message_with_routing(msgtype, data, None)
            .await
    }

    /// 构建签名消息，可附加多设备路由元数据（发送侧逐设备扇出用）。
    ///
    /// 多设备场景下各设备副本用各自 ML-KEM 加密（密文/hash 不同），
    /// 但携带**同一 origin_hash**（去重键稳定）、同一发送方签名（身份不变）。
    pub(crate) async fn build_signed_message_with_routing(
        &self,
        msgtype: ChatMessageType,
        data: Vec<u8>,
        routing: Option<crate::DeviceRouting>,
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
        // 附带当前会话 ML-KEM 公钥指纹（32B）：接收方据此检测我方密钥是否轮换。
        // 指纹纳入签名哈希，篡改即验签失败。
        let mlkem_fingerprint = self
            .mlkem_pubkey_hex
            .as_deref()
            .map_or_else(Vec::new, |hex| {
                sha2::Sha256::digest(hex.as_bytes()).to_vec()
            });
        ChatMessage::new_signed(
            msgtype,
            processed_data,
            mldsa_private_key,
            &mldsa_public_key,
            mlkem_fingerprint,
            routing,
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
        let is_new = self.peer_cache.cache_pubkey(peer_id, pubkey_hex.clone());
        if is_new && self.connected_peers.contains_key(&peer_id) {
            self.send_online_status().await;
        }
        // 该联系人已连上（收到其消息），无需继续定时发现打扰：重置冷却
        self.last_discovery_time.remove(&pubkey_hex);
    }

    /// 屏蔽状态查询（带最近已知结果缓存，fail-closed）。
    ///
    /// 查询成功 → 更新缓存并返回真值；查询失败 → 退回缓存值；
    /// 无缓存且失败 → 记录告警并按已屏蔽处理，防止 DB 短暂故障把被屏蔽
    /// 联系人放行。缓存有容量上限（满时清空，联系人数目远小于上限）。
    pub(crate) async fn is_contact_blocked_cached(&mut self, owner: &str, pubkey: &str) -> bool {
        if let Some(pool) = storage::pool() {
            match storage::is_contact_blocked(pool, owner, pubkey).await {
                Ok(v) => {
                    self.last_known_blocked.insert(pubkey.to_string(), v);
                    if self.last_known_blocked.len() > 2048 {
                        self.last_known_blocked.clear();
                    }
                    return v;
                }
                Err(e) => {
                    tracing::warn!(
                        "屏蔽状态查询失败 ({e}), 使用缓存: {}..",
                        &pubkey[..16.min(pubkey.len())]
                    );
                }
            }
        }
        match self.last_known_blocked.get(pubkey) {
            Some(v) => *v,
            None => {
                tracing::warn!(
                    "屏蔽状态无缓存且查询失败，fail-closed 拦截: {}..",
                    &pubkey[..16.min(pubkey.len())]
                );
                true
            }
        }
    }

    /// 联系人屏蔽状态变更时失效缓存，避免陈旧结果误判
    pub(crate) fn invalidate_blocked_cache(&mut self, pubkey_hex: &str) {
        self.last_known_blocked.remove(pubkey_hex);
    }

    /// 每 peer 入站消息滑窗限流：返回 `true` 表示超限应拒绝该消息。
    ///
    /// 防止已连接对端（含联系人）以高频消息持续冲刷 ML-DSA 验签与
    /// SQLite 写入。窗口为 `INBOUND_MSG_WINDOW`，每窗口上限
    /// `INBOUND_MSG_MAX_PER_WINDOW`；窗口表满时剔除过期项防膨胀。
    pub(crate) fn inbound_message_rate_limited(&mut self, peer: &PeerId) -> bool {
        let now = Instant::now();
        let mut over = false;
        match self.inbound_msg_windows.get_mut(peer) {
            Some(entry) => {
                if now.duration_since(entry.0) >= INBOUND_MSG_WINDOW {
                    // 窗口过期：重置计数并退出首窗突发期
                    *entry = (now, 1, false);
                } else {
                    entry.1 += 1;
                    let cap = if entry.2 {
                        INBOUND_MSG_BURST
                    } else {
                        INBOUND_MSG_MAX_PER_WINDOW
                    };
                    if entry.1 > cap {
                        over = true;
                    }
                }
            }
            None => {
                // 首个窗口：突发放宽，容纳离线消息积压
                self.inbound_msg_windows.insert(*peer, (now, 1, true));
            }
        }
        if self.inbound_msg_windows.len() > 4096 {
            let cutoff = now - INBOUND_MSG_WINDOW;
            self.inbound_msg_windows.retain(|_, (t, _, _)| *t >= cutoff);
        }
        over
    }

    /// 每 peer 入站文件分片滑窗限流：返回 `true` 表示超限应拒绝该分片。
    ///
    /// 防止已连接对端（含联系人）以高频文件分片持续冲刷 ML-DSA 验签、ML-KEM
    /// 解封与磁盘写入。窗口独立于消息窗口：分片受 rr_file 请求-响应 Ack 节流，
    /// 合法传输（含 100MB 大文件突发）远低于上限，超上限才拒绝。
    pub(crate) fn inbound_file_chunk_rate_limited(&mut self, peer: &PeerId) -> bool {
        let now = Instant::now();
        let mut over = false;
        match self.inbound_chunk_windows.get_mut(peer) {
            Some(entry) => {
                if now.duration_since(entry.0) >= INBOUND_CHUNK_WINDOW {
                    *entry = (now, 1, false);
                } else {
                    entry.1 += 1;
                    if entry.1 > INBOUND_CHUNK_MAX_PER_WINDOW {
                        over = true;
                    }
                }
            }
            None => {
                self.inbound_chunk_windows.insert(*peer, (now, 1, false));
            }
        }
        if self.inbound_chunk_windows.len() > 4096 {
            let cutoff = now - INBOUND_CHUNK_WINDOW;
            self.inbound_chunk_windows.retain(|_, (t, _, _)| *t >= cutoff);
        }
        over
    }

    /// 每账号（发送方 ML-DSA 公钥）入站加密失配（指纹未命中 / 解密失败）滑窗限流：
    /// 返回 `true` 表示超限应**停止回发 FriendOnline**（持续异常视为骚扰/DoS）。
    ///
    /// 按账号（pubkey）计数而非 PeerID：多设备下攻击者控制目标账号一台设备即可
    /// 生成多个 PeerID 绕过 per-peer 限流；账号级计数使换 PeerID 无效。
    /// 与消息/分片限流独立：正常重启/密钥轮换后失配仅发生一两次，触发 FriendOnline
    /// 交换后自愈；攻击者用错误公钥持续加密轰炸则超限停止回发。超限仅影响密钥交换
    /// 回发，**不阻断连接建立/拨号**，因此不会成为阻止我方连接目标的攻击面。
    pub(crate) fn inbound_mismatch_rate_limited(&mut self, pubkey_hex: &str) -> bool {
        let now = Instant::now();
        let mut over = false;
        match self.inbound_mismatch_windows.get_mut(pubkey_hex) {
            Some(entry) => {
                if now.duration_since(entry.0) >= INBOUND_MISMATCH_WINDOW {
                    *entry = (now, 1);
                } else {
                    entry.1 += 1;
                    if entry.1 > INBOUND_MISMATCH_MAX_PER_WINDOW {
                        over = true;
                    }
                }
            }
            None => {
                self.inbound_mismatch_windows
                    .insert(pubkey_hex.to_string(), (now, 1));
            }
        }
        if self.inbound_mismatch_windows.len() > 4096 {
            let cutoff = now - INBOUND_MISMATCH_WINDOW;
            self.inbound_mismatch_windows.retain(|_, (t, _)| *t >= cutoff);
        }
        over
    }
}

impl Drop for ChatCore {
    fn drop(&mut self) {
        self.core_handle.shutdown_token.cancel();
    }
}
