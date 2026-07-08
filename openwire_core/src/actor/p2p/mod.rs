//! P2pActor：P2P 网络事件循环 Actor
//!
//! 将 libp2p Swarm 的事件循环从 ChatCore 中分离出来，独立运行在 P2pActor 中。
//! ChatCore 通过 P2pActorHandle 与 P2pActor 通信。
//!
//! # 架构
//! - P2pActor 拥有 Swarm 的所有权，负责处理所有网络事件
//! - ChatCore 不再持有 Swarm，通过命令通道与 P2pActor 交互
//! - P2pActor 将网络事件转换为 P2pEvent 发送给 ChatCore

pub mod netevent;
pub mod swarm_ops;

use std::collections::HashSet;
use std::str::FromStr;
use std::sync::Arc;
use std::sync::LazyLock;

use async_trait::async_trait;
use futures::StreamExt;
use libp2p::kad::{self, GetRecordOk, QueryResult, Mode};
use libp2p::multiaddr::Protocol;
use libp2p::request_response::{Event as RequestResponseEvent, Message as RequestResponseMessage};
use libp2p::swarm::SwarmEvent;
use libp2p::{PeerId, Swarm, autonat, core, dcutr, identify, mdns, relay};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::actor::{Actor, ActorCommand, ActorHandle};
use crate::p2p::behaviour::{MyBehaviour, MyBehaviourEvent};
use crate::p2p::dht_cache::DhtCache;
use crate::p2p::netevent::{NetEventRequest, NetEventResponse};
use crate::p2p::{self, DHT_PROVIDER_CALLBACKS};
use crate::{ChatMessage, ChatResponse};

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
    /// 发布身份到 DHT
    PublishIdentity {
        /// ML-DSA 公钥 hex
        mldsa_pubkey_hex: String,
        /// ML-KEM 公钥 hex
        mlkem_pubkey_hex: String,
    },
    /// 发起 GetProviders 查询
    GetProviders {
        /// DHT 查询键
        key: String,
    },
    /// 发起 GetRecord 查询
    GetRecord {
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
    /// 关闭
    Shutdown,
    /// 配置中继服务（前端计费网络检测后调用）
    RelayServerConfig {
        /// 是否允许启用中继服务
        allowed: bool,
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
    /// 日志
    Log(String),
}

// ============================================================================
// AutoRelay 常量
// ============================================================================

/// Circuit Relay v2 hop protocol ID — 中继服务器必须支持的协议
static RELAY_HOP_PROTOCOL: LazyLock<libp2p::StreamProtocol> =
    LazyLock::new(|| libp2p::StreamProtocol::new("/libp2p/circuit/relay/0.2.0"));

/// 监听地址列表最大数量（防止恶意 peer 注入大量地址导致 OOM）
const MAX_LISTEN_ADDRS: usize = 64;

/// Relay 候选节点最大数量
const MAX_RELAY_CANDIDATES: usize = 32;

/// DHT 中继节点发现 key — 所有中继节点在此 key 下发布 provider
const DHT_RELAY_INDEX_KEY: &str = "relay_nodes_public";

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
    /// mDNS 缓存刷新间隔
    mdns_refresh_interval: std::time::Duration,
    /// mDNS 缓存
    mdns_cache: lru::LruCache<PeerId, std::time::Instant>,
    /// 通过 relay 连接的 peers
    relay_connections: HashSet<PeerId>,
    /// 当前 Kademlia 模式
    kademlia_mode: Mode,
    /// AutoNAT 状态
    nat_status: autonat::NatStatus,
    /// 本节点所有监听地址（含直连地址和 /p2p-circuit 中继地址）
    listen_addrs: HashSet<libp2p::Multiaddr>,
    /// Relay 节点配置 [(PeerId, Multiaddr)]
    relay_nodes: Vec<(String, String)>,
    /// 是否已向 relay 节点发起拨号
    relay_dialed: bool,
    /// 通过 Identify 自动发现的 relay 候选节点 (PeerId, first_known_addr)
    relay_candidates: Vec<(PeerId, libp2p::Multiaddr)>,
    /// Relay 重连冷却时间（防止无限循环重试）
    relay_reconnect_cooldown_until: Option<std::time::Instant>,
    /// Relay 重连尝试次数（指数退避）
    relay_reconnect_attempt: u32,
    /// 中继服务端是否已启用
    relay_server_enabled: bool,
    /// 是否允许启用中继服务（前端计费网络检测后设置）
    relay_server_allowed: bool,
}

impl P2pActor {
    /// 创建新的 P2pActor
    pub fn new(
        swarm: Swarm<MyBehaviour>,
        dht_cache: Arc<DhtCache>,
        data_dir: std::path::PathBuf,
        event_tx: mpsc::Sender<P2pEvent>,
        relay_nodes: Vec<(String, String)>,
    ) -> Self {
        Self {
            swarm,
            dht_cache,
            event_tx,
            data_dir,
            mdns_refresh_interval: std::time::Duration::from_secs(60 * 60),
            mdns_cache: lru::LruCache::new(std::num::NonZeroUsize::new(2000).unwrap()),
            relay_connections: HashSet::new(),
            kademlia_mode: Mode::Server,
            nat_status: autonat::NatStatus::Unknown,
            listen_addrs: HashSet::new(),
            relay_nodes,
            relay_dialed: false,
            relay_candidates: Vec::new(),
            relay_reconnect_cooldown_until: None,
            relay_reconnect_attempt: 0,
            relay_server_enabled: false,
            relay_server_allowed: false,
        }
    }

    /// 发送事件到 ChatCore
    async fn send_event(&mut self, event: P2pEvent) {
        if let Err(e) = self.event_tx.send(event).await {
            tracing::error!("发送 P2pEvent 失败: {}", e);
        }
    }

    /// 处理单个 swarm 事件
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
                            tracing::info!(
                                "AutoNAT: node is publicly reachable at {:?}",
                                addr
                            );
                            if self.kademlia_mode != Mode::Server {
                                tracing::info!("AutoNAT: switching Kademlia to Server mode (public)");
                                self.swarm.behaviour_mut().kademlia.set_mode(Some(Mode::Server));
                                self.kademlia_mode = Mode::Server;
                            }
                            // 公网节点不需要中继，断开 relay 连接减轻中继压力
                            if self.relay_dialed {
                                self.disconnect_relay_nodes();
                            }
                            // 公网节点自动启用中继服务（向 DHT 注册 + listen /p2p-circuit）
                            self.try_enable_relay_server();
                        }
                        autonat::NatStatus::Private => {
                            tracing::warn!(
                                "AutoNAT: node is behind NAT, switching Kademlia to Client mode"
                            );
                            // 不再是公网节点，关闭中继服务
                            self.disable_relay_server();
                            if self.kademlia_mode != Mode::Client {
                                tracing::info!("AutoNAT: switching Kademlia to Client mode (NATed)");
                                self.swarm.behaviour_mut().kademlia.set_mode(Some(Mode::Client));
                                self.kademlia_mode = Mode::Client;
                            }
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
                message: RequestResponseMessage::Response { response, .. },
                ..
            })) => match response.verify() {
                Ok(true) => tracing::debug!("收到签名响应: timestamp={}", response.timestamp),
                Ok(false) => tracing::warn!("收到无效签名的响应，已忽略"),
                Err(e) => tracing::warn!("验证响应签名时出错: {}", e),
            },

            // --- rr_msg: 出站失败 ---
            SwarmEvent::Behaviour(MyBehaviourEvent::RrMsg(
                RequestResponseEvent::OutboundFailure { peer, error, .. },
            )) => {
                tracing::error!("向 {} 发送消息失败: {:?}", peer, error);
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
                    message: RequestResponseMessage::Response { response, .. },
                    ..
                },
            )) => {
                tracing::debug!("收到 NetEvent 响应: {:?}", response);
            }

            // --- rr_netevent: 出站失败 ---
            SwarmEvent::Behaviour(MyBehaviourEvent::RrNetevent(
                RequestResponseEvent::OutboundFailure { peer, error, .. },
            )) => {
                tracing::error!("向 {} 发送 NetEvent 失败: {:?}", peer, error);
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
                    match self.mdns_cache.get(&peer_id) {
                        Some(last_seen) => {
                            if now.duration_since(*last_seen) >= self.mdns_refresh_interval {
                                self.mdns_cache.put(peer_id, now);
                                self.swarm
                                    .behaviour_mut()
                                    .kademlia
                                    .add_address(&peer_id, multiaddr);
                            } else {
                                continue;
                            }
                        }
                        None => {
                            self.mdns_cache.put(peer_id, now);
                            self.swarm
                                .behaviour_mut()
                                .kademlia
                                .add_address(&peer_id, multiaddr);
                        }
                    }
                    tracing::info!("mDNS discovered: {peer_id}");
                }
            }

            // --- mDNS 过期 ---
            SwarmEvent::Behaviour(MyBehaviourEvent::Mdns(mdns::Event::Expired(list))) => {
                for (peer_id, _multiaddr) in list {
                    tracing::info!("mDNS expired: {peer_id}");
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
                let observed = &info.observed_addr;
                if !observed.iter().any(|p| matches!(p, Protocol::P2pCircuit)) {
                    self.swarm.add_external_address(observed.clone());
                    tracing::info!("Added external address from Identify: {observed}");
                }
                tracing::info!(
                    "Identified {} with {} protocols",
                    peer_id,
                    info.protocols.len()
                );
                // AutoRelay: 检测 peer 是否支持中继 hop 协议
                if info.protocols.contains(&RELAY_HOP_PROTOCOL) {
                    let already_known = self.relay_candidates.iter().any(|(pid, _)| *pid == peer_id);
                    if !already_known && self.relay_candidates.len() < MAX_RELAY_CANDIDATES {
                        if let Some(first_addr) = info.listen_addrs.first() {
                            self.relay_candidates.push((peer_id, first_addr.clone()));
                            tracing::info!("Discovered relay-capable candidate: {} at {}", peer_id, first_addr);
                        }
                        // 如果 NAT 后且尚未连接 relay，立即尝试连接
                        if !self.relay_dialed
                            && matches!(self.nat_status, autonat::NatStatus::Private | autonat::NatStatus::Unknown)
                        {
                            self.dial_relay_nodes();
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
                peer_id,
                endpoint,
                ..
            } => {
                let is_relay = match &endpoint {
                    core::ConnectedPoint::Dialer { address, .. }
                    | core::ConnectedPoint::Listener { send_back_addr: address, .. } => {
                        address.iter().any(|p| matches!(p, Protocol::P2pCircuit))
                    }
                };
                if is_relay {
                    if self.relay_connections.insert(peer_id) {
                        tracing::info!(
                            "Relay connection to {}, DCUtR auto-handled by libp2p",
                            peer_id
                        );
                    }
                    tracing::info!("Relay connection established with {}", peer_id);
                } else {
                    tracing::info!("Direct connection established with {}", peer_id);
                }
                self.send_event(P2pEvent::ConnectionEstablished {
                    peer_id,
                    listen_addrs: self.listen_addrs.iter().cloned().collect(),
                })
                    .await;
            }

            // --- 连接关闭 ---
            SwarmEvent::ConnectionClosed { peer_id, .. } => {
                tracing::info!("Connection closed with {}", peer_id);
                self.relay_connections.remove(&peer_id);
                self.send_event(P2pEvent::ConnectionClosed { peer_id })
                    .await;
            }

            // --- 其他事件（日志/忽略）---
            SwarmEvent::NewListenAddr { address, .. } => {
                if self.listen_addrs.len() < MAX_LISTEN_ADDRS {
                    self.listen_addrs.insert(address.clone());
                }
                if address.iter().any(|p| matches!(p, Protocol::P2pCircuit)) {
                    tracing::info!("Relay listen address: {address}");
                } else {
                    tracing::info!("Listening on: {address}");
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
                tracing::info!("Peer {peer_id} new addr: {address}");
            }
            SwarmEvent::ExpiredListenAddr { address, .. } => {
                self.listen_addrs.remove(&address);
                if address.iter().any(|p| matches!(p, Protocol::P2pCircuit)) {
                    tracing::info!("Relay reservation expired, removed address: {address}");
                    // re-reservation attempt: reset and re-dial
                    self.relay_dialed = false;
                    // 检查冷却时间，防止绕过退避
                    let now = std::time::Instant::now();
                    let in_cooldown = self.relay_reconnect_cooldown_until.is_some_and(|c| now < c);
                    if !in_cooldown && matches!(self.nat_status, autonat::NatStatus::Private | autonat::NatStatus::Unknown) {
                        self.dial_relay_nodes();
                    } else if in_cooldown {
                        tracing::debug!("ExpiredListenAddr: in cooldown, skipping relay re-dial");
                    }
                } else {
                    tracing::info!("Listen address expired: {address}");
                }
            }

            // --- Relay Client 事件 ---
            SwarmEvent::Behaviour(MyBehaviourEvent::RelayClient(
                relay::client::Event::ReservationReqAccepted { relay_peer_id, .. },
            )) => {
                tracing::info!("Relay reservation accepted by relay: {}", relay_peer_id);
                self.relay_reconnect_cooldown_until = None;
                self.relay_reconnect_attempt = 0;
            }
            SwarmEvent::Behaviour(MyBehaviourEvent::RelayClient(
                relay::client::Event::OutboundCircuitEstablished { relay_peer_id, .. },
            )) => {
                tracing::info!("Outbound circuit established via relay: {}", relay_peer_id);
                if self.relay_connections.insert(relay_peer_id) {
                    tracing::info!("Relay connection to {}, DCUtR auto-handled by libp2p", relay_peer_id);
                }
            }
            SwarmEvent::Behaviour(MyBehaviourEvent::RelayClient(event)) => {
                tracing::trace!("Relay client event (unhandled): {:?}", event);
            }

            // --- Relay Server 事件 ---
            SwarmEvent::Behaviour(MyBehaviourEvent::RelayServer(
                relay::Event::ReservationReqAccepted { src_peer_id, .. },
            )) => {
                tracing::info!("Relay server: reservation accepted from {}", src_peer_id);
            }
            SwarmEvent::Behaviour(MyBehaviourEvent::RelayServer(
                relay::Event::ReservationReqDenied { src_peer_id, .. },
            )) => {
                tracing::debug!("Relay server: reservation denied for {}", src_peer_id);
            }
            SwarmEvent::Behaviour(MyBehaviourEvent::RelayServer(event)) => {
                tracing::trace!("Relay server event (unhandled): {:?}", event);
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

            _ => {}
        }
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
                Err(e) => tracing::warn!("Failed to publish DHT record: {:?}", e),
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

                        // 缓存到本地 DHT 缓存
                        let store = &self.dht_cache;
                        for provider in &providers {
                            let _ = store.set_pubkey_peerid(key_str, provider);
                        }

                        // 通知等待的 oneshot callbacks
                        if let Ok(mut callbacks) = DHT_PROVIDER_CALLBACKS.lock() {
                            if let Some(sender) = callbacks.remove(key_str) {
                                if let Some(first_provider) = providers.iter().next() {
                                    let _ = sender.send(*first_provider);
                                }
                            }
                        }

                        // 发送事件给 ChatCore
                        self.send_event(P2pEvent::GetProvidersResult {
                            key: key_str.to_string(),
                            providers: providers.iter().copied().collect(),
                        })
                        .await;

                        // 如果是中继节点发现 key，自动连接发现的 relay
                        if key_str == DHT_RELAY_INDEX_KEY && !self.relay_server_enabled {
                            for provider in providers.iter().take(4) {
                                if !self.relay_connections.contains(provider) {
                                    let pid_str = provider.to_string();
                                    // 通过 Dial 让 libp2p 自动从路由表解析地址
                                    let _ = self.swarm.dial(*provider);
                                    tracing::info!(
                                        "Discovered relay node via DHT: {}, dialing...",
                                        pid_str
                                    );
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

            kad::Event::RoutingUpdated {
                peer,
                is_new_peer,
                addresses,
                old_peer,
                ..
            } => {
                if is_new_peer {
                    tracing::info!(
                        "New peer added to routing table: {} with {} addresses",
                        peer,
                        addresses.len()
                    );
                }
                if let Some(old) = old_peer {
                    tracing::debug!("Peer {} replaced by {} in routing table", old, peer);
                }
            }

            _ => tracing::trace!("Unhandled Kademlia event: {:?}", kad_event),
        }
    }

    /// 向所有 relay 节点发起拨号（NAT 后节点需要中继连接）
    fn dial_relay_nodes(&mut self) {
        // 检查冷却时间，防止无限重试循环
        let now = std::time::Instant::now();
        if let Some(cooldown) = self.relay_reconnect_cooldown_until {
            if now < cooldown {
                tracing::debug!(
                    "Relay reconnect in cooldown, skipping (until {:?})",
                    cooldown
                );
                return;
            }
        }

        let has_configured = !self.relay_nodes.is_empty();
        let has_candidates = !self.relay_candidates.is_empty();
        if !has_configured && !has_candidates {
            return;
        }
        if self.relay_dialed && !has_candidates {
            return;
        }
        self.relay_dialed = true;
        let mut dial_success = false;
        let mut attempted = false;

        // 1. 尝试配置的 relay 节点（静态列表）
        let configured_nodes = self.relay_nodes.clone();
        for (relay_peer_id, relay_addr) in &configured_nodes {
            match relay_addr.parse::<libp2p::Multiaddr>() {
                Ok(addr) => {
                    let has_p2p = addr.iter().any(|p| matches!(p, Protocol::P2p(..)));
                    let full_addr = if has_p2p {
                        addr.clone()
                    } else {
                        match PeerId::from_str(relay_peer_id) {
                            Ok(pid) => addr.clone().with_p2p(pid).unwrap_or(addr.clone()),
                            Err(e) => {
                                tracing::warn!("无效的 relay PeerID '{}': {}", relay_peer_id, e);
                                continue;
                            }
                        }
                    };
                    attempted = true;
                    if self.try_relay_connection(relay_peer_id, &full_addr).is_ok() {
                        dial_success = true;
                    }
                }
                Err(e) => {
                    tracing::warn!("无效的 relay 地址 '{}': {}", relay_addr, e);
                }
            }
        }

        // 2. 尝试通过 Identify 自动发现的 relay 候选节点
        let candidates = self.relay_candidates.clone();
        for (candidate_peer_id, candidate_addr) in &candidates {
            let has_p2p = candidate_addr.iter().any(|p| matches!(p, Protocol::P2p(..)));
            let full_addr = if has_p2p {
                candidate_addr.clone()
            } else {
                candidate_addr
                    .clone()
                    .with_p2p(*candidate_peer_id)
                    .unwrap_or(candidate_addr.clone())
            };
            let peer_id_str = candidate_peer_id.to_string();
            attempted = true;
            if self.try_relay_connection(&peer_id_str, &full_addr).is_ok() {
                dial_success = true;
            }
        }

        // 3. 通过 DHT 查询其他中继节点
        let _ = self.swarm.behaviour_mut().kademlia.get_providers(libp2p::kad::RecordKey::new(&DHT_RELAY_INDEX_KEY));

        if !dial_success && attempted {
            self.relay_reconnect_attempt += 1;
            let backoff_secs = 30u64
                .saturating_mul(2u64.saturating_pow(self.relay_reconnect_attempt.saturating_sub(1)))
                .min(3600);
            let msg = format!(
                "中继连接失败（第{}次）：所有 relay 节点均无法连接，{}s 后重试。NAT 后的节点可能无法被其他节点发现",
                self.relay_reconnect_attempt, backoff_secs
            );
            tracing::warn!("{}", msg);
            self.relay_reconnect_cooldown_until = Some(now + std::time::Duration::from_secs(backoff_secs));
            let _ = self.event_tx.try_send(P2pEvent::Log(format!("relay_warning:{}", msg)));
        } else if dial_success {
            self.relay_reconnect_attempt = 0;
        }
    }

    /// 尝试连接单个 relay 节点：dial + listen_on
    fn try_relay_connection(&mut self, peer_id_str: &str, full_addr: &libp2p::Multiaddr) -> Result<(), String> {
        self.swarm.dial(full_addr.clone()).map_err(|e| format!("dial failed: {}", e))?;
        tracing::info!("Dialed relay node: {}", peer_id_str);
        let relay_listen = full_addr.clone().with(Protocol::P2pCircuit);
        match self.swarm.listen_on(relay_listen) {
            Ok(listener_id) => {
                tracing::info!("Listening on relay circuit at {}, listener={:?}", peer_id_str, listener_id);
            }
            Err(e) => {
                tracing::warn!(
                    "listen_on on relay {} failed (dial already succeeded): {:?}",
                    peer_id_str, e
                );
            }
        }
        Ok(())
    }

    /// 断开所有 relay 节点的连接（公网节点不需要中继）
    fn disconnect_relay_nodes(&mut self) {
        let connected_relays: Vec<PeerId> = self.relay_connections.iter().copied().collect();
        for pid in &connected_relays {
            if let Err(e) = self.swarm.disconnect_peer_id(*pid) {
                tracing::warn!("Failed to disconnect relay {}: {:?}", pid, e);
            } else {
                tracing::info!("Disconnected from relay {}", pid);
            }
        }
        self.relay_connections.clear();
        self.relay_dialed = false;
        self.relay_candidates.clear();
    }

    /// 公网节点自动启用中继服务
    fn try_enable_relay_server(&mut self) {
        if self.relay_server_enabled {
            return;
        }
        if !self.relay_server_allowed {
            tracing::debug!("Relay server not allowed (metered network), skipping");
            return;
        }
        // 在 DHT 注册为中继节点，供 NAT 后节点发现
        let relay_key = libp2p::kad::RecordKey::new(&DHT_RELAY_INDEX_KEY);
        let _ = self.swarm.behaviour_mut().kademlia.start_providing(relay_key);
        // 监听中继电路地址，接收 reservation 请求
        match self.swarm.listen_on("/p2p-circuit".parse().unwrap()) {
            Ok(_) => {
                tracing::info!("Relay server enabled: listening on /p2p-circuit");
                self.relay_server_enabled = true;
            }
            Err(e) => {
                tracing::warn!("Failed to listen on /p2p-circuit: {:?}", e);
            }
        }
    }

    /// 关闭中继服务：DHT 反注册 + 移除 listener
    fn disable_relay_server(&mut self) {
        if !self.relay_server_enabled {
            return;
        }
        let relay_key = libp2p::kad::RecordKey::new(&DHT_RELAY_INDEX_KEY);
        self.swarm.behaviour_mut().kademlia.stop_providing(&relay_key);
        self.relay_server_enabled = false;
        tracing::info!("Relay server disabled (DHT unregistered)");
    }

    /// 处理 P2pActor 控制命令
    async fn handle_command(&mut self, cmd: P2pCommand) {
        match cmd {
            P2pCommand::SendMessage { peer_id, message } => {
                p2p_swarm_ops::send_message(&mut self.swarm, &peer_id, message);
            }
            P2pCommand::SendNetEvent { peer_id, request } => {
                p2p_swarm_ops::send_netevent_request(&mut self.swarm, &peer_id, request);
            }
            P2pCommand::SendNetEventResponse { channel, response } => {
                p2p_swarm_ops::send_netevent_response(&mut self.swarm, channel, response);
            }
            P2pCommand::PublishIdentity {
                mldsa_pubkey_hex,
                mlkem_pubkey_hex,
            } => {
                p2p_swarm_ops::publish_identity_to_dht(
                    &mut self.swarm,
                    &mldsa_pubkey_hex,
                    &mlkem_pubkey_hex,
                );
            }
            P2pCommand::GetProviders { key } => {
                p2p_swarm_ops::get_providers(&mut self.swarm, &key);
            }
            P2pCommand::GetRecord { key } => {
                p2p_swarm_ops::get_record(&mut self.swarm, &key);
            }
            P2pCommand::AddKademliaAddress { peer_id, addr } => {
                p2p_swarm_ops::add_kademlia_address(&mut self.swarm, &peer_id, addr);
            }
            P2pCommand::Dial { peer_id } => {
                p2p_swarm_ops::dial(&mut self.swarm, &peer_id);
            }
            P2pCommand::DialAddr { addr } => {
                p2p_swarm_ops::dial_addr(&mut self.swarm, addr);
            }
            P2pCommand::SendResponse { channel, response } => {
                p2p_swarm_ops::send_response(&mut self.swarm, channel, response);
            }
            P2pCommand::SaveRoutingTable => {
                let cache_path = self.data_dir.join("routing_table.cache");
                p2p::save_routing_table(&mut self.swarm, &cache_path);
            }
            P2pCommand::Shutdown => {
                tracing::info!("P2pActor shutting down...");
                self.disable_relay_server();
            }
            P2pCommand::RelayServerConfig { allowed } => {
                self.relay_server_allowed = allowed;
                if allowed {
                    // 如果当前是公网状态，尝试重新启用中继
                    if matches!(self.nat_status, autonat::NatStatus::Public(_)) {
                        self.try_enable_relay_server();
                    }
                } else {
                    self.disable_relay_server();
                }
            }
        }
    }
}

#[async_trait]
impl Actor for P2pActor {
    type Command = P2pCommand;
    type Event = P2pEvent;

    async fn handle(&mut self, cmd: ActorCommand<Self::Command>) -> Vec<Self::Event> {
        match cmd {
            ActorCommand::Custom(custom_cmd) => {
                self.handle_command(custom_cmd).await;
                vec![]
            }
        }
    }

    async fn on_shutdown(&mut self) -> Vec<Self::Event> {
        tracing::info!("P2pActor shutting down...");
        // 保存路由表
        let cache_path = self.data_dir.join("routing_table.cache");
        p2p::save_routing_table(&mut self.swarm, &cache_path);
        vec![]
    }
}

// ============================================================================
// P2pActorHandle
// ============================================================================

/// P2pActor 的句柄，用于向 P2pActor 发送命令
pub type P2pActorHandle = ActorHandle<ActorCommand<P2pCommand>>;

/// 启动 P2pActor 事件循环
///
/// 在独立线程中运行 P2pActor 的事件循环，处理 swarm 事件和命令。
pub fn start_p2p_actor(
    mut actor: P2pActor,
    channel_size: usize,
    cancellation_token: CancellationToken,
) -> P2pActorHandle {
    let (tx, mut rx) = mpsc::channel::<ActorCommand<P2pCommand>>(channel_size);
    let ct = cancellation_token.clone();

    crate::actor::RUNTIME.spawn(async move {
        // 主事件循环：处理 swarm 事件和命令
        loop {
            tokio::select! {
                // 处理 swarm 事件
                event = actor.swarm.select_next_some() => {
                    actor.handle_swarm_event(event).await;
                }
                // 处理命令
                cmd_opt = rx.recv() => {
                    if let Some(cmd) = cmd_opt {
                        match cmd {
                            ActorCommand::Custom(P2pCommand::Shutdown) => {
                                tracing::info!("P2pActor 收到关闭命令");
                                actor.disable_relay_server();
                                let cache_path = actor.data_dir.join("routing_table.cache");
                                p2p::save_routing_table(&mut actor.swarm, &cache_path);
                                break;
                            }
                            ActorCommand::Custom(custom_cmd) => {
                                actor.handle_command(custom_cmd).await;
                            }
                        }
                    } else {
                        break;
                    }
                }
                // 处理取消信号
                _ = cancellation_token.cancelled() => {
                    tracing::info!("P2pActor 收到取消信号");
                    actor.disable_relay_server();
                    let cache_path = actor.data_dir.join("routing_table.cache");
                    p2p::save_routing_table(&mut actor.swarm, &cache_path);
                    break;
                }
            }
        }
        tracing::info!("P2pActor 事件循环已退出");
    });

    P2pActorHandle {
        tx,
        cancellation_token: ct,
    }
}
