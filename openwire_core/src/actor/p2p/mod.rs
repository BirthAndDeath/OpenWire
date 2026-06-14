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

use std::sync::Arc;

use async_trait::async_trait;
use futures::StreamExt;
use libp2p::kad::{self, GetRecordOk, QueryResult};
use libp2p::request_response::{Event as RequestResponseEvent, Message as RequestResponseMessage};
use libp2p::swarm::SwarmEvent;
use libp2p::{dcutr, identify, mdns, relay, PeerId, Swarm};
use redb::Database;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::actor::{Actor, ActorCommand, ActorHandle};
use crate::p2p::behaviour::{MyBehaviour, MyBehaviourEvent};
use crate::p2p::dht::RedbRecordStore;
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
        peer_id: PeerId,
        message: ChatMessage,
    },
    /// 发送 NetEvent 请求
    SendNetEvent {
        peer_id: PeerId,
        request: NetEventRequest,
    },
    /// 发送 NetEvent 响应
    SendNetEventResponse {
        channel: libp2p::request_response::ResponseChannel<NetEventResponse>,
        response: NetEventResponse,
    },
    /// 发布身份到 DHT
    PublishIdentity {
        mldsa_pubkey_hex: String,
        mlkem_pubkey_hex: String,
    },
    /// 发起 GetProviders 查询
    GetProviders {
        key: String,
    },
    /// 发起 GetRecord 查询
    GetRecord {
        key: String,
    },
    /// 添加地址到 Kademlia 路由表
    AddKademliaAddress {
        peer_id: PeerId,
        addr: libp2p::Multiaddr,
    },
    /// 拨号
    Dial {
        peer_id: PeerId,
    },
    /// 拨号到地址
    DialAddr {
        addr: libp2p::Multiaddr,
    },
    /// 发送 rr_msg 响应确认
    SendResponse {
        channel: libp2p::request_response::ResponseChannel<ChatResponse>,
        response: ChatResponse,
    },
    /// 保存路由表
    SaveRoutingTable,
    /// 关闭
    Shutdown,
}

/// P2pActor 发出的事件
#[derive(Debug)]
pub enum P2pEvent {
    /// 收到消息
    MessageReceived {
        peer: PeerId,
        message: ChatMessage,
        channel: libp2p::request_response::ResponseChannel<ChatResponse>,
    },
    /// 收到 NetEvent 请求
    NetEventRequestReceived {
        peer: PeerId,
        request: NetEventRequest,
        channel: libp2p::request_response::ResponseChannel<NetEventResponse>,
    },
    /// 连接建立
    ConnectionEstablished {
        peer_id: PeerId,
    },
    /// 连接关闭
    ConnectionClosed {
        peer_id: PeerId,
    },
    /// mDNS 发现
    MdnsDiscovered {
        peer_id: PeerId,
        addr: libp2p::Multiaddr,
    },
    /// mDNS 过期
    MdnsExpired {
        peer_id: PeerId,
    },
    /// Identify 事件
    IdentifyReceived {
        peer_id: PeerId,
        listen_addrs: Vec<libp2p::Multiaddr>,
    },
    /// Kademlia GetProviders 结果
    GetProvidersResult {
        key: String,
        providers: Vec<PeerId>,
    },
    /// Kademlia GetRecord 结果
    GetRecordResult {
        key: String,
        value: Vec<u8>,
    },
    /// 日志
    Log(String),
}

// ============================================================================
// P2pActor 结构体
// ============================================================================

/// P2P 网络 Actor
///
/// 拥有 Swarm 的所有权，独立运行事件循环。
pub struct P2pActor {
    /// libp2p 网络 swarm
    swarm: Swarm<MyBehaviour>,
    /// DHT 数据库连接
    dht_db: Option<Arc<Database>>,
    /// 事件发送通道（向 ChatCore 发送事件）
    event_tx: mpsc::Sender<P2pEvent>,
    /// 数据目录路径
    data_dir: std::path::PathBuf,
    /// mDNS 缓存刷新间隔
    mdns_refresh_interval: std::time::Duration,
    /// mDNS 缓存
    mdns_cache: lru::LruCache<PeerId, std::time::Instant>,
}

impl P2pActor {
    /// 创建新的 P2pActor
    pub fn new(
        swarm: Swarm<MyBehaviour>,
        dht_db: Option<Arc<Database>>,
        data_dir: std::path::PathBuf,
        event_tx: mpsc::Sender<P2pEvent>,
    ) -> Self {
        Self {
            swarm,
            dht_db,
            event_tx,
            data_dir,
            mdns_refresh_interval: std::time::Duration::from_secs(60 * 60),
            mdns_cache: lru::LruCache::new(
                std::num::NonZeroUsize::new(2000).unwrap(),
            ),
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
            // --- Kademlia 事件 ---
            SwarmEvent::Behaviour(MyBehaviourEvent::Kademlia(kad_event)) => {
                self.handle_kademlia_event(kad_event).await;
            }

            // --- rr_msg: 收到消息请求 ---
            SwarmEvent::Behaviour(MyBehaviourEvent::RrMsg(RequestResponseEvent::Message {
                peer,
                message:
                    RequestResponseMessage::Request {
                        channel,
                        request,
                        ..
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
            })) => {
                match response.verify() {
                    Ok(true) => tracing::debug!("收到签名响应: timestamp={}", response.timestamp),
                    Ok(false) => tracing::warn!("收到无效签名的响应，已忽略"),
                    Err(e) => tracing::warn!("验证响应签名时出错: {}", e),
                }
            }

            // --- rr_msg: 出站失败 ---
            SwarmEvent::Behaviour(MyBehaviourEvent::RrMsg(RequestResponseEvent::OutboundFailure {
                peer,
                error,
                ..
            })) => {
                tracing::error!("向 {} 发送消息失败: {:?}", peer, error);
            }

            // --- rr_msg: 入站失败 ---
            SwarmEvent::Behaviour(MyBehaviourEvent::RrMsg(RequestResponseEvent::InboundFailure {
                peer,
                error,
                ..
            })) => {
                tracing::error!("来自 {} 的入站请求失败: {:?}", peer, error);
            }

            // --- rr_msg: 响应已发送 ---
            SwarmEvent::Behaviour(MyBehaviourEvent::RrMsg(RequestResponseEvent::ResponseSent {
                ..
            })) => {}

            // --- rr_netevent: 收到 NetEvent 请求 ---
            SwarmEvent::Behaviour(MyBehaviourEvent::RrNetevent(RequestResponseEvent::Message {
                peer,
                message:
                    RequestResponseMessage::Request {
                        channel,
                        request,
                        ..
                    },
                ..
            })) => {
                self.send_event(P2pEvent::NetEventRequestReceived {
                    peer,
                    request,
                    channel,
                })
                .await;
            }

            // --- rr_netevent: 收到 NetEvent 响应 ---
            SwarmEvent::Behaviour(MyBehaviourEvent::RrNetevent(RequestResponseEvent::Message {
                message: RequestResponseMessage::Response { response, .. },
                ..
            })) => {
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
                tracing::info!(
                    "Identified {} with {} protocols",
                    peer_id,
                    info.protocols.len()
                );
                self.send_event(P2pEvent::IdentifyReceived {
                    peer_id,
                    listen_addrs: info.listen_addrs,
                })
                .await;
            }

            // --- 连接建立 ---
            SwarmEvent::ConnectionEstablished { peer_id, .. } => {
                tracing::info!("Connection established with {}", peer_id);
                self.send_event(P2pEvent::ConnectionEstablished { peer_id })
                    .await;
            }

            // --- 连接关闭 ---
            SwarmEvent::ConnectionClosed { peer_id, .. } => {
                tracing::info!("Connection closed with {}", peer_id);
                self.send_event(P2pEvent::ConnectionClosed { peer_id }).await;
            }

            // --- 其他事件（日志/忽略）---
            SwarmEvent::NewListenAddr { address, .. } => {
                tracing::info!("Listening on: {address}");
            }
            SwarmEvent::OutgoingConnectionError { peer_id, error, .. } => match peer_id {
                Some(pid) => tracing::error!("Connect failed to {pid}: {error:?}"),
                None => tracing::debug!("Outgoing connection error (no peer id): {error:?}"),
            },
            SwarmEvent::IncomingConnectionError { local_addr, error, .. } => {
                tracing::error!("Incoming error on {local_addr:?}: {error:?}");
            }
            SwarmEvent::Dialing { peer_id, .. } => {
                tracing::debug!("Dialing: {peer_id:?}");
            }
            SwarmEvent::ListenerClosed { addresses, reason, .. } => {
                tracing::warn!("Listener closed: {addresses:?}, reason: {reason:?}");
            }
            SwarmEvent::ListenerError { error, .. } => {
                tracing::error!("Listener error: {error}");
            }
            SwarmEvent::NewExternalAddrOfPeer { peer_id, address } => {
                tracing::info!("Peer {peer_id} new addr: {address}");
            }
            SwarmEvent::ExpiredListenAddr { address, .. } => {
                tracing::error!("Address expired: {address}");
            }

            // --- Relay 事件 ---
            SwarmEvent::Behaviour(MyBehaviourEvent::Relay(relay::Event::ReservationReqAccepted {
                src_peer_id, ..
            })) => {
                tracing::info!("Relay reservation accepted for: {}", src_peer_id);
            }
            SwarmEvent::Behaviour(MyBehaviourEvent::Relay(relay::Event::ReservationTimedOut { src_peer_id })) => {
                tracing::warn!("Relay reservation timed out for: {}", src_peer_id);
            }
            SwarmEvent::Behaviour(MyBehaviourEvent::Relay(event)) => {
                tracing::trace!("Relay event (unhandled): {:?}", event);
            }

            // --- DCUtR 事件 ---
            SwarmEvent::Behaviour(MyBehaviourEvent::Dcutr(dcutr::Event { remote_peer_id, result: Ok(_) })) => {
                tracing::info!("DCUtR direct connection upgraded with: {}", remote_peer_id);
            }
            SwarmEvent::Behaviour(MyBehaviourEvent::Dcutr(dcutr::Event { remote_peer_id, result: Err(ref e) })) => {
                tracing::warn!("DCUtR direct connection upgrade failed for {}: {:?}", remote_peer_id, e);
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
            } => {
                match result {
                    Ok(GetRecordOk::FoundRecord(record)) => {
                        let key_str =
                            std::str::from_utf8(record.record.key.as_ref()).unwrap_or("");
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
                }
            }

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

                        // 缓存到本地 DHT 数据库
                        if let Some(ref db) = self.dht_db {
                            let store = RedbRecordStore::new(db.clone());
                            for provider in &providers {
                                let _ = store.set_pubkey_peerid(key_str, provider);
                            }
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
                                // 保存路由表
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
