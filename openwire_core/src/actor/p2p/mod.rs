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
use crate::p2p::dht_cache::DhtCache;
use crate::p2p::netevent::{NetEventRequest, NetEventResponse};
use crate::p2p::{self, DHT_PROVIDER_CALLBACKS};
use crate::{ChatMessage, ChatResponse};
use crate::command::NetworkStatusData;

use self::swarm_ops as p2p_swarm_ops;

// ============================================================================
// P2pActor 命令和事件
// ============================================================================

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
    /// 保存路由表
    SaveRoutingTable,
    /// 随机刷新路由表（随机桶查询，扩展路由表覆盖）
    RefreshRoutingTable,
    /// 通过中继发现对端 PeerID
    DiscoverPeer {
        /// 目标 ML-DSA 公钥 hex
        mldsa_pubkey_hex: String,
    },
    /// 关闭
    Shutdown,
    /// 设置计费网络检测模式："free" / "paid" / "disabled"
    /// 禁用时中继始终关闭，优先于 API 自动检测与用户手动选择
    SetPaidNetworkMode {
        /// 检测模式
        mode: String,
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
    /// 设置中继角色："server" / "client" / "off"（互斥，server 与 client 不能同时启用）
    SetRelayRole {
        /// 角色
        role: String,
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
    paid_network_mode: String,
    /// 已发起 reservation 请求的 relay（防重复）
    reservation_attempted: HashSet<PeerId>,
    /// Relay DHT 查询冷却时间（防止频繁 get_providers）
    relay_dht_query_cooldown_until: Option<std::time::Instant>,

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

    /// UPnP 检测结果状态
    upnp_state: String,
    /// 中继角色："server" / "client" / "off"（互斥）
    relay_role: String,
}

impl P2pActor {
    /// 创建新的 P2pActor
    pub fn new(
        swarm: Swarm<MyBehaviour>,
        dht_cache: Arc<DhtCache>,
        data_dir: std::path::PathBuf,
        event_tx: mpsc::Sender<P2pEvent>,
        relay_nodes: Vec<(PeerId, Multiaddr)>,
        bootstrap_nodes: Vec<(PeerId, Multiaddr)>,
    ) -> Self {
        Self {
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
            relay_role_user_configured: false,
            paid_network: true,
            paid_network_mode: "paid".to_string(),
            reservation_attempted: HashSet::new(),
            relay_dht_query_cooldown_until: None,
            dht_semaphore: Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_DHT_QUERIES)),
            bootstrap_ready_sent: false,
            pending_requests: HashMap::new(),
            upnp_state: "Unknown".to_string(),
            relay_role: "client".to_string(),
        }
    }

    fn is_relay_addr(addr: &libp2p::multiaddr::Multiaddr) -> bool {
        addr.iter().any(|p| matches!(p, Protocol::P2pCircuit))
    }

    /// 发送事件到 ChatCore
    ///
    /// 关键事件（MessageReceived/GetProvidersResult）使用 send() 确保不丢失；
    /// 非关键事件使用 try_send() 避免背压死锁。
    async fn send_event(&mut self, event: P2pEvent) {
        let is_critical = matches!(
            event,
            P2pEvent::MessageReceived { .. } | P2pEvent::GetProvidersResult { .. }
        );
        if is_critical {
            if let Err(e) = self.event_tx.send(event).await {
                tracing::error!("事件通道关闭，P2pActor 退出: {e:?}");
            }
        } else if let Err(e) = self.event_tx.try_send(event) {
            tracing::warn!("事件通道满，丢弃非关键 P2pEvent: {e:?}");
        }
    }

    /// 处理单个 swarm 事件
    #[tracing::instrument(skip(self, event))]
    async fn handle_swarm_event(&mut self, event: SwarmEvent<MyBehaviourEvent>) {
        match event {
            SwarmEvent::Behaviour(MyBehaviourEvent::Autonat(event)) => {
                if let autonat::Event::StatusChanged {
                    old: _,
                    new: new_status,
                } = event
                {
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
                            if !self.relay_role_user_configured && self.relay_role == "server" {
                                self.relay_role = "client".to_string();
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
                        return;
                    }
                    NetEventResponse::Ack => {
                        tracing::debug!("NetEvent 请求被 {} 确认（Ack）", peer);
                        return;
                    }
                    _ => {}
                }
            }
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
                    let already_known =
                        self.relay_candidates.iter().any(|(pid, _, _)| *pid == peer_id);
                    if !already_known && self.relay_candidates.len() < MAX_RELAY_CANDIDATES {
                        // 选择第一个非 loopback / 非 circuit 地址，避免拨号到不可达地址
                        let first_usable = info.listen_addrs.iter().find(|a| {
                            let s = a.to_string();
                            !s.contains("127.0.0.1")
                                && !s.contains("::1")
                                && !s.contains("/p2p-circuit")
                        });
                        let Some(selected_addr) = first_usable.or_else(|| info.listen_addrs.first()) else {
                            tracing::debug!(
                                "Discovered relay-capable candidate {} but no usable address",
                                peer_id
                            );
                            return;
                        };
                        // 归一化为纯传输地址（剥离 P2p 组件），保证存储不变量
                        let selected_addr = relay_handler::strip_p2p(&selected_addr);
                        self.relay_candidates.push((peer_id, selected_addr.clone(), std::time::Instant::now()));
                            tracing::debug!(
                                "Discovered relay-capable candidate: {} at {}",
                                peer_id,
                                selected_addr
                            );
                        // 如果 NAT 后，立即尝试连接 relay
                        if matches!(
                            self.nat_status,
                            autonat::NatStatus::Private | autonat::NatStatus::Unknown
                        ) {
                            self.dial_relay_nodes();
                        }
                    }
                    // Identify 完成后，对所有 relay 节点请求 circuit reservation
                    // 与官方 DCUtR 示例一致：dial → Identify → listen_on(relay_addr.with(P2pCircuit))
                    if !self.reservation_attempted.contains(&peer_id) {
                        if self.on_relay_connected(&peer_id) {
                            self.reservation_attempted.insert(peer_id);
                        }
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
                    }
                } else {
                    tracing::debug!("Direct connection established with {}", peer_id);
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
                self.relay_connections.remove(&peer_id);
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
                Some(pid) => tracing::error!("Connect failed to {pid}: {error:?}"),
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
            #[cfg(feature = "pathranker")]
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
    #[cfg(feature = "pathranker")]
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

                        // 通知等待的 oneshot callbacks
                        if let Ok(mut callbacks) = DHT_PROVIDER_CALLBACKS.lock()
                            && let Some(sender) = callbacks.remove(key_str)
                                && let Some(first_provider) = providers.iter().next() {
                                    let _ = sender.send(*first_provider);
                                }

                        // 发送事件给 ChatCore
                        self.send_event(P2pEvent::GetProvidersResult {
                            key: key_str.to_string(),
                            providers: providers.iter().copied().collect(),
                        })
                        .await;

                        // 如果是中继节点发现 key，自动连接发现的 relay
                        if key_str == DHT_RELAY_INDEX_KEY && !self.relay_server_enabled {
                            for provider in &providers {
                                if !self.relay_connections.contains(provider) {
                                    // 将 DHT 发现的 relay 加入候选列表，由 dial_relay_nodes 统一管理
                                    let already_candidate = self.relay_candidates.iter()
                                        .any(|(pid, _, _)| pid == provider);
                                    if !already_candidate && self.relay_candidates.len() < MAX_RELAY_CANDIDATES {
                                        // 仅记录 PeerId，传输地址待 Kademlia 路由表解析；
                                        // strip_p2p 后为空地址，dial_relay_nodes 的传输层检查会跳过它
                                        // 不能使用 /p2p-circuit 占位，否则会形成 "通过中继拨号到中继" 的循环依赖
                                        self.relay_candidates.push((*provider, Multiaddr::empty(), std::time::Instant::now()));
                                        tracing::debug!("DHT relay {} added to candidates", provider);
                                    }
                                }
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
                let _ = self.dht_cache.add_multiaddr(&peer_id, &addr);
                p2p_swarm_ops::add_kademlia_address(&mut self.swarm, &peer_id, addr);
            }
            P2pCommand::Dial { peer_id } => {
                // 使用 DHT 缓存地址 + 路径评分排序后拨号
                let addrs = self.dht_cache.get_multiaddrs(&peer_id).unwrap_or_default();
                #[cfg(feature = "pathranker")]
                let ranked = self.swarm.behaviour_mut().pathranker.rank(&peer_id, addrs);
                #[cfg(not(feature = "pathranker"))]
                let ranked = addrs;
                if ranked.is_empty() {
                    p2p_swarm_ops::dial(&mut self.swarm, &peer_id);
                } else {
                    for addr in ranked {
                        if self.swarm.is_connected(&peer_id) {
                            break;
                        }
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
                p2p_swarm_ops::dial_addr(&mut self.swarm, addr);
            }
            P2pCommand::SendResponse { channel, response } => {
                p2p_swarm_ops::send_response(&mut self.swarm, channel, response);
            }
            P2pCommand::SaveRoutingTable => {
                let cache_path = self.data_dir.join("routing_table.cache");
                p2p::save_routing_table(&mut self.swarm, &cache_path, &self.connected_peers).await;
            }
            P2pCommand::RefreshRoutingTable => {
                p2p_swarm_ops::refresh_routing_table(&mut self.swarm);
            }
            P2pCommand::DiscoverPeer { mldsa_pubkey_hex } => {
                // 向所有已连接的中继节点发送 DiscoverPeer 查询
                let relay_peers: Vec<PeerId> = self.relay_nodes
                    .iter()
                    .map(|(pid, _)| *pid)
                    .filter(|pid| self.swarm.is_connected(pid))
                    .collect();
                if relay_peers.is_empty() {
                    tracing::debug!("DiscoverPeer: no connected relay for {}", &mldsa_pubkey_hex[..16.min(mldsa_pubkey_hex.len())]);
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
                let paid = match mode.as_str() {
                    "free" => false,
                    "paid" | "disabled" => true,
                    _ => { tracing::warn!("unknown paid_network_mode: {mode}"); return; }
                };
                self.paid_network_mode = mode;
                self.paid_network = paid;
                if paid {
                    tracing::info!("Paid/disabled network: turning off relay server");
                    self.disable_relay_server();
                } else {
                    tracing::info!("Free network: re-evaluating relay server");
                    self.try_enable_relay_server();
                }
            }
            P2pCommand::SetRelayRole { role } => {
                self.set_relay_role(&role);
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
            if addr.iter().any(|p| matches!(p, libp2p::multiaddr::Protocol::Ip4(_))) {
                if !ipv4.contains(&s) { ipv4.push(s.clone()); }
            }
            if addr.iter().any(|p| matches!(p, libp2p::multiaddr::Protocol::Ip6(_))) {
                if !ipv6.contains(&s) { ipv6.push(s); }
            }
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

        for (peer_id, _count) in &self.connected_peers {
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

        crate::command::NetworkStatusData {
            error_code,
            error_message,
            online,
            is_paid_network: self.paid_network,
            paid_network_mode: self.paid_network_mode.clone(),
            relay_enabled,
            relay_role: self.relay_role.clone(),
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
        for (pid, _) in &self.connected_peers {
            if *pid == local_pid || seen.contains(pid) {
                continue;
            }
            seen.insert(*pid);
            let addrs = self.dht_cache.get_multiaddrs(pid).unwrap_or_default();
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
        const MAX_IMPORT_ADDRESSES: u32 = 10_000;

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
                    if let Some(v4) = ip.to_ipv4_mapped() {
                        if v4.is_loopback() || v4.is_private() || v4.is_link_local() || v4.is_unspecified() {
                            return false;
                        }
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
                    errors.push("import limit reached (max 10,000 addresses)".to_string());
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
                self.swarm.behaviour_mut().kademlia.add_address(&pid, addr.clone());
                let _ = self.dht_cache.add_multiaddr(&pid, &addr);
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
    pub async fn send(&self, cmd: P2pCommand) -> Result<(), mpsc::error::SendError<P2pCommand>> {
        self.tx.send(cmd).await
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
                    _ = cancellation_token.cancelled() => {
                        tracing::info!("P2pActor 收到取消信号");
                        actor.disable_relay_server();
                        let cache_path = actor.data_dir.join("routing_table.cache");
                        p2p::save_routing_table(&mut actor.swarm, &cache_path, &actor.connected_peers);
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
