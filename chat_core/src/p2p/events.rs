use libp2p::kad::{self, GetRecordOk, QueryResult};
use libp2p::request_response::{Event as RequestResponseEvent, Message as RequestResponseMessage};
use libp2p::swarm::SwarmEvent;
use libp2p::{identify, mdns};
use std::time::{Duration, Instant};

use super::behaviour::MyBehaviourEvent;
use crate::{ChatCore, ChatMessageType, ChatResponse, crypto, storage};

const MDNS_REFRESH_INTERVAL: Duration = Duration::from_secs(60 * 60);

/// 处理 Swarm 网络事件
///
/// # 事件分类处理
/// - mDNS：局域网节点发现/过期
/// - 连接管理：建立、关闭、错误
/// - Kademlia：DHT 记录验证和管理
pub async fn swarm_event(event: SwarmEvent<MyBehaviourEvent>, core: &mut ChatCore) {
    match event {
        // Kademlia 事件处理
        SwarmEvent::Behaviour(MyBehaviourEvent::Kademlia(kad_event)) => {
            handle_kademlia_event(kad_event, core);
        }
        
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
            tracing::info!("收到: {:?} from {}", request.msgtype, peer);

            if let Some(pool) = storage::pool() {
                // 首先检查发送方是否是已添加的联系人（好友）
                match storage::is_contact_exists(pool, &peer.to_string()).await {
                    Ok(true) => {
                        tracing::debug!("消息来自已知联系人: {}", peer);
                    }
                    Ok(false) => {
                        tracing::warn!("收到来自未知用户 {} 的消息，已拒绝", peer);
                        // 仍然发送响应，但不处理消息内容
                        let response = ChatResponse {
                            timestamp: std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap()
                                .as_millis() as u64,
                        };
                        if let Err(e) = core
                            .swarm
                            .behaviour_mut()
                            .rr_msg
                            .send_response(channel, response)
                        {
                            eprintln!("发送响应失败: {:?}", e);
                        }
                        return;
                    }
                    Err(e) => {
                        tracing::error!("检查联系人状态失败: {}", e);
                        // 出错时保守处理，不接收消息
                        let response = ChatResponse {
                            timestamp: std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap()
                                .as_millis() as u64,
                        };
                        if let Err(e) = core
                            .swarm
                            .behaviour_mut()
                            .rr_msg
                            .send_response(channel, response)
                        {
                            eprintln!("发送响应失败: {:?}", e);
                        }
                        return;
                    }
                }

                // 保存或更新联系人（包含公钥）- 仅在确认是好友后更新公钥
                if let Err(e) = storage::upsert_contact(
                    pool,
                    &peer.to_string(),
                    None,
                    Some(&request.sender_public_key),
                )
                .await
                {
                    tracing::warn!("更新联系人公钥失败: {}", e);
                }

                if let ChatMessageType::Text = request.msgtype {
                    match request.verify(&peer) {
                        Ok(true) => {
                            // 解压缩数据
                            match request.get_decompressed_data() {
                                Ok(compressed_data) => {
                                    // 获取当前身份的公钥
                                    if let Some((peer_id_str, _public_key_bytes)) =
                                        storage::get_current_mlkem_identity(pool)
                                            .await
                                            .ok()
                                            .flatten()
                                    {
                                        // 从安全存储中获取私钥
                                        match rootcell::identity::PrivateKeyHandle::load(&core.data_dir, &peer_id_str)
                                        {
                                            Ok(private_key_handle) => {
                                                let private_key_bytes = private_key_handle.get_private_key();
                                                // 解密消息（使用私钥）
                                                match crypto::decrypt_message(
                                                    &compressed_data,
                                                    &private_key_bytes,
                                                ) {
                                                    Ok(decrypted_data) => {
                                                        match String::from_utf8(decrypted_data) {
                                                            Ok(text) => {
                                                                if let Err(e) =
                                                                    storage::add_message(
                                                                        pool,
                                                                        &peer.to_string(),
                                                                        &text,
                                                                        false,
                                                                        false,
                                                                    )
                                                                    .await
                                                                {
                                                                    tracing::warn!(
                                                                        "保存接收消息失败: {}",
                                                                        e
                                                                    );
                                                                }
                                                                core.send_message_mpsc(text).await;
                                                            }
                                                            Err(e) => {
                                                                tracing::warn!(
                                                                    "解密后的消息不是合法 UTF-8: {}",
                                                                    e
                                                                );
                                                            }
                                                        }
                                                    }
                                                    Err(e) => {
                                                        tracing::warn!("解密消息失败: {}", e);
                                                    }
                                                }
                                            }
                                            Err(e) => {
                                                tracing::warn!("获取 ML-KEM 私钥失败: {}", e);
                                            }
                                        }
                                    } else {
                                        tracing::warn!("未找到当前 ML-KEM 身份");
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
        
        // 其他事件（忽略）
        _ => {}
    }
}

/// 处理 Kademlia 事件
fn handle_kademlia_event(kad_event: kad::Event, core: &mut ChatCore) {
    match kad_event {
        // 记录查询结果
        kad::Event::OutboundQueryProgressed {
            result: QueryResult::GetRecord(result),
            ..
        } => {
            match result {
                Ok(GetRecordOk::FoundRecord(record)) => {
                    // 验证找到的记录
                    if let Some(publisher) = &record.record.publisher {
                        let mut validator = core.validator.write().unwrap();
                        
                        // 使用 validator 验证记录
                        let record_size = record.record.value.len();
                        if !validator.validate_dht_record(
                            publisher,
                            record_size,
                            &record.record.key,
                        ) {
                            tracing::warn!(
                                "Rejected DHT record from {}: validation failed",
                                publisher
                            );
                            return;
                        }
                        
                        tracing::debug!(
                            "Validated DHT record from {} (size: {} bytes)",
                            publisher,
                            record_size
                        );
                    }
                }
                Ok(GetRecordOk::FinishedWithNoAdditionalRecord { .. }) => {
                    tracing::debug!("DHT query finished with no additional records");
                }
                Err(e) => {
                    tracing::warn!("DHT get record query failed: {:?}", e);
                }
            }
        }
        
        // 记录发布事件
        kad::Event::OutboundQueryProgressed {
            result: QueryResult::PutRecord(result),
            ..
        } => {
            match result {
                Ok(ok) => {
                    tracing::debug!("Successfully published DHT record to {:?}", ok.key);
                }
                Err(e) => {
                    tracing::warn!("Failed to publish DHT record: {:?}", e);
                }
            }
        }
        
        // 提供者查询
        kad::Event::OutboundQueryProgressed {
            result: QueryResult::GetProviders(result),
            ..
        } => {
            match result {
                Ok(ok) => {
                    tracing::debug!("Found providers: {:?}", ok);
                }
                Err(e) => {
                    tracing::warn!("Get providers query failed: {:?}", e);
                }
            }
        }
        
        // 路由表更新
        kad::Event::RoutingUpdated {
            peer,
            is_new_peer,
            addresses,
            old_peer,
            ..
        } => {
            if is_new_peer {
                tracing::info!("New peer added to routing table: {} with {} addresses", peer, addresses.len());
            }
            if let Some(old) = old_peer {
                tracing::debug!("Peer {} replaced by {} in routing table", old, peer);
            }
        }
        
        // 其他 Kademlia 事件
        _ => {
            tracing::trace!("Unhandled Kademlia event: {:?}", kad_event);
        }
    }
}
