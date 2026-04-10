use libp2p::request_response::{Event as RequestResponseEvent, Message as RequestResponseMessage};
use libp2p::swarm::SwarmEvent;
use libp2p::{identify, mdns};
use std::time::{Duration, Instant};

use super::behaviour::MyBehaviourEvent;
use crate::{ChatCore, ChatMessageType, ChatResponse, storage};

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
            connection_id: _,
            message:
                RequestResponseMessage::Request {
                    channel,
                    request,
                    request_id: _,
                },
        })) => {
            println!("收到: {:?} from {}", request, peer);

            if let Some(pool) = storage::pool() {
                if let Err(e) = storage::upsert_contact(pool, &peer.to_string(), None).await {
                    tracing::warn!("保存联系人失败: {}", e);
                }

                if let ChatMessageType::Text = request.msgtype {
                    match request.verify(&peer) {
                        Ok(true) => {
                            // 解压缩数据
                            match request.get_decompressed_data() {
                                Ok(decompressed_data) => {
                                    match String::from_utf8(decompressed_data) {
                                        Ok(text) => {
                                            if let Err(e) = storage::add_message(
                                                pool,
                                                &peer.to_string(),
                                                &text,
                                                false,
                                                false,
                                            )
                                            .await
                                            {
                                                tracing::warn!("保存接收消息失败: {}", e);
                                            }
                                            core.send_message_mpsc(text).await;
                                        }
                                        Err(e) => {
                                            tracing::warn!("接收消息不是合法 UTF-8: {}", e);
                                        }
                                    }
                                }
                                Err(e) => {
                                    tracing::warn!("解压缩消息失败: {}", e);
                                }
                            }
                        }
                        Ok(false) => {
                            tracing::warn!("来自 {} 的消息校验失败，已忽略", peer);
                        }
                        Err(e) => {
                            tracing::warn!("验证消息失败: {}", e);
                        }
                    }
                }
            }

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
        SwarmEvent::OutgoingConnectionError { peer_id, error, .. } => match peer_id {
            Some(pid) => {
                tracing::error!("Connect failed to {pid}: {error:?}");
            }
            None => {
                tracing::debug!("Outgoing connection error (no peer id): {error:?}");
            }
        },

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
