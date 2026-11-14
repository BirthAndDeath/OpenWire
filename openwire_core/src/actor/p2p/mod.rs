//! P2pActor：P2P 网络事件循环 Actor
//!
//! 将 libp2p Swarm 的事件循环从 ChatCore 中分离出来，独立运行在 P2pActor 中。
//! ChatCore 通过 P2pActorHandle 与 P2pActor 通信。
//!
//! # 架构
//! - P2pActor 拥有 Swarm 的所有权，负责处理所有网络事件
//! - ChatCore 不再持有 Swarm，通过命令通道与 P2pActor 交互
//! - P2pActor 将网络事件转换为 P2pEvent 发送给 ChatCore
//!
//! # 关于 Actor trait
//! P2pActor **不**实现 `crate::actor::Actor` trait，因为其事件循环需要
//! `tokio::select!` 同时处理 Swarm 事件流和命令通道，
//! 且 `Shutdown` 命令需要特殊处理（保存路由表、禁用中继服务后 break 循环）。
//! `Actor` trait 适用于只需处理命令的简单 Actor（无外部事件流），
//! 而 P2pActor 是 libp2p 驱动的非典型 Actor，故使用自定义事件循环。

pub mod netevent;
pub mod swarm_ops;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::LazyLock;

use futures::StreamExt;
use rand::seq::IndexedRandom;
use libp2p::kad::{self, GetRecordOk, Mode, QueryResult};
use libp2p::multiaddr::Protocol;
use libp2p::request_response::{
    Event as RequestResponseEvent, Message as RequestResponseMessage,
};
use libp2p::request_response::OutboundRequestId;
use libp2p::swarm::SwarmEvent;
use libp2p::{Multiaddr, PeerId, Swarm, autonat, core, dcutr, identify, mdns, relay};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::p2p::behaviour::{MyBehaviour, MyBehaviourEvent};
use crate::p2p::{self, dht_cache::DhtCache};
use crate::p2p::netevent::{NetEventRequest, NetEventResponse};
use crate::{ChatMessage, ChatResponse};


use self::swarm_ops as p2p_swarm_ops;

// ============================================================================
// P2pActor 命令和事件
// ============================================================================

use crate::command::{PaidNetworkMode, RelayRole};

/// P2pActor 控制命令
#[derive(Debug)]
pub enum P2pCommand {
    /// 发送消息到网络
    SendMessage {
        /// 目标节点 PeerId
        peer_id: PeerId,
        /// 要发送的消息
        message: ChatMessage,
        /// 消息哈希（用于发送结果回调标记 pending）
        message_hash: String,
    },
    /// 发送 NetEvent 请求
    SendNetEvent {
        /// 目标节点 PeerId
        peer_id: PeerId,
        /// 要发送的 NetEvent 请求
        request: NetEventRequest,
    },
    /// 发送 NetEvent 响应
    SendNetEventResponse {
        /// 用于回送响应的请求-响应通道
        channel: libp2p::request_response::ResponseChannel<NetEventResponse>,
        /// 要发送的 NetEvent 响应
        response: NetEventResponse,
    },
    /// 发布身份到 DHT（使用 SHA256(公钥) 作为 key）
    PublishIdentity {
        /// ML-DSA 公钥 hex
        mldsa_pubkey_hex: String,
    },
    /// 停止在 DHT 提供身份（删除 identity 时调用，替代原本的重新发布）
    StopProviding {
        /// ML-DSA 公钥 hex
        mldsa_pubkey_hex: String,
    },
    /// 发起 GetProviders 查询
    GetProviders {
        /// DHT 查询键
        key: String,
    },
    /// 添加地址到 Kademlia 路由表
    AddKademliaAddress {
        /// 目标节点 PeerId
        peer_id: PeerId,
        /// 要添加的地址
        addr: libp2p::Multiaddr,
    },
    /// 拨号
    Dial {
        /// 要拨号的目标节点 PeerId
        peer_id: PeerId,
    },
    /// 拨号到地址
    DialAddr {
        /// 要拨号的地址
        addr: libp2p::Multiaddr,
    },
    /// 发送 rr_msg 响应确认
    SendResponse {
        /// 用于回送响应的请求-响应通道
        channel: libp2p::request_response::ResponseChannel<ChatResponse>,
        /// 要发送的聊天响应
        response: ChatResponse,
    },
    /// 回送 rr_file 响应（分片接收确认，由事件循环在验证后决定 true/false）
    SendFileChunkResponse {
        /// 用于回送响应的请求-响应通道
        channel: libp2p::request_response::ResponseChannel<bool>,
        /// 是否接受该分片
        accepted: bool,
    },
    /// 保存路由表
    SaveRoutingTable,
    /// 随机刷新路由表（随机桶查询，扩展路由表覆盖）
    RefreshRoutingTable,
    /// 应用从后台回到前台：重建网络连接层
    ///
    /// 移动端后台切换会静默断开 TCP/中继连接且无 ConnectionClosed 通知：
    /// - 重拨中继节点（恢复 NAT 穿透与 DiscoverPeer 可达性）；
    /// - 重跑 Kademlia bootstrap（路由表可能已陈旧）；
    /// - 补发暂存的 DiscoverPeer 查询。
    OnForeground,
    /// 通过中继发现对端 PeerID
    DiscoverPeer {
        /// 目标 ML-DSA 公钥 hex
        mldsa_pubkey_hex: String,
    },
    /// 关闭
    Shutdown,
    /// 设置计费网络检测模式
    SetPaidNetworkMode {
        /// 检测模式
        mode: PaidNetworkMode,
    },
    /// 查询网络状态（用于前端网络监控组件）
    GetNetworkStatus {
        /// 响应通道：返回 JSON 序列化的 NetworkStatusData
        resp: tokio::sync::oneshot::Sender<String>,
    },
    /// 导出当前路由表（用于分享给其他节点）
    ExportRoutingTable {
        /// 响应通道：返回 JSON 序列化的 RoutingTableExport
        resp: tokio::sync::oneshot::Sender<String>,
    },
    /// 导入路由表（将其他节点导出的 peers 加入本地路由表）
    ImportRoutingTable {
        /// 导出的路由表 JSON 字符串
        data: String,
        /// 响应通道：返回 JSON 序列化的导入结果 { imported, error }
        resp: tokio::sync::oneshot::Sender<String>,
    },
    /// 设置中继角色（互斥，server 与 client 不能同时启用）
    SetRelayRole {
        /// 角色
        role: RelayRole,
    },
    /// 发送文件流分片（通过 /chat/rr_file 协议，4MB 上限）
    SendFileChunk {
        /// 目标节点 PeerId
        peer_id: PeerId,
        /// 加密签名的文件流分片
        chunk: crate::message::EncryptedFileChunk,
        /// 发送结果回执：true=对端确认接收成功，false=失败
        /// （rr_file 的 Response / OutboundFailure 驱动）
        ack: tokio::sync::oneshot::Sender<bool>,
    },
    /// 通知 ChatCore：后台文件发送任务失败，需向下载方回发拒绝响应
    ///
    /// 由 send_file_chunks_background 在整条传输中止时发出，经 P2pActor
    /// 转发为 P2pEvent::FileDownloadReject，ChatCore 据此调用
    /// send_reject_response 让下载方立即 cancel（而非等 30 分钟超时）。
    NotifyFileDownloadReject {
        /// 下载方 ML-DSA 公钥 hex（= 原始 DownloadRequest 的发送者）
        pubkey_hex: String,
        /// 被拒绝的文件哈希
        file_hash: [u8; 32],
        /// 拒绝原因
        reason: String,
    },
}

/// P2pActor 发出的事件
#[derive(Debug)]
pub enum P2pEvent {
    /// 收到消息
    MessageReceived {
        /// 发送方 PeerId
        peer: PeerId,
        /// 收到的消息
        message: ChatMessage,
        /// 用于回送响应的请求-响应通道
        channel: libp2p::request_response::ResponseChannel<ChatResponse>,
    },
    /// 收到 NetEvent 请求
    NetEventRequestReceived {
        /// 发送方 PeerId
        peer: PeerId,
        /// 收到的 NetEvent 请求
        request: NetEventRequest,
        /// 用于回送响应的请求-响应通道
        channel: libp2p::request_response::ResponseChannel<NetEventResponse>,
    },
    /// 连接建立
    ConnectionEstablished {
        /// 已建立连接的节点 PeerId
        peer_id: PeerId,
        /// 本节点的监听地址列表
        listen_addrs: Vec<libp2p::Multiaddr>,
    },
    /// 连接关闭
    ConnectionClosed {
        /// 已关闭连接的节点 PeerId
        peer_id: PeerId,
    },
    /// mDNS 发现
    MdnsDiscovered {
        /// 发现的节点 PeerId
        peer_id: PeerId,
        /// 发现的地址
        addr: libp2p::Multiaddr,
    },
    /// mDNS 过期
    MdnsExpired {
        /// 过期的节点 PeerId
        peer_id: PeerId,
    },
    /// Identify 事件
    IdentifyReceived {
        /// 对端节点 PeerId
        peer_id: PeerId,
        /// 对端节点的监听地址列表
        listen_addrs: Vec<libp2p::Multiaddr>,
    },
    /// Kademlia GetProviders 结果
    GetProvidersResult {
        /// DHT 查询键
        key: String,
        /// 发现的 provider 节点列表
        providers: Vec<PeerId>,
    },
    /// Kademlia GetRecord 结果
    GetRecordResult {
        /// DHT 查询键
        key: String,
        /// 查询到的记录值
        value: Vec<u8>,
    },
    /// DHT 发布记录失败
    DhtPublishFailed {
        /// 发布失败的错误信息
        error: String,
    },
    /// 日志
    Log(String),
    /// DHT 网络就绪：Kademlia 路由表已添加至少一个节点，可执行 DHT 查询
    BootstrapReady,
    /// 中继返回的节点信息（DiscoverPeer 响应）
    PeerInfoReceived {
        /// 查询的 ML-DSA 公钥 hex
        mldsa_pubkey_hex: String,
        /// 发现的 PeerID（Base58）
        peer_id: PeerId,
        /// 发现的 ML-KEM 公钥 hex
        mlkem_pubkey_hex: String,
    },
    /// 中继返回的节点未找到（对方尚未添加本节点为联系人）
    PeerNotFound {
        /// 查询的 ML-DSA 公钥 hex
        mldsa_pubkey_hex: String,
    },
    /// 发出的 FriendOnline 被对方拒绝（含结构化原因）
    FriendOnlineNack {
        /// 拒绝本节点 FriendOnline 的对端 PeerID
        peer: PeerId,
        /// 拒绝原因
        reason: crate::p2p::netevent::NackReason,
    },
    /// 消息发送成功（P2P 层确认收到响应）
    MessageSent {
        /// 目标节点 PeerId
        peer: PeerId,
        /// 消息哈希
        message_hash: String,
    },
    /// 消息发送失败（P2P 层 OutboundFailure）
    MessageSendFailed {
        /// 目标节点 PeerId
        peer: PeerId,
        /// 消息哈希
        message_hash: String,
    },
    /// 后台文件发送任务中止，向下载方回发拒绝响应
    FileDownloadReject {
        /// 下载方 ML-DSA 公钥 hex（= 原始 DownloadRequest 的发送者）
        pubkey_hex: String,
        /// 被拒绝的文件哈希
        file_hash: [u8; 32],
        /// 拒绝原因
        reason: String,
    },
    /// 收到文件流分片（通过 /chat/rr_file 协议）
    IncomingFileChunk {
        /// 发送方 PeerId
        peer: PeerId,
        /// 加密签名的文件流分片
        chunk: crate::message::EncryptedFileChunk,
        /// 用于回送协议的请求-响应通道
        channel: libp2p::request_response::ResponseChannel<bool>,
    },
}

// ============================================================================
// AutoRelay 常量
// ============================================================================

/// Circuit Relay v2 hop protocol ID — 中继服务器必须支持的协议
static RELAY_HOP_PROTOCOL: LazyLock<libp2p::StreamProtocol> =
    LazyLock::new(|| libp2p::StreamProtocol::new("/libp2p/circuit/relay/0.2.0/hop"));

/// 监听地址列表最大数量
const MAX_LISTEN_ADDRS: usize = 128;

/// Relay 候选节点最大数量
const MAX_RELAY_CANDIDATES: usize = 32;

/// 备选节点表（LRU）容量上限：满时淘汰最久未连接成功的节点
const MAX_BACKUP_PEERS: usize = 512;
/// 备选节点串行探活间隔（秒）：每轮只探活 1 个不在线节点
const BACKUP_PROBE_INTERVAL_SECS: u64 = 15;

/// DHT 中继节点发现 key — 所有中继节点在此 key 下发布 provider
pub(crate) const DHT_RELAY_INDEX_KEY: &str = "relay_nodes_public";

// ============================================================================
// P2pActor 结构体
// ============================================================================

/// P2P 网络 Actor
///
/// 拥有 Swarm 的所有权，独立运行事件循环。
pub struct P2pActor {
    /// libp2p 网络 swarm
    swarm: Swarm<MyBehaviour>,
    /// DHT 缓存
    dht_cache: Arc<DhtCache>,
    /// 事件发送通道（向 ChatCore 发送事件）
    event_tx: mpsc::Sender<P2pEvent>,
    /// 数据目录路径
    data_dir: std::path::PathBuf,
    // mDNS 缓存由 LruCache 自动管理，无需刷新间隔
    /// mDNS 缓存
    mdns_cache: lru::LruCache<PeerId, std::time::Instant>,
    /// 通过 relay 连接的 peers
    relay_connections: HashSet<PeerId>,
    // Kademlia 模式由 set_mode(Some(...)) 直接管理，无需本地跟踪
    /// AutoNAT 状态
    nat_status: autonat::NatStatus,
    /// 本节点所有监听地址（含直连地址和 /p2p-circuit 中继地址）
    listen_addrs: HashSet<libp2p::Multiaddr>,
    /// Relay 节点配置 [(PeerId, Multiaddr)] —— 地址已归一化为纯传输地址（无 P2p 组件）
    relay_nodes: Vec<(PeerId, Multiaddr)>,
    /// Bootstrap 节点配置 [(PeerId, Multiaddr)]
    bootstrap_nodes: Vec<(PeerId, Multiaddr)>,
    /// 当前所有已连接的 peer（含 relay 和直连），每个 peer 的引用计数
    connected_peers: HashMap<PeerId, usize>,
    /// 通过 Identify 自动发现的 relay 候选节点 (PeerId, addr, added_at)
    relay_candidates: Vec<(PeerId, libp2p::Multiaddr, std::time::Instant)>,
    /// Relay 重连冷却时间（防止无限循环重试）
    relay_reconnect_cooldown_until: Option<std::time::Instant>,
    /// Relay 重连尝试次数（指数退避）
    relay_reconnect_attempt: u32,
    /// 中继服务端是否已启用
    relay_server_enabled: bool,
    /// 是否允许启用中继服务（前端计费网络检测后设置）
    relay_server_allowed: bool,
    /// 用户是否手动设置过中继角色（用于区分自动迁移与显式选择）
    relay_role_user_configured: bool,
    /// 当前网络是否为计费（付费/移动热点）网络
    /// 为 true 时禁止启用中继服务，避免产生额外流量费用
    paid_network: bool,
    /// 计费网络检测模式："free" / "paid" / "disabled"
    paid_network_mode: PaidNetworkMode,
    /// 已发起 reservation 请求的 relay（防重复请求同一节点）
    reservation_attempted: HashSet<PeerId>,
    /// 已成功建立 reservation（对端 ReservationReqAccepted）的 relay
    ///
    /// 与 `reservation_attempted`（"已请求过"）分开：候选数量上限按
    /// 成功数计，失败的尝试不占配额，避免被拒/不可达的中继耗尽槽位。
    reservation_established: HashSet<PeerId>,
    /// Relay DHT 查询冷却时间（防止频繁 get_providers）
    relay_dht_query_cooldown_until: Option<std::time::Instant>,
    /// 无连接中继时暂存的 DiscoverPeer 查询（中继接通后立即补发）
    pending_discover_peers: Vec<String>,

    // ====== PathRanker 协议状态 ======
    // 注意：当前仅处理入站评分查询（ScoreRequest），无出站查询。
    // 三个字段（pending_queries/pending_targets/recent_nonces）已移除，
    // 它们仅在出站查询的 ScoreResponse 处理路径中需要，但 send_query() 从未被调用。
    // 若将来实现出站查询，需重新添加这些字段。

    // ====== DHT 并发控制 ======
    /// DHT 查询信号量，限制并发 GetProviders 请求数
    dht_semaphore: Arc<tokio::sync::Semaphore>,

    /// 是否已发送 BootstrapReady 事件
    bootstrap_ready_sent: bool,

    /// 追踪待发送请求的 request_id → message_hash 映射
    pending_requests: HashMap<OutboundRequestId, String>,

    /// 追踪 rr_file 出站分片请求的 request_id → ack 回执
    pending_file_chunks: HashMap<OutboundRequestId, tokio::sync::oneshot::Sender<bool>>,

    /// UPnP 检测结果状态
    upnp_state: String,
    /// 中继角色："server" / "client" / "off"（互斥）
    relay_role: RelayRole,

    // ====== 备选节点表（LRU）=======
    /// 已认识但当前不在线的节点（LRU 缓存，容量上限）。
    ///
    /// 由定时器串行随机探活：每轮只挑 1 个不在线节点拨号。
    /// 探活成功刷新 recency（保留在表内），失败不刷新 recency
    /// （自然沉到 LRU 尾部），容量满时逐步淘汰连接不上的节点。
    backup_peers: lru::LruCache<PeerId, BackupPeerInfo>,
}

/// 备选节点信息
#[derive(Debug, Clone)]
struct BackupPeerInfo {
    /// 最近一次确认在线的时间（成功连接时刷新）
    last_online: Option<std::time::Instant>,
    /// 连续探活失败次数（失败不刷新 recency，用于 LRU 淘汰依据）
    fail_count: u32,
    /// 最近一次探活失败时间（用于 per-peer 失败退避，避免无限重拨死节点）
    last_failed: Option<std::time::Instant>,
    /// 最近一次连接建立时间（用于区分「健康连接后断开」与「探活拨通即断」）
    last_established: Option<std::time::Instant>,
}

/// 备选节点探活失败退避基数（秒）：fail_count 越高，退避越长
const BACKUP_PROBE_BACKOFF_BASE_SECS: u64 = 30;
/// 失败退避封顶（秒）
const BACKUP_PROBE_BACKOFF_MAX_SECS: u64 = 600;
/// 健康连接阈值（秒）：连接维持超过该时长后断开视为健康断开，
/// 否则视为「探活拨通即断」（握手/协商失败），计为探活失败
const BACKUP_HEALTHY_CONN_THRESHOLD_SECS: u64 = 10;

impl P2pActor {
    /// 创建新的 P2pActor
    pub fn new(
        mut swarm: Swarm<MyBehaviour>,
        dht_cache: Arc<DhtCache>,
        data_dir: std::path::PathBuf,
        event_tx: mpsc::Sender<P2pEvent>,
        relay_nodes: Vec<(PeerId, Multiaddr)>,
        bootstrap_nodes: Vec<(PeerId, Multiaddr)>,
    ) -> Self {
        // 默认角色 client：启动时先关闭 relay server behaviour 运行时开关，
        // 由 try_enable_relay_server 在角色为 server 时重新打开（本地 libp2p-relay patch）。
        swarm.behaviour_mut().relay_server.set_server_enabled(false);

        // 加载持久化的中继设置：用户显式选择最高优先级，
        // 检测 API 不可用/disabled 时按持久化身份（默认 client）运行。
        let persisted = crate::actor::p2p::relay_handler::load_relay_settings(&data_dir);
        let relay_role = persisted.parsed_relay_role().unwrap_or(RelayRole::Client);
        let paid_network_mode = persisted
            .parsed_paid_mode()
            .unwrap_or(PaidNetworkMode::Disabled);
        // 持久化中有角色 → 视为用户显式配置过；否则走自动迁移
        let relay_role_user_configured = persisted.relay_role.is_some();

        let mut actor = Self {
            swarm,
            dht_cache,
            event_tx,
            data_dir,
            
            mdns_cache: lru::LruCache::new(std::num::NonZeroUsize::new(2000).unwrap()),
            relay_connections: HashSet::new(),
            connected_peers: HashMap::new(),
            // kademlia_mode 已移除，由 set_mode(Some(...)) 直接管理
            nat_status: autonat::NatStatus::Unknown,
            listen_addrs: HashSet::new(),
            relay_nodes,
            bootstrap_nodes,
            relay_candidates: Vec::new(),
            relay_reconnect_cooldown_until: None,
            relay_reconnect_attempt: 0,
            relay_server_enabled: false,
            relay_server_allowed: false,
            relay_role_user_configured,
            // 计费网络状态由 paid_network_mode 决定：
            // - free  → 非计费，中继服务可启用
            // - paid  → 计费，禁止自动启用中继服务（避免流量费）
            // - disabled/未检测 → 回退到持久化角色（默认 client），不强制按付费处理
            paid_network: paid_network_mode == PaidNetworkMode::Metered,
            paid_network_mode,
            reservation_attempted: HashSet::new(),
            reservation_established: HashSet::new(),
            relay_dht_query_cooldown_until: None,
            pending_discover_peers: Vec::new(),
            dht_semaphore: Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_DHT_QUERIES)),
            bootstrap_ready_sent: false,
            pending_requests: HashMap::new(),
            pending_file_chunks: HashMap::new(),
            upnp_state: "Unknown".to_string(),
            relay_role,
            backup_peers: lru::LruCache::new(std::num::NonZeroUsize::new(MAX_BACKUP_PEERS).unwrap()),
        };

        // 用户显式持久化的 server 角色最高优先级：启动时立即启用中继服务，
        // 不必等待 AutoNAT Public / SetPaidNetworkMode 事件（检测 API 可能不可用）。
        if persisted.relay_role.as_deref() == Some("server") {
            actor.try_enable_relay_server();
        }
        actor
    }

    fn is_relay_addr(addr: &libp2p::multiaddr::Multiaddr) -> bool {
        addr.iter().any(|p| matches!(p, Protocol::P2pCircuit))
    }

    /// 连接断开时记录备选表（LRU）。
    ///
    /// 区分两种断开：
    /// - **拨通即断**：连接维持时间不足 `BACKUP_HEALTHY_CONN_THRESHOLD_SECS`
    ///   （典型为探活拨号成功但协议协商失败/对端立即关闭），视为探活失败，
    ///   仅累加 fail_count 并设置退避，不刷新 last_online——避免死节点被
    ///   「拨通→断开」循环无限复活、永远无法淘汰。
    /// - **健康断开**：连接维持超过阈值，确认节点确实在线过，刷新 last_online、
    ///   清零 fail_count，使其重新获得探活机会。
    ///
    /// relay 节点由 relay 子系统管理，不入表。
    fn record_backup_disconnect(&mut self, peer_id: PeerId) {
        if self.relay_connections.contains(&peer_id)
            || self.relay_nodes.iter().any(|(rp, _)| *rp == peer_id)
        {
            return;
        }
        let now = std::time::Instant::now();
        // 表内已有该 peer：判断是否拨通即断。
        // 用 peek_mut 而非 get_mut：失败语义不刷新 LRU recency（与
        // OutgoingConnectionError 的 peek_mut 一致），避免死节点靠
        // 「拨通→断开」循环持续刷新 recency 而驻留表内，容量压力时也能淘汰。
        let should_evict = {
            let Some(info) = self.backup_peers.peek_mut(&peer_id) else {
                // 表内没有：新建条目（健康断开的普通节点）
                self.backup_peers.push(
                    peer_id,
                    BackupPeerInfo {
                        last_online: Some(now),
                        fail_count: 0,
                        last_failed: None,
                        last_established: None,
                    },
                );
                return;
            };
            let flicker = info
                .last_established
                .is_some_and(|t| now.duration_since(t) < std::time::Duration::from_secs(BACKUP_HEALTHY_CONN_THRESHOLD_SECS));
            if flicker {
                info.fail_count = info.fail_count.saturating_add(1);
                info.last_failed = Some(now);
                tracing::debug!(
                    "Backup peer {peer_id}: probe connect-and-drop (fail_count={})",
                    info.fail_count
                );
            } else {
                info.last_online = Some(now);
                info.fail_count = 0;
                info.last_failed = None;
            }
            let should_evict = info.fail_count >= 10;
            info.last_established = None;
            should_evict
        };
        // 拨通即断与探活失败累计同一淘汰阈值（10 次），
        // 否则「拨通→断开」循环的死节点永远无法淘汰，只依赖 LRU 容量压力。
        if should_evict {
            self.backup_peers.pop(&peer_id);
            tracing::warn!("Backup peer {peer_id} connect-and-dropped 10 times, evicted");
        }
    }

    /// 每轮串行探活一个不在线的备选节点。
    ///
    /// 从备选表中随机挑一个「当前不在线且未处于失败退避期」的 peer 拨号：
    /// - 成功 → ConnectionEstablished 事件中刷新 last_online/last_established
    ///   （条目保留在表内，在线时由 connected_peers 过滤避免重复探活；
    ///   连接维持不足阈值后断开则按「拨通即断」计为探活失败）
    /// - 失败 → OutgoingConnectionError 事件中 fail_count+1 并设置退避
    ///   （不刷新 recency，长期连不上的节点沉底，容量满时被逐步淘汰）
    ///
    /// 返回是否挑中了候选（供定时器日志用）。
    fn probe_one_backup_peer(&mut self) -> bool {
        // 配置的 relay 节点由 relay 子系统管理（含冷却退避），不参与备选表探活
        let config_relays: HashSet<PeerId> =
            self.relay_nodes.iter().map(|(rp, _)| *rp).collect();
        let now = std::time::Instant::now();
        let offline: Vec<PeerId> = self
            .backup_peers
            .iter()
            .filter_map(|(pid, info)| {
                if self.connected_peers.contains_key(pid)
                    || self.relay_connections.contains(pid)
                    || self.relay_candidates.iter().any(|(rp, _, _)| rp == pid)
                    || config_relays.contains(pid)
                {
                    return None;
                }
                // 失败退避：fail_count 次失败 → 退避 30s * 2^fail_count（封顶 600s）
                if let Some(last) = info.last_failed {
                    let backoff = BACKUP_PROBE_BACKOFF_BASE_SECS
                        .saturating_mul(1u64 << info.fail_count.min(4))
                        .min(BACKUP_PROBE_BACKOFF_MAX_SECS);
                    if now.duration_since(last) < std::time::Duration::from_secs(backoff) {
                        return None;
                    }
                }
                Some(*pid)
            })
            .collect();
        if offline.is_empty() {
            return false;
        }
        let Some(pid) = offline.choose(&mut rand::rng()) else {
            return false;
        };
        let pid = *pid;
        tracing::debug!("Backup probe: dialing offline peer {pid}");
        p2p_swarm_ops::dial(&mut self.swarm, &pid);
        true
    }

    /// 登记一个中继候选节点（统一去重 + 上限 + 可选拨号）
    ///
    /// Identify 与 GetProviders 两处登记逻辑共用，避免重复实现漂移：
    /// - 已在 `relay_connections` 或 `relay_candidates` 中则跳过；
    /// - 已达 `MAX_RELAY_CANDIDATES` 上限则跳过；
    /// - 已存在但地址为空（DHT 发现的占位）时补齐真实传输地址；
    /// - `dial` 为 true 时直接拨号（DHT 发现路径），false 仅登记
    ///   （Identify 路径，NAT 后由 dial_relay_nodes 统一拨号）。
    fn register_relay_candidate(&mut self, peer_id: PeerId, addr: libp2p::Multiaddr, dial: bool) {
        if self.relay_connections.contains(&peer_id) {
            return;
        }
        if let Some(entry) = self.relay_candidates.iter_mut().find(|(pid, _, _)| *pid == peer_id) {
            // DHT 发现的 relay 只有 PeerID，Identify 后补齐真实传输地址，
            // 否则 dial_relay_nodes / reservation 都会因空地址跳过它
            if entry.1.iter().next().is_none() && !addr.iter().next().is_none() {
                entry.1 = addr.clone();
                tracing::debug!(
                    "Filled transport address for relay candidate {}: {}",
                    peer_id,
                    addr
                );
            }
            return;
        }
        if self.relay_candidates.len() >= MAX_RELAY_CANDIDATES {
            tracing::debug!("Relay candidate {} rejected: candidate list full", peer_id);
            return;
        }
        self.relay_candidates
            .push((peer_id, addr.clone(), std::time::Instant::now()));
        tracing::debug!(
            "Relay candidate registered: {} at {}",
            peer_id,
            if addr.iter().next().is_none() {
                "(pending address)".to_string()
            } else {
                addr.to_string()
            }
        );
        if dial
            && let Err(e) = self.swarm.dial(peer_id)
        {
            tracing::debug!("DHT relay {} dial deferred/failed: {}", peer_id, e);
        }
        // NAT 后 Identify 登记的候选也立即触发拨号流程
        if !dial
            && matches!(
                self.nat_status,
                autonat::NatStatus::Private | autonat::NatStatus::Unknown
            )
        {
            self.dial_relay_nodes();
        }
    }

    /// 发送事件到 ChatCore
    ///
    /// 关键事件（MessageReceived/GetProvidersResult）使用 send() 确保不丢失；
    /// 非关键事件使用 try_send() 避免背压死锁。
    async fn send_event(&mut self, event: P2pEvent) {
        let is_critical = matches!(
            event,
            P2pEvent::MessageReceived { .. }
                | P2pEvent::GetProvidersResult { .. }
                | P2pEvent::IncomingFileChunk { .. }
        );
        if is_critical {
            // 限时发送：ChatCore 可能暂时不消费（正在处理其他命令）。若在此无限阻塞，
            // P2pActor 会停止处理 swarm/命令事件，与 ChatCore 形成死锁；超时则丢弃并告警。
            match tokio::time::timeout(
                std::time::Duration::from_secs(5),
                self.event_tx.send(event),
            )
            .await
            {
                Ok(Ok(())) => {}
                Ok(Err(e)) => tracing::error!("事件通道关闭，P2pActor 退出: {e:?}"),
                Err(_) => tracing::error!("关键事件发送超时（ChatCore 未消费），已丢弃"),
            }
        } else if let Err(e) = self.event_tx.try_send(event) {
            tracing::warn!("事件通道满，丢弃非关键 P2pEvent: {e:?}");
        }
    }

    /// 处理单个 swarm 事件
    #[tracing::instrument(skip(self, event))]
    async fn handle_swarm_event(&mut self, event: SwarmEvent<MyBehaviourEvent>) {
        match event {
            SwarmEvent::Behaviour(MyBehaviourEvent::Autonat(autonat::Event::StatusChanged {
                old: _,
                new: new_status,
            })) => {
                let old_status = std::mem::replace(&mut self.nat_status, new_status.clone());
                match new_status {
                    autonat::NatStatus::Public(addr) => {
                        tracing::info!("AutoNAT: node is publicly reachable at {:?}", addr);
                        tracing::info!("AutoNAT: switching Kademlia to Server mode (public)");
                        self.swarm
                            .behaviour_mut()
                            .kademlia
                            .set_mode(Some(Mode::Server));
                        // 公网节点不需要中继，断开 relay 连接减轻中继压力
                        self.disconnect_relay_nodes();
                        // 公网节点自动启用中继服务（向 DHT 注册 + listen /p2p-circuit）
                        self.try_enable_relay_server();
                    }
                    autonat::NatStatus::Private => {
                        tracing::warn!(
                            "AutoNAT: node is behind NAT, switching Kademlia to Client mode"
                        );
                        // 不再是公网节点，关闭中继服务
                        self.disable_relay_server();
                        // 重置授权标志，使下次 Public 时能重新进入自动迁移块
                        self.relay_server_allowed = false;
                        // 如果中继角色是自动迁移的（非用户手动设置），回退到 client
                        if !self.relay_role_user_configured && self.relay_role == RelayRole::Server {
                            self.relay_role = RelayRole::Client;
                            tracing::info!("AutoNAT: relay role reverted to 'client' (was auto-migrated)");
                        }
                        tracing::info!("AutoNAT: switching Kademlia to Client mode (NATed)");
                        self.swarm
                            .behaviour_mut()
                            .kademlia
                            .set_mode(Some(Mode::Client));
                        // NAT 后节点需要中继连接
                        self.dial_relay_nodes();
                    }
                    autonat::NatStatus::Unknown => {
                        if old_status != autonat::NatStatus::Unknown {
                            tracing::info!("AutoNAT: status unknown (probing in progress)");
                        }
                        // 状态未知时也尝试连接中继，确保可达性
                        self.dial_relay_nodes();
                    }
                }
            }
            // --- Kademlia 事件 ---
            SwarmEvent::Behaviour(MyBehaviourEvent::Kademlia(kad_event)) => {
                self.handle_kademlia_event(kad_event).await;
            }

            // --- rr_msg: 收到消息请求 ---
            SwarmEvent::Behaviour(MyBehaviourEvent::RrMsg(RequestResponseEvent::Message {
                peer,
                message:
                    RequestResponseMessage::Request {
                        channel, request, ..
                    },
                ..
            })) => {
                self.send_event(P2pEvent::MessageReceived {
                    peer,
                    message: request,
                    channel,
                })
                .await;
            }

            // --- rr_msg: 收到响应 ---
            SwarmEvent::Behaviour(MyBehaviourEvent::RrMsg(RequestResponseEvent::Message {
                message: RequestResponseMessage::Response { response, request_id, .. },
                peer,
                ..
            })) => {
                match response.verify() {
                    Ok(true) => tracing::debug!("收到签名响应: timestamp={}", response.timestamp),
                    Ok(false) => tracing::warn!("收到无效签名的响应，已忽略"),
                    Err(e) => tracing::warn!("验证响应签名时出错: {}", e),
                }
                if let Some(hash) = self.pending_requests.remove(&request_id) {
                    self.send_event(P2pEvent::MessageSent { peer, message_hash: hash }).await;
                }
            }

            // --- rr_msg: 出站失败 ---
            SwarmEvent::Behaviour(MyBehaviourEvent::RrMsg(
                RequestResponseEvent::OutboundFailure { peer, error, request_id, .. },
            )) => {
                tracing::error!("向 {} 发送消息失败: {:?}", peer, error);
                if let Some(hash) = self.pending_requests.remove(&request_id) {
                    self.send_event(P2pEvent::MessageSendFailed { peer, message_hash: hash }).await;
                }
            }

            // --- rr_msg: 入站失败 ---
            SwarmEvent::Behaviour(MyBehaviourEvent::RrMsg(
                RequestResponseEvent::InboundFailure { peer, error, .. },
            )) => {
                tracing::error!("来自 {} 的入站请求失败: {:?}", peer, error);
            }

            // --- rr_msg: 响应已发送 ---
            SwarmEvent::Behaviour(MyBehaviourEvent::RrMsg(
                RequestResponseEvent::ResponseSent { .. },
            )) => {}

            // --- rr_netevent: 收到 NetEvent 请求 ---
            SwarmEvent::Behaviour(MyBehaviourEvent::RrNetevent(
                RequestResponseEvent::Message {
                    peer,
                    message:
                        RequestResponseMessage::Request {
                            channel, request, ..
                        },
                    ..
                },
            )) => {
                self.send_event(P2pEvent::NetEventRequestReceived {
                    peer,
                    request,
                    channel,
                })
                .await;
            }

            // --- rr_netevent: 收到 NetEvent 响应 ---
            SwarmEvent::Behaviour(MyBehaviourEvent::RrNetevent(
                RequestResponseEvent::Message {
                    peer,
                    message: RequestResponseMessage::Response { response, .. },
                    ..
                },
            )) => {
                // DiscoverPeer 响应：转发到 ChatCore 处理
                match &response {
                    NetEventResponse::PeerInfo {
                        mldsa_pubkey_hex,
                        peer_id,
                        mlkem_pubkey_hex,
                    } if !peer_id.is_empty() => {
                        match peer_id.parse::<PeerId>() {
                            Ok(pid) => {
                                self.send_event(P2pEvent::PeerInfoReceived {
                                    mldsa_pubkey_hex: mldsa_pubkey_hex.clone(),
                                    peer_id: pid,
                                    mlkem_pubkey_hex: mlkem_pubkey_hex.clone(),
                                }).await;
                            }
                            Err(e) => {
                                tracing::warn!("PeerInfo invalid peer_id '{peer_id}': {e}");
                            }
                        }
                        return;
                    }
                    NetEventResponse::PeerNotFound { mldsa_pubkey_hex } => {
                        self.send_event(P2pEvent::PeerNotFound {
                            mldsa_pubkey_hex: mldsa_pubkey_hex.clone(),
                        }).await;
                        return;
                    }
                    NetEventResponse::Nack { reason } => {
                        // 同一 rr_netevent 协议承载 FriendOnline 与 DiscoverPeer：
                        // 仅 FriendOnline 专属原因才判定为 FriendOnline 被拒，
                        // 其余（如 DiscoverPeer 查询被拒）只记日志，避免误报。
                        match reason {
                            crate::p2p::netevent::NackReason::PeerIdMismatch
                            | crate::p2p::netevent::NackReason::SignatureVerificationFailed
                            | crate::p2p::netevent::NackReason::VersionMismatch { .. } => {
                                tracing::warn!(
                                    "对方 {} 拒绝了 FriendOnline: {:?}",
                                    peer,
                                    reason
                                );
                                self.send_event(P2pEvent::FriendOnlineNack {
                                    peer,
                                    reason: reason.clone(),
                                })
                                .await;
                            }
                            crate::p2p::netevent::NackReason::Other { description } => {
                                tracing::warn!("NetEvent 请求被 {} 拒绝: {}", peer, description);
                            }
                        }
                        return;
                    }
                    NetEventResponse::Ack => {
                        tracing::debug!("NetEvent 请求被 {} 确认（Ack）", peer);
                        return;
                    }
                    _ => {}
                }
            }
            // --- rr_file: 收到文件流分片请求 ---
            SwarmEvent::Behaviour(MyBehaviourEvent::RrFile(
                RequestResponseEvent::Message {
                    peer,
                    message:
                        RequestResponseMessage::Request {
                            channel, request, ..
                        },
                    ..
                },
            )) => {
                // 不在协议层立即回 Ack：由事件循环完成签名验证、联系人校验并
                // 成功写入后再回 true/false，避免伪造分片被当作已接收。
                self.send_event(P2pEvent::IncomingFileChunk {
                    peer,
                    chunk: request,
                    channel,
                })
                .await;
            }
            // --- rr_file: 收到响应（文件分片确认）---
            SwarmEvent::Behaviour(MyBehaviourEvent::RrFile(
                RequestResponseEvent::Message {
                    message: RequestResponseMessage::Response { response, request_id, .. },
                    ..
                },
            )) => {
                if let Some(ack) = self.pending_file_chunks.remove(&request_id) {
                    let _ = ack.send(response);
                }
            }
            // --- rr_file: 出站/入站失败 ---
            SwarmEvent::Behaviour(MyBehaviourEvent::RrFile(
                RequestResponseEvent::OutboundFailure { peer, error, request_id, .. },
            )) => {
                tracing::warn!("向 {} 发送文件分片失败: {:?}", peer, error);
                if let Some(ack) = self.pending_file_chunks.remove(&request_id) {
                    let _ = ack.send(false);
                }
            }
            SwarmEvent::Behaviour(MyBehaviourEvent::RrFile(
                RequestResponseEvent::InboundFailure { peer, error, .. },
            )) => {
                tracing::warn!("来自 {} 的文件分片入站失败: {:?}", peer, error);
            }
            SwarmEvent::Behaviour(MyBehaviourEvent::RrFile(
                RequestResponseEvent::ResponseSent { .. },
            )) => {}
            SwarmEvent::Behaviour(MyBehaviourEvent::RelayServer(event)) => {
                match event {
                    relay::Event::ReservationReqAccepted { src_peer_id, .. } => {
                        tracing::info!("Relay server: reservation from {}", src_peer_id);
                    }
                    relay::Event::ReservationReqDenied { src_peer_id, .. } => {
                        tracing::warn!("Relay server: reservation denied for {}", src_peer_id);
                    }
                    relay::Event::ReservationClosed { src_peer_id } => {
                        tracing::info!("Relay server: reservation closed for {}", src_peer_id);
                    }
                    relay::Event::ReservationTimedOut { src_peer_id } => {
                        tracing::warn!("Relay server: reservation timed out for {}", src_peer_id);
                    }
                    relay::Event::CircuitReqAccepted { src_peer_id, dst_peer_id, .. } => {
                        tracing::debug!("Relay server: circuit {} -> {}", src_peer_id, dst_peer_id);
                    }
                    relay::Event::CircuitReqDenied { src_peer_id, dst_peer_id, .. } => {
                        tracing::warn!("Relay server: circuit denied {} -> {}", src_peer_id, dst_peer_id);
                    }
                    relay::Event::CircuitClosed { src_peer_id, dst_peer_id, .. } => {
                        tracing::debug!("Relay server: circuit closed {} -> {}", src_peer_id, dst_peer_id);
                    }
                    _ => {}
                }
            }

            // --- rr_netevent: 出站失败 ---
            SwarmEvent::Behaviour(MyBehaviourEvent::RrNetevent(
                RequestResponseEvent::OutboundFailure { peer, error, .. },
            )) => {
                // IPFS 引导节点等非 OpenWire 节点不支持 NetEvent 协议，
                // 这是正常情况，降级为 debug 级别。
                tracing::debug!("向 {} 发送 NetEvent 失败: {:?}", peer, error);
            }

            // --- rr_netevent: 入站失败 ---
            SwarmEvent::Behaviour(MyBehaviourEvent::RrNetevent(
                RequestResponseEvent::InboundFailure { peer, error, .. },
            )) => {
                tracing::error!("来自 {} 的 NetEvent 入站失败: {:?}", peer, error);
            }

            // --- rr_netevent: 响应已发送 ---
            SwarmEvent::Behaviour(MyBehaviourEvent::RrNetevent(
                RequestResponseEvent::ResponseSent { .. },
            )) => {}

            // --- mDNS 发现 ---
            SwarmEvent::Behaviour(MyBehaviourEvent::Mdns(mdns::Event::Discovered(list))) => {
                let now = std::time::Instant::now();
                for (peer_id, multiaddr) in list {
                    if self.mdns_cache.get(&peer_id).is_none() {
                        self.mdns_cache.put(peer_id, now);
                    }
                    self.swarm
                        .behaviour_mut()
                        .kademlia
                        .add_address(&peer_id, multiaddr);
                    tracing::debug!("mDNS discovered: {peer_id}");
                }
            }

            // --- mDNS 过期 ---
            SwarmEvent::Behaviour(MyBehaviourEvent::Mdns(mdns::Event::Expired(list))) => {
                for (peer_id, _multiaddr) in list {
                    tracing::debug!("mDNS expired: {peer_id}");
                    self.mdns_cache.pop(&peer_id);
                }
            }

            // --- Identify 事件 ---
            SwarmEvent::Behaviour(MyBehaviourEvent::Identify(identify::Event::Received {
                peer_id,
                info,
                ..
            })) => {
                for multiaddr in info.listen_addrs.clone() {
                    self.swarm
                        .behaviour_mut()
                        .kademlia
                        .add_address(&peer_id, multiaddr);
                }
                // 注意：不要在此处手动调用 swarm.add_external_address()。
                // Identify 协议会自动将 observed_addr 提交到地址管理，
                // 并触发 NewExternalAddrCandidate 事件供 DCUtR 使用。
                // 手动 add_external_address 会"吃掉"这个事件，
                // 导致 DCUtR 拿不到公网地址，打洞失败报 NoAddress。
                tracing::debug!(
                    "Identified {} with {} protocols",
                    peer_id,
                    info.protocols.len()
                );
                // AutoRelay: 检测 peer 是否支持中继 hop 协议
                if info.protocols.contains(&RELAY_HOP_PROTOCOL) {
                    // 选择第一个非 loopback / 非 circuit 地址，避免拨号到不可达地址
                    let first_usable = info.listen_addrs.iter().find(|a| {
                        let s = a.to_string();
                        !s.contains("127.0.0.1")
                            && !s.contains("::1")
                            && !s.contains("/p2p-circuit")
                    });
                    let Some(first_addr) = first_usable.or_else(|| info.listen_addrs.first()) else {
                        tracing::debug!(
                            "Discovered relay-capable candidate {} but no usable address",
                            peer_id
                        );
                        return;
                    };
                    // 归一化为纯传输地址（剥离 P2p 组件），保证存储不变量
                    let selected_addr = relay_handler::strip_p2p(first_addr);
                    // 统一登记：DHT 发现的空地址候选在此补齐真实传输地址
                    self.register_relay_candidate(peer_id, selected_addr, false);
                    // Identify 完成后，对已连接且支持 HOP 的 relay 请求 circuit reservation
                    // 与官方 DCUtR 示例一致：dial → Identify → listen_on(relay_addr.with(P2pCircuit))
                    // 配置节点优先；候选节点（含 DHT 发现的轻量级 server）在上限内同样请求。
                    // 不依赖当前 libp2p-relay 对 ReservationReqAccepted 事件的具体实现，
                    // 上游修复后本条逻辑自动正确。
                    if !self.reservation_attempted.contains(&peer_id)
                        && self.on_relay_connected(&peer_id) {
                            self.reservation_attempted.insert(peer_id);
                        }
                }
                self.send_event(P2pEvent::IdentifyReceived {
                    peer_id,
                    listen_addrs: info.listen_addrs,
                })
                .await;
            }

            // --- 连接建立 ---
            SwarmEvent::ConnectionEstablished {
                peer_id, endpoint, ..
            } => {
                let is_relay = match &endpoint {
                    core::ConnectedPoint::Dialer { address, .. }
                    | core::ConnectedPoint::Listener {
                        send_back_addr: address,
                        ..
                    } => Self::is_relay_addr(address),
                };
                *self.connected_peers.entry(peer_id).or_insert(0) += 1;
                if is_relay {
                    if self.relay_connections.insert(peer_id) {
                        tracing::debug!("Relay connection to {}", peer_id);
                        // 中继接通：补发暂存的 DiscoverPeer 查询
                        self.flush_pending_discover_peers();
                    }
                } else {
                    tracing::debug!("Direct connection established with {}", peer_id);
                }
                // 记录连接建立时间：供 ConnectionClosed 区分「健康断开」与
                // 「探活拨通即断」（后者应计为探活失败，避免死节点被复活）。
                // 不再 pop 移出表——在线节点由 connected_peers 过滤避免重复探活，
                // 保留条目使失败计数能在探活拨通即断时正确累积直至淘汰。
                if !is_relay
                    && let Some(info) = self.backup_peers.get_mut(&peer_id)
                {
                    info.last_online = Some(std::time::Instant::now());
                    info.last_established = Some(std::time::Instant::now());
                }
                self.send_event(P2pEvent::ConnectionEstablished {
                    peer_id,
                    listen_addrs: self.listen_addrs.iter().cloned().collect(),
                })
                .await;
            }

            // --- 连接关闭 ---
            SwarmEvent::ConnectionClosed { peer_id, .. } => {
                tracing::debug!("Connection closed with {}", peer_id);
                if let std::collections::hash_map::Entry::Occupied(mut e) = self.connected_peers.entry(peer_id) {
                    *e.get_mut() = e.get().saturating_sub(1);
                    if *e.get() == 0 {
                        e.remove();
                    }
                }
                // 先判定是否 relay（record_backup_disconnect 内检查 relay_connections 排除），
                // 再移除 relay 记录，避免排除守卫在 remove 后失效
                let is_relay = self.relay_connections.contains(&peer_id);
                self.relay_connections.remove(&peer_id);
                // 中继断开：立即重拨（受冷却/退避限制），无需等 reservation 过期
                // （ExpiredListenAddr 的分钟级触发）——移动端切后台静默断连后恢复更快。
                if is_relay {
                    let now = std::time::Instant::now();
                    let in_cooldown = self
                        .relay_reconnect_cooldown_until
                        .is_some_and(|c| now < c);
                    if !in_cooldown {
                        self.dial_relay_nodes();
                    } else {
                        tracing::debug!(
                            "Relay {peer_id} 断开，处于重连冷却期，跳过立即重拨"
                        );
                    }
                }
                // 断开连接 → 入备选表（LRU），供串行探活与逐步淘汰；
                // 区分「健康断开」与「探活拨通即断」（后者计失败，防死节点复活）
                if !is_relay {
                    self.record_backup_disconnect(peer_id);
                }
                self.send_event(P2pEvent::ConnectionClosed { peer_id })
                    .await;
            }

            // --- 其他事件（日志/忽略）---
            SwarmEvent::NewListenAddr { address, .. } => {
                if self.listen_addrs.len() < MAX_LISTEN_ADDRS {
                    self.listen_addrs.insert(address.clone());
                }
                if Self::is_relay_addr(&address) {
                    // libp2p v0.56.0 workaround: ReservationReqAccepted 事件可能被 Swarm 丢弃，
                    // 但 NewListenAddr 在 listen_on(relay_addr/p2p-circuit) 成功时必然触发，
                    // 用此作为 reservation 成功的间接确认信号，清除退避状态。
                    // 从 circuit 地址中提取 relay peer ID 用于日志（地址格式: /.../p2p/<relay_pid>/p2p-circuit）
                    let relay_pid: Option<PeerId> = address.iter().find_map(|p| {
                        if let Protocol::P2p(pid) = p { Some(pid) } else { None }
                    });
                    if let Some(pid) = relay_pid {
                        tracing::info!("=== RELAY READY: reservation accepted by relay {pid} ===");
                    } else {
                        tracing::info!("=== RELAY ADDR: {address} ===");
                    }
                    self.relay_reconnect_cooldown_until = None;
                    self.relay_reconnect_attempt = 0;
                } else {
                    tracing::debug!("Listening on: {address}");
                }
            }
            SwarmEvent::OutgoingConnectionError { peer_id, error, .. } => match peer_id {
                Some(pid) => {
                    tracing::error!("Connect failed to {pid}: {error:?}");
                    // 探活失败：fail_count+1 并记录失败时间（用于退避），
                    // 不刷新 recency（peek_mut 不更新 LRU 位置），
                    // 使长期连不上的节点逐渐沉到 LRU 尾部，容量满时被逐步淘汰
                    let evict = if let Some(info) = self.backup_peers.peek_mut(&pid) {
                        info.fail_count = info.fail_count.saturating_add(1);
                        info.last_failed = Some(std::time::Instant::now());
                        tracing::debug!(
                            "Backup peer {pid} probe failed (fail_count={})",
                            info.fail_count
                        );
                        info.fail_count >= 10
                    } else {
                        false
                    };
                    if evict {
                        self.backup_peers.pop(&pid);
                        tracing::warn!("Backup peer {pid} failed 10 times, evicted");
                    }
                }
                None => tracing::debug!("Outgoing connection error (no peer id): {error:?}"),
            },
            SwarmEvent::IncomingConnectionError {
                local_addr, error, ..
            } => {
                tracing::error!("Incoming error on {local_addr:?}: {error:?}");
            }
            SwarmEvent::Dialing { peer_id, .. } => {
                tracing::debug!("Dialing: {peer_id:?}");
            }
            SwarmEvent::ListenerClosed {
                addresses, reason, ..
            } => {
                tracing::warn!("Listener closed: {addresses:?}, reason: {reason:?}");
            }
            SwarmEvent::ListenerError { error, .. } => {
                tracing::error!("Listener error: {error}");
            }
            SwarmEvent::NewExternalAddrOfPeer { peer_id, address } => {
                tracing::debug!("Peer {peer_id} new addr: {address}");
            }
            SwarmEvent::ExpiredListenAddr { address, .. } => {
                self.listen_addrs.remove(&address);
                if Self::is_relay_addr(&address) {
                    tracing::info!("Relay reservation expired, removed address: {address}");
                    // 清除 reservation_attempted，允许 reservation 过期后重新发起
                    self.reservation_attempted.clear();
                    // 已成功的 reservation 也随地址过期而失效
                    self.reservation_established.clear();
                    // re-reservation attempt: reset and re-dial
                    // 检查冷却时间，防止绕过退避
                    let now = std::time::Instant::now();
                    let in_cooldown = self.relay_reconnect_cooldown_until.is_some_and(|c| now < c);
                    if !in_cooldown
                        && matches!(
                            self.nat_status,
                            autonat::NatStatus::Private | autonat::NatStatus::Unknown
                        )
                    {
                        self.dial_relay_nodes();
                    } else if in_cooldown {
                        tracing::debug!("ExpiredListenAddr: in cooldown, skipping relay re-dial");
                    }
                } else {
                    tracing::debug!("Listen address expired: {address}");
                }
            }

            // --- Relay Client 事件 ---
            SwarmEvent::Behaviour(MyBehaviourEvent::RelayClient(
                relay::client::Event::ReservationReqAccepted { relay_peer_id, .. },
            )) => {
                tracing::info!("=== RELAY READY: reservation accepted by relay {} ===", relay_peer_id);
                self.relay_reconnect_cooldown_until = None;
                self.relay_reconnect_attempt = 0;
                // 记录真正成功的 reservation：候选配额按成功数计
                self.reservation_established.insert(relay_peer_id);
            }
            SwarmEvent::Behaviour(MyBehaviourEvent::RelayClient(
                relay::client::Event::OutboundCircuitEstablished { relay_peer_id, .. },
            )) => {
                tracing::debug!("Outbound circuit established via relay: {}", relay_peer_id);
                if self.relay_connections.insert(relay_peer_id) {
                    tracing::debug!(
                        "Relay connection to {}, DCUtR auto-handled by libp2p",
                        relay_peer_id
                    );
                    // 中继接通：补发暂存的 DiscoverPeer 查询
                    self.flush_pending_discover_peers();
                }
            }
            // client::Event 变体: ReservationReqAccepted, OutboundCircuitEstablished, InboundCircuitEstablished
            // 无 ReservationReqRejected/TimedOut（libp2p v0.56），catch-all 处理其他变体
            SwarmEvent::Behaviour(MyBehaviourEvent::RelayClient(event)) => {
                tracing::debug!("=== RELAY EVENT: {:?} ===", event);
            }

            // --- DCUtR 事件 ---
            SwarmEvent::Behaviour(MyBehaviourEvent::Dcutr(dcutr::Event {
                remote_peer_id,
                result: Ok(_),
            })) => {
                tracing::info!("DCUtR direct connection upgraded with: {}", remote_peer_id);
                // 移除 relay connection 标记，直连已建立
                self.relay_connections.remove(&remote_peer_id);
            }
            SwarmEvent::Behaviour(MyBehaviourEvent::Dcutr(dcutr::Event {
                remote_peer_id,
                result: Err(ref e),
            })) => {
                tracing::warn!(
                    "DCUtR direct connection upgrade failed for {}: {:?}",
                    remote_peer_id,
                    e
                );
                // 不重试：DCUtR failure 通常意味着 NAT 限制无法打洞
            }

            // --- UPnP 端口映射事件 ---
            SwarmEvent::Behaviour(MyBehaviourEvent::Upnp(event)) => {
                match event {
                    libp2p::upnp::Event::NewExternalAddr(addr) => {
                        self.upnp_state = "Enabled".to_string();
                        tracing::info!("UPnP: mapped external address {addr}");
                    }
                    libp2p::upnp::Event::ExpiredExternalAddr(addr) => {
                        if matches!(self.upnp_state.as_str(), "Enabled" | "Unknown") {
                            self.upnp_state = "Disabled".to_string();
                        }
                        tracing::warn!("UPnP: port mapping expired for {addr}");
                    }
                    libp2p::upnp::Event::GatewayNotFound => {
                        self.upnp_state = "NotSupported".to_string();
                        tracing::warn!("UPnP: no IGD gateway found on this network");
                    }
                    libp2p::upnp::Event::NonRoutableGateway => {
                        self.upnp_state = "Disabled".to_string();
                        tracing::warn!("UPnP: gateway does not support port mapping");
                    }
                }
            }

            // --- PathRanker 协议事件 ---
            SwarmEvent::Behaviour(MyBehaviourEvent::Pathranker(event)) => {
                self.handle_pathranker_event(event).await;
            }

            _ => {}
        }
    }

    /// 处理 PathRanker 协议事件
    ///
    /// 当前仅处理入站评分查询（ScoreRequest），被动响应邻居的路径评分请求。
    /// 出站评分查询（send_query）未实现，因此 ScoreResponse / OutboundFailure / InboundFailure 路径已被移除。
    /// 若将来实现出站查询，需重新添加 pending_queries、pending_targets、recent_nonces 状态。
    #[tracing::instrument(skip(self, event))]
    async fn handle_pathranker_event(&mut self, event: libp2p_pathranker::PathRankerEvent) {
        let pathranker = &mut self.swarm.behaviour_mut().pathranker;
        let libp2p_pathranker::PathRankerEvent::Message { message, .. } = event else {
            return;
        };
        let RequestResponseMessage::Request {
            request, channel, ..
        } = message else {
            return;
        };

        let target_peer = match request.target.parse::<PeerId>() {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!("ScoreRequest invalid target '{}': {:?}", request.target, e);
                let resp = libp2p_pathranker::ScoreResponse {
                    score: 0.0,
                    updated_at: 0,
                    nonce: request.nonce,
                    target: request.target,
                    signature: Vec::new(),
                    responder_key: Vec::new(),
                    overloaded: false,
                    self_score: pathranker.ranker.self_score(),
                };
                let _ = pathranker.send_response(channel, resp);
                return;
            }
        };
        let best = pathranker.best_addr(&target_peer);
        let (score, updated_at) = match best {
            Some(addr) => {
                let entry = pathranker.ranker.get_entry(&target_peer, &addr);
                let score = entry.map(|e| e.score).unwrap_or(0.0);
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                (score, now)
            }
            None => (0.0, 0),
        };
        let resp = libp2p_pathranker::ScoreResponse {
            score,
            updated_at,
            nonce: request.nonce,
            target: request.target,
            signature: Vec::new(),
            responder_key: Vec::new(),
            overloaded: pathranker.is_overloaded(),
            self_score: pathranker.ranker.self_score(),
        };
        let _ = pathranker.send_response(channel, resp);
    }

    /// 处理 Kademlia 事件
    async fn handle_kademlia_event(&mut self, kad_event: kad::Event) {
        match kad_event {
            kad::Event::OutboundQueryProgressed {
                result: QueryResult::GetRecord(result),
                ..
            } => match result {
                Ok(GetRecordOk::FoundRecord(record)) => {
                    let key_str = std::str::from_utf8(record.record.key.as_ref()).unwrap_or("");
                    let value = record.record.value.clone();
                    self.send_event(P2pEvent::GetRecordResult {
                        key: key_str.to_string(),
                        value,
                    })
                    .await;
                }
                Ok(GetRecordOk::FinishedWithNoAdditionalRecord { .. }) => {
                    tracing::debug!("DHT query finished with no additional records");
                }
                Err(e) => tracing::warn!("DHT get record query failed: {:?}", e),
            },

            kad::Event::OutboundQueryProgressed {
                result: QueryResult::PutRecord(result),
                ..
            } => match result {
                Ok(ok) => tracing::debug!("Successfully published DHT record to {:?}", ok.key),
                Err(e) => {
                    let err_msg = format!("{:?}", e);
                    tracing::warn!("Failed to publish DHT record: {}", err_msg);
                    self.send_event(P2pEvent::DhtPublishFailed { error: err_msg }).await;
                }
            },

            kad::Event::OutboundQueryProgressed {
                result: QueryResult::GetProviders(result),
                ..
            } => match result {
                Ok(kad::GetProvidersOk::FoundProviders { key, providers }) => {
                    let key_str = std::str::from_utf8(key.as_ref()).unwrap_or("");
                    if !key_str.is_empty() && !providers.is_empty() {
                        tracing::debug!(
                            "GetProviders: found {} providers for key {}..",
                            providers.len(),
                            &key_str[..16.min(key_str.len())]
                        );

                        // 发送事件给 ChatCore
                        self.send_event(P2pEvent::GetProvidersResult {
                            key: key_str.to_string(),
                            providers: providers.iter().copied().collect(),
                        })
                        .await;

                        // 如果是中继节点发现 key，自动连接发现的 relay
                        if key_str == DHT_RELAY_INDEX_KEY && !self.relay_server_enabled {
                            for provider in &providers {
                                // 登记为候选（真实地址待连接后 Identify 补齐），并直接拨号。
                                // 传输地址由 GetProviders 查询写入的 Kademlia 路由表解析，
                                // 不在此处用 /p2p-circuit 占位，避免"通过中继拨号到中继"的循环依赖。
                                self.register_relay_candidate(*provider, Multiaddr::empty(), true);
                            }
                        }
                    }
                }
                Ok(kad::GetProvidersOk::FinishedWithNoAdditionalRecord { .. }) => {
                    tracing::trace!("GetProviders finished with no additional record");
                }
                Err(e) => tracing::warn!("Get providers query failed: {:?}", e),
            },

            // 注意：BootstrapReady 不在 RoutingUpdated 中触发（竞态问题）。
            // 见下方 Bootstrap(Ok(BootstrapOk { .. })) 处理。
            kad::Event::RoutingUpdated {
                peer,
                is_new_peer,
                addresses,
                old_peer,
                ..
            } => {
                if is_new_peer {
                    tracing::debug!(
                        "New peer added to routing table: {} with {} addresses",
                        peer,
                        addresses.len()
                    );
                }
                if let Some(old) = old_peer {
                    tracing::debug!("Peer {} replaced by {} in routing table", old, peer);
                }
            }

            // Bootstrap 查询完成：DHT 路由表已初始化，可安全发起联系人发现
            // 不依赖 RoutingUpdated（第一个 peer 加入路由表时触发，此时 DHT 网络可能尚未就绪），
            // 而是等待 Kademlia 的 Bootstrap 查询结果，确保 DHT 网络已建立至少一次查询。
            kad::Event::OutboundQueryProgressed {
                result: QueryResult::Bootstrap(Ok(_)),
                ..
            } => {
                if !self.bootstrap_ready_sent {
                    self.bootstrap_ready_sent = true;
                    tracing::info!("=== BOOTSTRAP OK: DHT bootstrap completed ===");
                    let _ = self.event_tx.try_send(P2pEvent::BootstrapReady);
                }
                // DHT 就绪后重试 start_providing（可能在 bootstrap 前因路由表为空而失败）
                if self.relay_server_allowed && !self.relay_server_enabled {
                    tracing::debug!("DHT bootstrap completed, retrying relay server start_providing");
                    self.try_enable_relay_server();
                }
            }

            _ => tracing::trace!("Unhandled Kademlia event: {:?}", kad_event),
        }
    }
}

mod relay_handler;

impl P2pActor {
    /// 中继接通后补发暂存的 DiscoverPeer 查询（无连接中继时的快速重试路径）
    fn flush_pending_discover_peers(&mut self) {
        if self.pending_discover_peers.is_empty() {
            return;
        }
        let mut candidates: Vec<PeerId> = Vec::new();
        let mut seen = HashSet::new();
        for (pid, _) in &self.relay_nodes {
            if seen.insert(*pid) {
                candidates.push(*pid);
            }
        }
        for (pid, _, _) in &self.relay_candidates {
            if seen.insert(*pid) {
                candidates.push(*pid);
            }
        }
        for pid in &self.relay_connections {
            if seen.insert(*pid) {
                candidates.push(*pid);
            }
        }
        let relay_peers: Vec<PeerId> = candidates
            .into_iter()
            .filter(|pid| self.swarm.is_connected(pid))
            .collect();
        if relay_peers.is_empty() {
            return;
        }
        for key in std::mem::take(&mut self.pending_discover_peers) {
            let request = NetEventRequest::DiscoverPeer {
                mldsa_pubkey_hex: key.clone(),
            };
            for relay_id in &relay_peers {
                p2p_swarm_ops::send_netevent_request(&mut self.swarm, relay_id, request.clone());
                tracing::debug!(
                    "=== DISCOVER_PEER RETRY: {}.. → relay {} ===",
                    &key[..16.min(key.len())],
                    relay_id
                );
            }
        }
    }

    /// 处理 P2pActor 控制命令
    #[tracing::instrument(skip(self, cmd))]
    async fn handle_command(&mut self, cmd: P2pCommand) {
        match cmd {
            P2pCommand::SendMessage { peer_id, message, message_hash } => {
                let req_id = p2p_swarm_ops::send_message(&mut self.swarm, &peer_id, message);
                if !message_hash.is_empty() {
                    self.pending_requests.insert(req_id, message_hash);
                }
            }
            P2pCommand::SendNetEvent { peer_id, request } => {
                p2p_swarm_ops::send_netevent_request(&mut self.swarm, &peer_id, request);
            }
            P2pCommand::SendNetEventResponse { channel, response } => {
                p2p_swarm_ops::send_netevent_response(&mut self.swarm, channel, response);
            }
            P2pCommand::PublishIdentity { mldsa_pubkey_hex } => {
                p2p_swarm_ops::publish_identity_to_dht(&mut self.swarm, &mldsa_pubkey_hex);
            }
            P2pCommand::StopProviding { mldsa_pubkey_hex } => {
                p2p_swarm_ops::stop_providing_to_dht(&mut self.swarm, &mldsa_pubkey_hex);
            }
            P2pCommand::GetProviders { key } => {
                let sem = self.dht_semaphore.clone();
                let _span = tracing::info_span!("dht_get_providers", key = %key[..16.min(key.len())]).entered();
                match sem.try_acquire_owned() {
                    Ok(_permit) => {
                        p2p_swarm_ops::get_providers(&mut self.swarm, &key);
                    }
                    Err(_) => {
                        tracing::warn!("DHT 查询过载，跳过 GetProviders");
                    }
                }
            }
            P2pCommand::AddKademliaAddress { peer_id, addr } => {
                self.dht_cache.add_multiaddr(&peer_id, &addr);
                p2p_swarm_ops::add_kademlia_address(&mut self.swarm, &peer_id, addr);
            }
            P2pCommand::Dial { peer_id } => {
                // 使用 DHT 缓存地址 + 路径评分排序后拨号
                let addrs = self.dht_cache.get_multiaddrs(&peer_id);
                let ranked = self.swarm.behaviour_mut().pathranker.rank(&peer_id, addrs);
                if ranked.is_empty() {
                    p2p_swarm_ops::dial(&mut self.swarm, &peer_id);
                } else {
                    for addr in ranked {
                        if self.swarm.is_connected(&peer_id) {
                            break;
                        }
                        // 记录拨号地址与开始时间：成功后测量真实 RTT，
                        // 失败时把惩罚归因到该真实地址（而非空地址占位）
                        self.swarm
                            .behaviour_mut()
                            .pathranker
                            .note_dial_started(&peer_id, &addr);
                        p2p_swarm_ops::dial_addr(&mut self.swarm, addr);
                    }
                    // 如果缓存地址未建立连接，回退到 swarm.dial(peer_id)
                    // 利用 Kademlia 路由表（可能包含来自 Identify 的中继地址）
                    if !self.swarm.is_connected(&peer_id) {
                        p2p_swarm_ops::dial(&mut self.swarm, &peer_id);
                    }
                }
                // 最后尝试通过中继拨号 circuit 地址
                // 当双方都在 NAT 后且各自连接了中继时，直连地址不可达，必须通过中继
                if !self.swarm.is_connected(&peer_id) {
                    // 先通过已连接的 relay 逐个尝试 circuit 拨号
                    let mut circuit_dialed = false;
                    for relay_pid in &self.relay_connections {
                        // 从候选节点或配置节点中查找 relay 的传输地址（均已归一化为纯传输地址）
                        let relay_addr = self.relay_candidates.iter()
                            .find(|(pid, _, _)| pid == relay_pid)
                            .map(|(_, addr, _)| addr.clone())
                            .or_else(|| {
                                self.relay_nodes.iter()
                                    .find_map(|(pid, addr)| (*pid == *relay_pid).then(|| addr.clone()))
                            });
                        if let Some(addr) = relay_addr {
                            // addr 已归一化（无 P2p 组件），安全追加 P2p + P2pCircuit + 目标 P2p
                            let circuit_addr = addr
                                .with(Protocol::P2p(*relay_pid))
                                .with(Protocol::P2pCircuit)
                                .with(Protocol::P2p(peer_id));
                            tracing::debug!("=== CIRCUIT via relay {}: trying {}", relay_pid, circuit_addr);
                            if self.swarm.dial(circuit_addr).is_ok() {
                                circuit_dialed = true;
                                break;
                            }
                        }
                    }
                    // 最后回退：通过任意 relay 拨号（依赖路由表解析）
                    if !circuit_dialed {
                        let addr = Multiaddr::empty()
                            .with(Protocol::P2pCircuit)
                            .with(Protocol::P2p(peer_id));
                        tracing::debug!("=== CIRCUIT FALLBACK (any relay): trying {}", addr);
                        p2p_swarm_ops::dial_addr(&mut self.swarm, addr);
                    }
                }
            }
            P2pCommand::DialAddr { addr } => {
                // 记录拨号地址与开始时间，使连接结果/失败能归因到真实地址
                if let Some(peer_id) = addr.iter().find_map(|p| match p {
                    libp2p::multiaddr::Protocol::P2p(pid) => Some(pid),
                    _ => None,
                }) {
                    self.swarm
                        .behaviour_mut()
                        .pathranker
                        .note_dial_started(&peer_id, &addr);
                }
                p2p_swarm_ops::dial_addr(&mut self.swarm, addr);
            }
            P2pCommand::SendResponse { channel, response } => {
                p2p_swarm_ops::send_response(&mut self.swarm, channel, response);
            }
            P2pCommand::SendFileChunkResponse { channel, accepted } => {
                if let Err(e) = self.swarm.behaviour_mut().rr_file.send_response(channel, accepted) {
                    tracing::warn!("回送文件分片确认失败: {e:?}");
                }
            }
            P2pCommand::SaveRoutingTable => {
                let cache_path = self.data_dir.join("routing_table.cache");
                p2p::save_routing_table(&mut self.swarm, &cache_path, &self.connected_peers).await;
            }
            P2pCommand::RefreshRoutingTable => {
                p2p_swarm_ops::refresh_routing_table(&mut self.swarm);
            }
            P2pCommand::OnForeground => {
                // 后台切换会静默断连：前台时重建网络连接层。
                // 1) 重拨中继（跳过仍"已连接"的——静默死连接由 ping 超时清理后触发重拨）
                self.dial_relay_nodes();
                // 2) 重跑 Kademlia bootstrap（路由表陈旧时重新填充）
                match self.swarm.behaviour_mut().kademlia.bootstrap() {
                    Ok(_) => {}
                    Err(e) => tracing::warn!("前台重 bootstrap 失败: {e:?}"),
                }
                // 3) 补发暂存的 DiscoverPeer 查询
                self.flush_pending_discover_peers();
                tracing::info!("前台恢复：已重拨中继并触发 DHT bootstrap");
            }
            P2pCommand::DiscoverPeer { mldsa_pubkey_hex } => {
                // 向所有已连接的中继节点发送 DiscoverPeer 查询。
                // 中继来源并集：配置节点 + Identify/DHT 自动发现候选 + 已建立
                // circuit 的中继，避免只查配置节点导致覆盖面不足。
                let mut candidates: Vec<PeerId> = Vec::new();
                let mut seen = HashSet::new();
                for (pid, _) in &self.relay_nodes {
                    if seen.insert(*pid) {
                        candidates.push(*pid);
                    }
                }
                for (pid, _, _) in &self.relay_candidates {
                    if seen.insert(*pid) {
                        candidates.push(*pid);
                    }
                }
                for pid in &self.relay_connections {
                    if seen.insert(*pid) {
                        candidates.push(*pid);
                    }
                }
                let relay_peers: Vec<PeerId> = candidates
                    .into_iter()
                    .filter(|pid| self.swarm.is_connected(pid))
                    .collect();
                if relay_peers.is_empty() {
                    // 无连接中继：暂存待补发，中继接通后由
                    // flush_pending_discover_peers 立即重发，不必等 120s 定时器。
                    tracing::debug!(
                        "DiscoverPeer: no connected relay for {}.., queued",
                        &mldsa_pubkey_hex[..16.min(mldsa_pubkey_hex.len())]
                    );
                    if !self.pending_discover_peers.contains(&mldsa_pubkey_hex) {
                        const MAX_PENDING_DISCOVER: usize = 32;
                        if self.pending_discover_peers.len() >= MAX_PENDING_DISCOVER {
                            self.pending_discover_peers.remove(0);
                        }
                        self.pending_discover_peers.push(mldsa_pubkey_hex.clone());
                    }
                }
                for relay_id in &relay_peers {
                    let request = NetEventRequest::DiscoverPeer {
                        mldsa_pubkey_hex: mldsa_pubkey_hex.clone(),
                    };
                    p2p_swarm_ops::send_netevent_request(&mut self.swarm, relay_id, request);
                    tracing::debug!("=== DISCOVER_PEER REQ: {}.. → relay {} ===", &mldsa_pubkey_hex[..16.min(mldsa_pubkey_hex.len())], relay_id);
                }
            }
            P2pCommand::Shutdown => {
                tracing::info!("P2pActor shutting down...");
                self.disable_relay_server();
            }
            P2pCommand::SetPaidNetworkMode { mode } => {
                // 只有明确检测到「计费」才视为付费；free 与 disabled 均不强制关中继。
                // - paid：自动路径关闭中继服务；用户显式 server 最高优先级，保留
                // - free / disabled：按角色重新评估（disabled = 关闭自动检测，回退持久化角色）
                let paid = mode == PaidNetworkMode::Metered;
                self.paid_network_mode = mode;
                self.paid_network = paid;
                if paid {
                    let user_explicit_server =
                        self.relay_role_user_configured && self.relay_role == RelayRole::Server;
                    if user_explicit_server {
                        tracing::info!("Paid network detected, but user explicitly chose 'server' — keeping relay server");
                    } else {
                        tracing::info!("Paid network detected: turning off relay server");
                        self.disable_relay_server();
                    }
                } else {
                    tracing::info!(
                        "Network mode {:?}: re-evaluating relay server by persisted role",
                        mode
                    );
                    self.try_enable_relay_server();
                }
                self.persist_relay_settings();
            }
            P2pCommand::SetRelayRole { role } => {
                self.set_relay_role(role);
            }
            P2pCommand::SendFileChunk { peer_id, chunk, ack } => {
                let req_id = self.swarm.behaviour_mut().rr_file.send_request(&peer_id, chunk);
                self.pending_file_chunks.insert(req_id, ack);
            }
            P2pCommand::NotifyFileDownloadReject {
                pubkey_hex,
                file_hash,
                reason,
            } => {
                // 转发给 ChatCore，由其构造并发送拒绝响应（需要 ChatCore 的加密/签名密钥）
                self.send_event(P2pEvent::FileDownloadReject {
                    pubkey_hex,
                    file_hash,
                    reason,
                })
                .await;
            }
            P2pCommand::GetNetworkStatus { resp } => {
                let status = self.build_network_status();
                let json = serde_json::to_string(&status).unwrap_or_else(|_| "{}".to_string());
                let _ = resp.send(json);
            }
            P2pCommand::ExportRoutingTable { resp } => {
                let export = self.build_routing_table_export();
                let json = serde_json::to_string(&export).unwrap_or_else(|_| "{}".to_string());
                let _ = resp.send(json);
            }
            P2pCommand::ImportRoutingTable { data, resp } => {
                let result = self.import_routing_table(&data);
                let _ = resp.send(result);
            }
        }
    }

    /// 构建网络状态数据（用于前端网络监控组件）
    fn build_network_status(&mut self) -> crate::command::NetworkStatusData {
        let local_peer_id = self.swarm.local_peer_id().to_base58();
        let online = !self.connected_peers.is_empty();
        let nat_status_str = match &self.nat_status {
            autonat::NatStatus::Public(_) => "Public".to_string(),
            autonat::NatStatus::Private => "Private".to_string(),
            autonat::NatStatus::Unknown => "Unknown".to_string(),
        };

        let (error_code, error_message) = if online {
            ("OK".to_string(), None)
        } else if self.bootstrap_ready_sent {
            (crate::command::NetworkStatusData::ERR_DEGRADED_NO_PEERS.to_string(), Some("Network is up but no peers are connected".to_string()))
        } else {
            (crate::command::NetworkStatusData::ERR_NOT_READY.to_string(), Some("Core is initializing, no network connections yet".to_string()))
        };

        let public_ip = match &self.nat_status {
            autonat::NatStatus::Public(addr) => Some(addr.to_string()),
            _ => None,
        };

        let mut ipv4: Vec<String> = Vec::new();
        let mut ipv6: Vec<String> = Vec::new();
        for addr in &self.listen_addrs {
            let s = addr.to_string();
            if addr.iter().any(|p| matches!(p, libp2p::multiaddr::Protocol::Ip4(_)))
                && !ipv4.contains(&s) { ipv4.push(s.clone()); }
            if addr.iter().any(|p| matches!(p, libp2p::multiaddr::Protocol::Ip6(_)))
                && !ipv6.contains(&s) { ipv6.push(s); }
        }

        let relay_connected = !self.relay_connections.is_empty();
        let connected_relay_peer = self.relay_connections.iter().next().map(|p| p.to_base58());

        let external_addresses: Vec<String> = self.swarm.external_addresses().map(|a| a.to_string()).collect();

        // UPnP 状态来自行为事件跟踪（Enabled/Disabled/NotSupported），
        // 仅在从未收到任何 UPnP 事件时回退到外部地址启发式判断
        let upnp_status = if self.upnp_state != "Unknown" {
            self.upnp_state.clone()
        } else if external_addresses.iter().any(|a| a.contains("/ip4/")) {
            "Enabled".to_string()
        } else {
            "Unknown".to_string()
        };

        let bootstrap_node_set: std::collections::HashSet<PeerId> =
            self.bootstrap_nodes.iter().map(|(pid, _)| *pid).collect();

        let local_pid = *self.swarm.local_peer_id();

        let mut known_peers: Vec<crate::command::PeerInfoDto> = Vec::new();

        known_peers.push(crate::command::PeerInfoDto {
            peer_id: local_peer_id.clone(),
            connected: true,
            is_relay: false,
            is_bootstrap: false,
            is_self: true,
        });

        for peer_id in self.connected_peers.keys() {
            if *peer_id == local_pid {
                continue;
            }
            let is_relay = self.relay_connections.contains(peer_id)
                || self.relay_nodes.iter().any(|(pid, _)| pid == peer_id);
            let is_bootstrap = bootstrap_node_set.contains(peer_id);
            known_peers.push(crate::command::PeerInfoDto {
                peer_id: peer_id.to_base58(),
                connected: true,
                is_relay,
                is_bootstrap,
                is_self: false,
            });
        }

        // A2: 补充路由表已知但未连接的节点（kademlia kbuckets）
        {
            let kademlia = &mut self.swarm.behaviour_mut().kademlia;
            for bucket in kademlia.kbuckets() {
                for entry in bucket.iter() {
                    let pid = entry.node.key.preimage();
                    if *pid == local_pid || self.connected_peers.contains_key(pid) {
                        continue;
                    }
                    known_peers.push(crate::command::PeerInfoDto {
                        peer_id: pid.to_base58(),
                        connected: false,
                        is_relay: self.relay_nodes.iter().any(|(rp, _)| rp == pid),
                        is_bootstrap: bootstrap_node_set.contains(pid),
                        is_self: false,
                    });
                }
            }
        }

        let relay_enabled = self.relay_server_enabled;
        let bootstrap_ready = self.bootstrap_ready_sent;

        // 可推荐直连地址（二维码 addr 用）：监听地址 + 外部地址并集，
        // 保留局域网/私有地址（同局域网扫码直连），排除环回/链路本地/组播/circuit。
        let mut recommend_addrs: Vec<String> = Vec::new();
        // libp2p 对 0.0.0.0 绑定只上报 0.0.0.0（is_unspecified 会被过滤，不可拨号）；
        // 局域网 IP 需从系统网卡枚举并与监听端口组合成可拨号地址，
        // 否则同局域网无服务器场景下二维码无地址可拨，只能走 mDNS/DHT 慢路径。
        let tcp_port: u16 = self
            .swarm
            .listeners()
            .find_map(|a| {
                a.iter().find_map(|p| match p {
                    Protocol::Tcp(port) => Some(port),
                    _ => None,
                })
            })
            .unwrap_or(0);
        if tcp_port > 0
            && let Ok(ifaces) = if_addrs::get_if_addrs()
        {
            let local_pid = *self.swarm.local_peer_id();
            for iface in ifaces {
                let ip = match iface.addr {
                    if_addrs::IfAddr::V4(a) => std::net::IpAddr::V4(a.ip),
                    if_addrs::IfAddr::V6(a) => std::net::IpAddr::V6(a.ip),
                };
                if ip.is_loopback() || ip.is_unspecified() {
                    continue;
                }
                let addr = libp2p::Multiaddr::from(ip)
                    .with(Protocol::Tcp(tcp_port))
                    .with(Protocol::P2p(local_pid));
                let s = addr.to_string();
                if !recommend_addrs.contains(&s)
                    && p2p_swarm_ops::is_qr_recommend_addr(&addr)
                {
                    recommend_addrs.push(s);
                    if recommend_addrs.len() >= 16 {
                        break;
                    }
                }
            }
        }
        for a in self.swarm.listeners().chain(self.swarm.external_addresses()) {
            let s = a.to_string();
            if recommend_addrs.len() >= 16 {
                break;
            }
            if !recommend_addrs.contains(&s)
                && s.parse::<libp2p::Multiaddr>()
                    .map(|m| p2p_swarm_ops::is_qr_recommend_addr(&m))
                    .unwrap_or(false)
            {
                recommend_addrs.push(s);
            }
        }

        crate::command::NetworkStatusData {
            error_code,
            error_message,
            online,
            is_paid_network: self.paid_network,
            paid_network_mode: self.paid_network_mode,
            relay_enabled,
            relay_role: self.relay_role,
            nat_status: nat_status_str,
            upnp_status,
            ipv4,
            ipv6,
            public_ip,
            known_peers,
            relay_connected,
            bootstrap_ready,
            connected_relay_peer,
            external_addresses,
            recommend_addrs,
            local_peer_id,
            connected_peer_count: self.connected_peers.len() as u64,
        }
    }

    /// 构建路由表导出数据（含本节点 + 路由表已知节点 + bootstrap/relay 节点）
    fn build_routing_table_export(&mut self) -> crate::command::RoutingTableExport {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;

        let local_pid = *self.swarm.local_peer_id();

        let mut self_addresses: Vec<String> = self.listen_addrs.iter().map(|a| a.to_string()).collect();
        for a in self.swarm.external_addresses() {
            let s = a.to_string();
            if !self_addresses.contains(&s) {
                self_addresses.push(s);
            }
        }

        let bootstrap_set: std::collections::HashSet<PeerId> =
            self.bootstrap_nodes.iter().map(|(pid, _)| *pid).collect();
        let relay_set: std::collections::HashSet<PeerId> =
            self.relay_nodes.iter().map(|(pid, _)| *pid).collect();

        let mut seen: std::collections::HashSet<PeerId> = std::collections::HashSet::new();
        let mut peers: Vec<crate::command::RoutingTableExportPeer> = Vec::new();

        // 1. 活跃连接节点（含 bootstrap/relay 标记）
        for pid in self.connected_peers.keys() {
            if *pid == local_pid || seen.contains(pid) {
                continue;
            }
            seen.insert(*pid);
            let addrs = self.dht_cache.get_multiaddrs(pid);
            peers.push(crate::command::RoutingTableExportPeer {
                peer_id: pid.to_base58(),
                addresses: addrs.iter().map(|a| a.to_string()).collect(),
                is_bootstrap: bootstrap_set.contains(pid),
                is_relay: relay_set.contains(pid) || self.relay_connections.contains(pid),
            });
        }

        // 2. 路由表已知节点（kbuckets）
        let kademlia = &mut self.swarm.behaviour_mut().kademlia;
        for bucket in kademlia.kbuckets() {
            for entry in bucket.iter() {
                let pid = entry.node.key.preimage();
                if *pid == local_pid || seen.contains(pid) {
                    continue;
                }
                seen.insert(*pid);
                let addrs: Vec<String> = entry.node.value.iter().map(|a| a.to_string()).collect();
                if !addrs.is_empty() {
                    peers.push(crate::command::RoutingTableExportPeer {
                        peer_id: pid.to_base58(),
                        addresses: addrs,
                        is_bootstrap: bootstrap_set.contains(pid),
                        is_relay: relay_set.contains(pid),
                    });
                }
            }
        }

        // 3. 未在线且不在路由表但已配置的 bootstrap/relay 节点（公网节点）
        for (pid, addr) in self.bootstrap_nodes.iter().chain(self.relay_nodes.iter()) {
            if *pid == local_pid || seen.contains(pid) {
                continue;
            }
            seen.insert(*pid);
            peers.push(crate::command::RoutingTableExportPeer {
                peer_id: pid.to_base58(),
                addresses: vec![addr.to_string()],
                is_bootstrap: bootstrap_set.contains(pid),
                is_relay: relay_set.contains(pid),
            });
        }

        crate::command::RoutingTableExport {
            version: crate::command::RoutingTableExport::CURRENT_VERSION,
            exported_at: now,
            self_peer_id: local_pid.to_base58(),
            self_addresses,
            peers,
        }
    }

    /// 导入路由表：将导出的 peers 加入 kademlia 路由表
    fn import_routing_table(&mut self, json: &str) -> String {
        let export: crate::command::RoutingTableExport = match serde_json::from_str(json) {
            Ok(e) => e,
            Err(e) => {
                return serde_json::json!({
                    "imported": 0,
                    "error": format!("invalid export file: {}", e)
                }).to_string();
            }
        };

        if export.version == 0 || export.version > crate::command::RoutingTableExport::CURRENT_VERSION {
            return serde_json::json!({
                "imported": 0,
                "error": format!("unsupported version: {}", export.version)
            }).to_string();
        }

        let mut imported = 0u32;
        let mut errors: Vec<String> = Vec::new();
        // 与 Kademlia 路由表容量匹配，避免恶意导出文件淹没本地路由表并挤掉合法节点
        const MAX_IMPORT_ADDRESSES: u32 = 1_000;
        let mut seen: std::collections::HashSet<(PeerId, Multiaddr)> =
            std::collections::HashSet::new();

        fn is_valid_routing_addr(addr: &Multiaddr) -> bool {
        // 必须有传输层协议（Tcp / QuicV1），拒绝 /p2p-circuit 等无传输地址
        if !addr.iter().any(|p| matches!(p, Protocol::Tcp(_) | Protocol::QuicV1)) {
            return false;
        }
        // 拒绝 DNS 协议（拨号时解析可指向内部地址，绕过 IP 校验）
        if addr.iter().any(|p| matches!(p, Protocol::Dns(_) | Protocol::Dns4(_) | Protocol::Dns6(_) | Protocol::Dnsaddr(_))) {
            return false;
        }
        // 拒绝 loopback、私有、link-local、unspecified 地址
        for p in addr.iter() {
            match p {
                Protocol::Ip4(ip) => {
                    if ip.is_loopback() || ip.is_private() || ip.is_link_local() || ip.is_unspecified() {
                        return false;
                    }
                }
                Protocol::Ip6(ip) => {
                    // 手动判断私有/链路本地（该 Rust 版本无 is_private/is_link_local 方法）
                    let oct = ip.octets();
                    let is_private_v6 = (oct[0] & 0xfe) == 0xfc; // fc00::/7 (ULA)
                    let is_link_local_v6 = oct[0] == 0xfe && (oct[1] & 0xc0) == 0x80; // fe80::/10
                    if ip.is_loopback() || ip.is_unspecified() || is_private_v6 || is_link_local_v6 {
                        return false;
                    }
                    // IPv4-mapped IPv6 地址（::ffff:127.0.0.1 等），递归检查底层 IPv4
                    if let Some(v4) = ip.to_ipv4_mapped()
                        && (v4.is_loopback() || v4.is_private() || v4.is_link_local() || v4.is_unspecified()) {
                            return false;
                        }
                }
                _ => {}
            }
        }
        true
    }

    for peer in &export.peers {
            let pid: PeerId = match peer.peer_id.parse() {
                Ok(p) => p,
                Err(e) => {
                    errors.push(format!("invalid peer_id '{}': {}", &peer.peer_id[..16.min(peer.peer_id.len())], e));
                    continue;
                }
            };
            if imported >= MAX_IMPORT_ADDRESSES {
                    errors.push("import limit reached (max 1,000 addresses)".to_string());
                    break;
                }
            for addr_str in &peer.addresses {
                let addr: Multiaddr = match addr_str.parse() {
                    Ok(a) => a,
                    Err(e) => {
                        errors.push(format!("invalid addr for {}: {}", &peer.peer_id[..16.min(peer.peer_id.len())], e));
                        continue;
                    }
                };
                // 地址语义校验：拒绝不可路由或内部地址
                if !is_valid_routing_addr(&addr) {
                    errors.push(format!("unroutable addr for {}: {}", &peer.peer_id[..16.min(peer.peer_id.len())], addr));
                    continue;
                }
                if !seen.insert((pid, addr.clone())) {
                    continue;
                }
                self.swarm.behaviour_mut().kademlia.add_address(&pid, addr.clone());
                self.dht_cache.add_multiaddr(&pid, &addr);
                imported += 1;
            }
        }

        serde_json::json!({
            "imported": imported,
            "error": if errors.is_empty() { serde_json::Value::Null } else { serde_json::Value::String(errors.join("; ")) }
        }).to_string()
    }
}

/// P2pActor 的句柄，用于向 P2pActor 发送命令和控制生命周期
pub struct P2pActorHandle {
    /// 命令发送通道
    pub tx: mpsc::Sender<P2pCommand>,
    /// 取消令牌
    pub cancellation_token: CancellationToken,
    /// 后台任务句柄
    pub join_handle: JoinHandle<()>,
}

impl P2pActorHandle {
    /// 异步发送命令
    #[allow(clippy::result_large_err)]
    pub async fn send(
        &self,
        cmd: P2pCommand,
    ) -> Result<(), Box<mpsc::error::SendError<P2pCommand>>> {
        self.tx.send(cmd).await.map_err(Box::new)
    }

    /// 触发关闭信号
    pub fn shutdown(&self) {
        self.cancellation_token.cancel();
    }

    /// 等待后台任务结束（消费 join_handle）
    pub async fn join(self) {
        let _ = self.join_handle.await;
    }

    /// 发送关闭命令并等待退出
    pub async fn graceful_shutdown(self) {
        self.cancellation_token.cancel();
        self.join().await;
    }
}

/// 默认消息通道容量
const DEFAULT_CHANNEL_SIZE: usize = 64;

/// 最大并发 DHT 查询数（GetProviders/GetRecord）
const MAX_CONCURRENT_DHT_QUERIES: usize = 8;

/// P2pActor 构建器
///
/// 用法：
/// ```ignore
/// let (handle, rx) = P2pActorBuilder::new()
///     .swarm(swarm)
///     .dht_cache(cache)
///     .data_dir(dir)
///     .relay_nodes(nodes)
///     .start();
/// ```
pub struct P2pActorBuilder {
    swarm: Option<Swarm<MyBehaviour>>,
    dht_cache: Option<Arc<DhtCache>>,
    data_dir: Option<std::path::PathBuf>,
    relay_nodes: Vec<(String, String)>,
    bootstrap_nodes: Vec<(String, String)>,
    channel_size: usize,
    cancellation_token: Option<CancellationToken>,
}

impl P2pActorBuilder {
    /// 创建新的构建器，所有字段默认为 None/空
    pub fn new() -> Self {
        Self {
            swarm: None,
            dht_cache: None,
            data_dir: None,
            relay_nodes: Vec::new(),
            bootstrap_nodes: Vec::new(),
            channel_size: DEFAULT_CHANNEL_SIZE,
            cancellation_token: None,
        }
    }

    /// 必需：P2P Swarm（已配置好 behaviour）
    pub fn swarm(mut self, swarm: Swarm<MyBehaviour>) -> Self {
        self.swarm = Some(swarm);
        self
    }

    /// 必需：DHT 缓存（Arc 共享）
    pub fn dht_cache(mut self, cache: Arc<DhtCache>) -> Self {
        self.dht_cache = Some(cache);
        self
    }

    /// 必需：数据目录（用于路由表持久化）
    pub fn data_dir(mut self, dir: std::path::PathBuf) -> Self {
        self.data_dir = Some(dir);
        self
    }

    /// 可选：中继节点列表 [(PeerId, Multiaddr)]
    pub fn relay_nodes(mut self, nodes: Vec<(String, String)>) -> Self {
        self.relay_nodes = nodes;
        self
    }

    /// 可选：Bootstrap 节点列表 [(PeerId, Multiaddr)]
    pub fn bootstrap_nodes(mut self, nodes: Vec<(String, String)>) -> Self {
        self.bootstrap_nodes = nodes;
        self
    }

    /// 可选：命令通道容量（默认 64）
    pub fn channel_size(mut self, size: usize) -> Self {
        self.channel_size = size;
        self
    }

    /// 可选：取消令牌（用于外部关闭）
    pub fn cancellation_token(mut self, token: CancellationToken) -> Self {
        self.cancellation_token = Some(token);
        self
    }

    /// 构建并启动 P2pActor，返回 (句柄, 事件接收器)
    pub fn start(self) -> (P2pActorHandle, mpsc::Receiver<P2pEvent>) {
        let (p2p_event_tx, p2p_event_rx) = mpsc::channel(self.channel_size);
        // 在边界处一次性解析并归一化中继节点（丢弃无效/重复条目）
        let relay_nodes = relay_handler::parse_relay_nodes(self.relay_nodes, "relay");
        let bootstrap_nodes = relay_handler::parse_relay_nodes(self.bootstrap_nodes, "bootstrap");
        let actor = P2pActor::new(
            self.swarm.expect("P2pActorBuilder: swarm is required"),
            self.dht_cache.expect("P2pActorBuilder: dht_cache is required"),
            self.data_dir.expect("P2pActorBuilder: data_dir is required"),
            p2p_event_tx,
            relay_nodes,
            bootstrap_nodes,
        );
        let token = self.cancellation_token.unwrap_or_default();
        let handle = Self::spawn(actor, self.channel_size, token);
        (handle, p2p_event_rx)
    }

    /// 在全局运行时上启动 P2pActor 事件循环
    fn spawn(
        mut actor: P2pActor,
        channel_size: usize,
        cancellation_token: CancellationToken,
    ) -> P2pActorHandle {
        let (tx, mut rx) = mpsc::channel::<P2pCommand>(channel_size);
        let ct = cancellation_token.clone();

        let join_handle = crate::actor::RUNTIME.spawn(async move {
            actor.dial_relay_nodes();

            // 备选节点串行探活定时器：每轮只挑 1 个不在线节点拨号
            let mut backup_probe =
                tokio::time::interval(std::time::Duration::from_secs(BACKUP_PROBE_INTERVAL_SECS));
            backup_probe.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

            loop {
                tokio::select! {
                    event = actor.swarm.select_next_some() => {
                        actor.handle_swarm_event(event).await;
                    }
                    cmd_opt = rx.recv() => {
                        match cmd_opt {
                            Some(P2pCommand::Shutdown) => {
                                tracing::info!("P2pActor 收到关闭命令");
                                actor.disable_relay_server();
                                let cache_path = actor.data_dir.join("routing_table.cache");
                                p2p::save_routing_table(&mut actor.swarm, &cache_path, &actor.connected_peers).await;
                                break;
                            }
                            Some(cmd) => actor.handle_command(cmd).await,
                            None => break,
                        }
                    }
                    _ = backup_probe.tick() => {
                        if actor.probe_one_backup_peer() {
                            tracing::debug!("Backup peer probe tick: selected an offline peer to dial");
                        }
                    }
                    _ = cancellation_token.cancelled() => {
                        tracing::info!("P2pActor 收到取消信号");
                        actor.disable_relay_server();
                        let cache_path = actor.data_dir.join("routing_table.cache");
                        p2p::save_routing_table(&mut actor.swarm, &cache_path, &actor.connected_peers).await;
                        break;
                    }
                }
            }
            tracing::info!("P2pActor 事件循环已退出");
        });

        P2pActorHandle {
            tx,
            cancellation_token: ct,
            join_handle,
        }
    }
}

impl Default for P2pActorBuilder {
    fn default() -> Self {
        Self::new()
    }
}
