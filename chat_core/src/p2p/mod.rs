use libp2p::kad::{self, Config as KadConfig, Mode, store::MemoryStore};
use libp2p::request_response::{
    Config as rrconfig, Event as RequestResponseEvent, Message as RequestResponseMessage,
    ProtocolSupport, cbor, cbor::codec::Codec,
};
use libp2p::{
    PeerId, StreamProtocol, Swarm, dcutr, identify, mdns, noise, ping, relay,
    swarm::{NetworkBehaviour, SwarmEvent},
    tcp, yamux,
};

use std::num::NonZero;
use std::ops::Deref;
use std::str::FromStr;
use std::time::Duration;
use std::time::Instant;
mod bootstrap;
mod dht;
use crate::{ChatCore, ChatMessage, ChatMessageType, ChatResponse};
/// libp2p 网络行为组合：Gossipsub（消息广播）+ mDNS（局域网发现）
///
/// 设计选择：
/// - Gossipsub: 去中心化消息传播，适合群聊/广播场景
/// - mDNS: 零配置局域网发现，无需中心服务器
#[derive(NetworkBehaviour)]
pub struct MyBehaviour {
    pub rr_msg: cbor::Behaviour<ChatMessage, ChatResponse>,

    /// mDNS 协议：局域网内自动发现对等节点
    mdns: mdns::tokio::Behaviour,
    /// Kademlia 协议：分布式哈希表，用于节点定位和路由
    kademlia: kad::Behaviour<MemoryStore>,
    //Ping 协议（连接保活/延迟检测）
    ping: ping::Behaviour,
    // Identify 协议（地址/协议交换）
    identify: identify::Behaviour,
    // Relay 协议（NAT 穿透，可选）
    relay: relay::Behaviour,
    // DCUtR 协议（直连升级，配合 Relay）
    dcutr: dcutr::Behaviour,
    //rendezvous: rendezvous::client::Behaviour,考虑添加服务申明
}

/// 初始化 libp2p Swarm
///
/// # 网络栈配置
/// - 传输：TCP + Noise 加密 + Yamux 多路复用
/// - 补充：QUIC（原生 TLS 1.3，性能更优）
/// - 发现：mDNS 局域网自动发现
/// - 消息：Gossipsub 广播
use std::sync::LazyLock;

static PROTOCOL_KAD: LazyLock<StreamProtocol> =
    LazyLock::new(|| StreamProtocol::new("/chat/kad/0.0.1"));
static PROTOCOL_MSGRR: LazyLock<StreamProtocol> =
    LazyLock::new(|| StreamProtocol::new("/chat/kad/0.0.1/rr_msg/0.0.1"));
pub fn swarm_init() -> anyhow::Result<Swarm<MyBehaviour>> {
    let mut swarm = libp2p::SwarmBuilder::with_new_identity()
        .with_tokio()
        // TCP 传输层：Noise 加密 + Yamux 流多路复用
        .with_tcp(
            tcp::Config::default(),
            noise::Config::new,     // XX 握手模式
            yamux::Config::default, // 流复用
        )?
        // QUIC 传输层：内置 TLS 1.3，0-RTT，更好 NAT 穿透
        .with_quic()
        // DNS 解析（支持 /dns4, /dns6, /dnsaddr）
        .with_dns()?
        .with_behaviour(|key| {
            let peer_id = key.public().to_peer_id();

            let codec = Codec::<ChatMessage, ChatResponse>::default()
                .set_request_size_maximum(1024 * 1024) // 1MB
                .set_response_size_maximum(64 * 1024); // 64KB

            let rr_msg = cbor::Behaviour::with_codec(
                codec,
                [(PROTOCOL_MSGRR.deref().to_owned(), ProtocolSupport::Full)],
                rrconfig::default(),
            );

            // --- mDNS 配置 ---
            let mdns =
                mdns::tokio::Behaviour::new(mdns::Config::default(), key.public().to_peer_id())?;

            // 创建 Kademlia 行为实例
            let kademlia = create_kademlia(peer_id);
            let identify_config =
                identify::Config::new("/rootcell/identify/1.0.0".to_string(), key.public())
                    .with_agent_version(format!("rootcell/{}", env!("CARGO_PKG_VERSION")))
                    .with_cache_size(100);
            let identify = identify::Behaviour::new(identify_config);
            let ping = ping::Behaviour::new(ping::Config::new());
            let relay = relay::Behaviour::new(key.public().to_peer_id(), Default::default());
            let dcutr = dcutr::Behaviour::new(key.public().to_peer_id());

            Ok(MyBehaviour {
                rr_msg,
                //gossipsub,
                mdns,
                kademlia,
                ping,
                identify,
                relay,
                dcutr,
            })
        })?
        .build();

    // 监听所有接口：IPv4/IPv6 + TCP/QUIC
    // 端口 0 表示系统自动分配
    swarm.listen_on("/ip4/0.0.0.0/udp/0/quic-v1".parse()?)?;
    swarm.listen_on("/ip4/0.0.0.0/tcp/0".parse()?)?;
    swarm.listen_on("/ip6/::/udp/0/quic-v1".parse()?)?;
    swarm.listen_on("/ip6/::/tcp/0".parse()?)?;

    Ok(swarm)
}

/// 创建 Kademlia 行为
fn create_kademlia(peer_id: PeerId) -> kad::Behaviour<MemoryStore> {
    // 存储：内存型（重启丢失），生产可用 PersistentStore
    let store = MemoryStore::new(peer_id);

    // 配置Kademlia网络参数
    let mut config = KadConfig::new(PROTOCOL_KAD.deref().to_owned());
    let _ = &mut config
        .set_query_timeout(Duration::from_secs(60))
        .set_replication_factor(NonZero::new(20).unwrap())
        .set_parallelism(NonZero::new(3).unwrap())
        .set_periodic_bootstrap_interval(Some(Duration::from_secs(300)))
        .set_provider_record_ttl(Some(Duration::from_secs(24 * 60 * 60)))
        .set_publication_interval(Some(Duration::from_secs(60 * 60)));

    let mut kademlia = kad::Behaviour::with_config(peer_id, store, config);
    kademlia.set_mode(Some(Mode::Server));
    for (peerid, addr) in bootstrap::BOOTSTRAP {
        kademlia.add_address(
            &PeerId::from_str(peerid).unwrap(),
            bootstrap::resolve_dnsaddr(addr),
        );
    }
    if let Err(e) = kademlia.bootstrap() {
        tracing::trace!("Bootstrap error: {}", e);
    }
    kademlia
}
const MDNS_REFRESH_INTERVAL: Duration = Duration::from_secs(60 * 60);
/// 处理 Swarm 网络事件
///
/// # 事件分类处理
/// - mDNS：局域网节点发现/过期
/// - Gossipsub：消息接收
/// - 连接管理：建立、关闭、错误
pub async fn swarm_event(event: SwarmEvent<MyBehaviourEvent>, core: &mut ChatCore) {
    match event {
        //request-response
        // 收到请求
        SwarmEvent::Behaviour(MyBehaviourEvent::RrMsg(RequestResponseEvent::Message {
            peer,
            connection_id,
            message:
                RequestResponseMessage::Request {
                    channel,
                    request,
                    request_id,
                },
        })) => {
            println!("收到: {:?} from {}", request, peer);

            let response = ChatResponse {
                timestamp: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_millis() as u64,
            };

            // 发送响应
            if let Err(e) = core
                .swarm
                .behaviour_mut()
                .rr_msg
                .send_response(channel, response)
            {
                eprintln!("发送响应失败: {:?}", e);
            }
        }

        // 收到响应
        SwarmEvent::Behaviour(MyBehaviourEvent::RrMsg(RequestResponseEvent::Message {
            message: RequestResponseMessage::Response { response, .. },
            ..
        })) => {
            println!("响应: {:?}", response);
        }

        // 请求发送失败
        SwarmEvent::Behaviour(MyBehaviourEvent::RrMsg(RequestResponseEvent::OutboundFailure {
            peer,
            error,
            ..
        })) => {
            eprintln!("向 {} 发送失败: {:?}", peer, error);
        }

        // 入站失败
        SwarmEvent::Behaviour(MyBehaviourEvent::RrMsg(RequestResponseEvent::InboundFailure {
            peer,
            error,
            ..
        })) => {
            eprintln!("来自 {} 的入站失败: {:?}", peer, error);
        }

        // 响应已发送
        SwarmEvent::Behaviour(MyBehaviourEvent::RrMsg(RequestResponseEvent::ResponseSent {
            ..
        })) => {
            println!("响应已发送");
        }

        // --- mDNS 发现 ---
        SwarmEvent::Behaviour(MyBehaviourEvent::Mdns(mdns::Event::Discovered(list))) => {
            let now = Instant::now();
            for (peer_id, multiaddr) in list {
                match core.mdns_cache.get(&peer_id) {
                    Some(last_seen) => {
                        if now.duration_since(*last_seen) >= MDNS_REFRESH_INTERVAL {
                            // 间隔足够，更新并处理
                            core.mdns_cache.put(peer_id, now);
                            core.swarm
                                .behaviour_mut()
                                .kademlia
                                .add_address(&peer_id, multiaddr);
                        } else {
                            continue;
                        }
                    }
                    None => {
                        // 首次发现，加入缓存
                        core.mdns_cache.put(peer_id, now);
                        core.swarm
                            .behaviour_mut()
                            .kademlia
                            .add_address(&peer_id, multiaddr);
                    }
                }
                tracing::info!("mDNS discovered: {peer_id}");

                /*// 添加到 Gossipsub
                core.swarm
                    .behaviour_mut()
                    .gossipsub
                    .add_explicit_peer(&peer_id);*/
                // 添加到 Kademlia
            }
        }

        // mDNS 节点过期
        SwarmEvent::Behaviour(MyBehaviourEvent::Mdns(mdns::Event::Expired(list))) => {
            for (peer_id, _multiaddr) in list {
                tracing::info!("mDNS expired: {peer_id}");
                // 从去重缓存中移除，以便下次发现时重新处理
                core.mdns_cache.pop(&peer_id);
            }
        }

        /*// --- Gossipsub 消息 ---
        SwarmEvent::Behaviour(MyBehaviourEvent::Gossipsub(gossipsub::Event::Message {
            propagation_source: peer_id,
            message_id: id,
            message,
        })) => {
            let data: Result<ChatMessage, postcard::Error> = postcard::from_bytes(&message.data);
            if let Ok(data) = data {
                match data.msgtype {
                    ChatMessageType::Text => match String::from_utf8(data.data) {
                        Ok(text) => {
                            core.send_message_mpsc(format!(
                                "From: {} | MsgID: {} | Content: '{}'",
                                peer_id, id, text
                            ))
                            .await;
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, "非法 UTF-8 消息");
                            return;
                        }
                    },

                    ChatMessageType::FileHash => {}
                    _ => {}
                }
            } else {
                core.send_log_mpsc(format!(
                    "From: {} | MsgID: {} | Error: '{}'",
                    peer_id, id, "数据格式错误"
                ))
                .await;
            }
        }*/

        // --- Identify 事件（获取 peer 信息）---
        SwarmEvent::Behaviour(MyBehaviourEvent::Identify(identify::Event::Received {
            peer_id,
            info,
            connection_id: _,
        })) => {
            for multiaddr in info.listen_addrs {
                core.swarm
                    .behaviour_mut()
                    .kademlia
                    .add_address(&peer_id, multiaddr);
            }
            tracing::info!(
                "Identified {} with {} protocols",
                peer_id,
                info.protocols.len()
            );
        }

        // --- 网络状态 ---
        SwarmEvent::NewListenAddr { address, .. } => {
            tracing::info!("Listening on: {address}");
        }

        // 连接建立
        SwarmEvent::ConnectionEstablished { peer_id, .. } => {
            tracing::info!("Connection established with {}", peer_id);
        }

        // 连接关闭
        SwarmEvent::ConnectionClosed { peer_id, .. } => {
            tracing::info!("Connection closed with {}", peer_id);
        }

        // 外拨连接失败
        SwarmEvent::OutgoingConnectionError { peer_id, error, .. } => {
            if let Some(pid) = peer_id {
                tracing::error!("Connect failed to {pid}: {error:?}");
            }
        }

        // 入站连接错误
        SwarmEvent::IncomingConnectionError {
            local_addr, error, ..
        } => {
            tracing::error!("Incoming error on {local_addr:?}: {error:?}");
        }

        // 拨号中
        SwarmEvent::Dialing { peer_id, .. } => {
            tracing::debug!("Dialing: {peer_id:?}");
        }

        // 监听器关闭
        SwarmEvent::ListenerClosed {
            addresses, reason, ..
        } => {
            tracing::warn!("Listener closed: {addresses:?}, reason: {reason:?}");
        }

        // 监听器错误
        SwarmEvent::ListenerError { error, .. } => {
            tracing::error!("Listener error: {error}");
        }

        // 发现对等节点新外部地址
        SwarmEvent::NewExternalAddrOfPeer { peer_id, address } => {
            tracing::info!("Peer {peer_id} new addr: {address}");
        }

        // 监听地址过期
        SwarmEvent::ExpiredListenAddr { address, .. } => {
            tracing::error!("Address expired: {address}");
        }

        // 其他事件
        _ => {}
    }
}
