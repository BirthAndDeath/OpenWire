use libp2p::kad::{self, GetRecordOk, QueryResult};
use libp2p::request_response::{Event as RequestResponseEvent, Message as RequestResponseMessage};
use libp2p::swarm::SwarmEvent;
use libp2p::{identify, mdns};
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::behaviour::MyBehaviourEvent;
use super::dht::RedbRecordStore;
use crate::{ChatCommand, ChatCore, ChatMessage, ChatMessageType, ChatResponse, crypto, storage};

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
            handle_incoming_request(core, peer, channel, request).await;
        }

        // 收到响应（验证签名）
        SwarmEvent::Behaviour(MyBehaviourEvent::RrMsg(RequestResponseEvent::Message {
            message: RequestResponseMessage::Response { response, .. },
            ..
        })) => match response.verify() {
            Ok(true) => {
                tracing::debug!("收到签名响应: timestamp={}", response.timestamp);
            }
            Ok(false) => {
                tracing::warn!("收到无效签名的响应，已忽略");
            }
            Err(e) => {
                tracing::warn!("验证响应签名时出错: {}", e);
            }
        },

        // 请求发送失败
        SwarmEvent::Behaviour(MyBehaviourEvent::RrMsg(RequestResponseEvent::OutboundFailure {
            peer,
            error,
            ..
        })) => {
            tracing::error!("向 {} 发送消息失败: {:?}", peer, error);
        }

        // 入站失败
        SwarmEvent::Behaviour(MyBehaviourEvent::RrMsg(RequestResponseEvent::InboundFailure {
            peer,
            error,
            ..
        })) => {
            tracing::error!("来自 {} 的入站请求失败: {:?}", peer, error);
        }

        // 响应已发送
        SwarmEvent::Behaviour(MyBehaviourEvent::RrMsg(RequestResponseEvent::ResponseSent {
            ..
        })) => {
            tracing::debug!("响应已发送");
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

        // 连接建立 - 触发离线消息重试并更新在线状态
        SwarmEvent::ConnectionEstablished { peer_id, .. } => {
            tracing::info!("Connection established with {}", peer_id);
            // 更新在线连接计数
            core.connected_peers.insert(peer_id);
            let count = core.connected_peers.len();
            core.send_message_mpsc(crate::command::IncomingMessage::OnlineStatus { count })
                .await;
            // 有新连接建立时，尝试重发待发送消息
            let cmd_tx = core.core_handle.cmd_tx.clone();
            let _ = cmd_tx.try_send(ChatCommand::RetryPendingMessages);
        }

        // 连接关闭
        SwarmEvent::ConnectionClosed { peer_id, .. } => {
            tracing::info!("Connection closed with {}", peer_id);
            core.connected_peers.remove(&peer_id);
            let count = core.connected_peers.len();
            core.send_message_mpsc(crate::command::IncomingMessage::OnlineStatus { count })
                .await;

            // === Re-dial on connection closed ===
            // Look up known multiaddrs from DHT and attempt to re-establish the connection.
            // If dial fails, just log and continue without blocking the event loop.
            if let Ok(store) = core.get_dht_store()
                && let Ok(addrs) = store.get_multiaddrs(&peer_id)
                && !addrs.is_empty()
            {
                tracing::info!(
                    "连接关闭：尝试重拨 {}，发现 {} 个已知地址",
                    peer_id,
                    addrs.len()
                );
                for addr in &addrs {
                    let dial_addr = addr.clone().with_p2p(peer_id).unwrap_or(addr.clone());
                    match core.swarm.dial(dial_addr) {
                        Ok(()) => tracing::info!("重拨 {} 地址: {}", peer_id, addr),
                        Err(e) => {
                            tracing::debug!("重拨 {} 地址 {} 失败: {}", peer_id, addr, e)
                        }
                    }
                }
            }
            // === 重拨结束 ===
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

/// 打开本地 DHT 数据库并创建 RecordStore
fn open_dht_store(core: &ChatCore) -> Option<RedbRecordStore> {
    let dht_path = core.data_dir.join("dht.redb");
    let db = redb::Database::create(&dht_path).ok()?;
    Some(RedbRecordStore::new(Arc::new(db)))
}

/// 处理 Kademlia 事件
fn handle_kademlia_event(kad_event: kad::Event, core: &mut ChatCore) {
    match kad_event {
        kad::Event::OutboundQueryProgressed {
            result: QueryResult::GetRecord(result),
            ..
        } => handle_get_record_result(result, core),

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
            Ok(ok) => tracing::debug!("Found providers: {:?}", ok),
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

/// 处理 GetRecord 查询结果
fn handle_get_record_result(
    result: Result<kad::GetRecordOk, kad::GetRecordError>,
    core: &mut ChatCore,
) {
    match result {
        Ok(GetRecordOk::FoundRecord(record)) => {
            let record_key_str = std::str::from_utf8(record.record.key.as_ref()).unwrap_or("");

            if handle_peerid_record(record_key_str, &record.record, core) {
                return;
            }
            if handle_mlkem_record(record_key_str, &record.record, core) {
                return;
            }

            // 非 pubkey->PeerID/ML-KEM 查询，走签名验证逻辑
            handle_signed_record(&record.record, core);
        }
        Ok(GetRecordOk::FinishedWithNoAdditionalRecord { .. }) => {
            tracing::debug!("DHT query finished with no additional records");
        }
        Err(e) => tracing::warn!("DHT get record query failed: {:?}", e),
    }
}

/// 处理 "peerid:{pubkey}" 格式的 DHT 记录
///
/// 记录值使用 postcard 序列化的 SignedIdentityRecord，
/// 验证签名通过后才信任该记录。
fn handle_peerid_record(key_str: &str, record: &libp2p::kad::Record, core: &ChatCore) -> bool {
    let Some(pubkey_hex) = key_str.strip_prefix("peerid:") else {
        return false;
    };

    // 反序列化 SignedIdentityRecord
    let signed: crate::signature::SignedIdentityRecord = match postcard::from_bytes(&record.value) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(
                "DHT GetRecord: failed to deserialize signed PeerID record for pubkey {}: {}",
                &pubkey_hex[..16],
                e
            );
            super::complete_dht_query(pubkey_hex, None);
            return false;
        }
    };

    // 验证签名
    let mldsa_pubkey_bytes = match hex::decode(pubkey_hex) {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(
                "DHT GetRecord: invalid pubkey hex for {}: {}",
                &pubkey_hex[..16],
                e
            );
            super::complete_dht_query(pubkey_hex, None);
            return false;
        }
    };
    let publisher: libp2p::PeerId = match signed.publisher.parse() {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(
                "DHT GetRecord: invalid publisher PeerID in signed record for pubkey {}: {}",
                &pubkey_hex[..16],
                e
            );
            super::complete_dht_query(pubkey_hex, None);
            return false;
        }
    };
    match signed.verify(
        &mldsa_pubkey_bytes,
        key_str.as_bytes(),
        &publisher,
        3_600_000,
    ) {
        Ok(true) => {} // 签名验证通过
        Ok(false) | Err(_) => {
            tracing::warn!(
                "DHT GetRecord: signature verification failed for PeerID record of pubkey {}",
                &pubkey_hex[..16]
            );
            super::complete_dht_query(pubkey_hex, None);
            return false;
        }
    }

    // 解析 PeerID 值
    match signed.value.parse::<libp2p::PeerId>().ok() {
        Some(peer_id) => {
            tracing::info!(
                "DHT GetRecord: found signed PeerID {} for pubkey {}",
                peer_id,
                &pubkey_hex[..16]
            );
            if let Some(store) = open_dht_store(core) {
                let _ = store.set_pubkey_peerid(pubkey_hex, &peer_id);
            }
            super::complete_dht_query(pubkey_hex, Some(peer_id));
        }
        None => {
            tracing::warn!(
                "DHT GetRecord: invalid PeerID value in signed record for pubkey {}",
                &pubkey_hex[..16]
            );
            super::complete_dht_query(pubkey_hex, None);
        }
    }
    true
}

/// 处理 "mlkem:{pubkey}" 格式的 DHT 记录
///
/// 记录值使用 postcard 序列化的 SignedIdentityRecord，
/// 验证签名通过后才信任该记录。
fn handle_mlkem_record(key_str: &str, record: &libp2p::kad::Record, core: &ChatCore) -> bool {
    let Some(pubkey_hex) = key_str.strip_prefix("mlkem:") else {
        return false;
    };

    // 反序列化 SignedIdentityRecord
    let signed: crate::signature::SignedIdentityRecord = match postcard::from_bytes(&record.value) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(
                "DHT GetRecord: failed to deserialize signed ML-KEM record for pubkey {}: {}",
                &pubkey_hex[..16],
                e
            );
            complete_mlkem_callback(pubkey_hex, None);
            return false;
        }
    };

    // 验证签名
    let mldsa_pubkey_bytes = match hex::decode(pubkey_hex) {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(
                "DHT GetRecord: invalid pubkey hex for {}: {}",
                &pubkey_hex[..16],
                e
            );
            complete_mlkem_callback(pubkey_hex, None);
            return false;
        }
    };
    let publisher: libp2p::PeerId = match signed.publisher.parse() {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(
                "DHT GetRecord: invalid publisher PeerID in signed ML-KEM record for pubkey {}: {}",
                &pubkey_hex[..16],
                e
            );
            complete_mlkem_callback(pubkey_hex, None);
            return false;
        }
    };
    match signed.verify(
        &mldsa_pubkey_bytes,
        key_str.as_bytes(),
        &publisher,
        3_600_000,
    ) {
        Ok(true) => {} // 签名验证通过
        Ok(false) | Err(_) => {
            tracing::warn!(
                "DHT GetRecord: signature verification failed for ML-KEM record of pubkey {}",
                &pubkey_hex[..16]
            );
            complete_mlkem_callback(pubkey_hex, None);
            return false;
        }
    }

    tracing::info!(
        "DHT GetRecord: found signed ML-KEM pubkey for pubkey {}",
        &pubkey_hex[..16]
    );
    // 先通过 oneshot channel 发送 ML-KEM hex 值，消除竞态条件
    // 调用方可以直接使用该值，无需等待数据库写入完成
    complete_mlkem_callback(pubkey_hex, Some(&signed.value));
    // 再写入本地数据库缓存（供后续离线查询使用）
    if let Some(store) = open_dht_store(core) {
        let _ = store.set_mlkem_pubkey(pubkey_hex, &signed.value);
    }
    true
}

/// 完成 ML-KEM 查询回调
fn complete_mlkem_callback(pubkey_hex: &str, mlkem_hex: Option<&str>) {
    // 兼容 lookup_peerid_by_pubkey_network 的回调
    super::complete_dht_query(pubkey_hex, None);

    let query_id = format!("mlkem_{}", pubkey_hex);
    if let Some(tx) = crate::p2p::mlkem_query_callbacks()
        .lock()
        .unwrap()
        .remove(&query_id)
    {
        let _ = tx.send(mlkem_hex.map(|s| s.to_string()));
    }
}

/// 处理带签名的 DHT 记录（签名验证）
fn handle_signed_record(record: &libp2p::kad::Record, core: &mut ChatCore) {
    let Some(publisher) = &record.publisher else {
        return;
    };

    let sig_meta = core
        .swarm
        .behaviour_mut()
        .kademlia
        .store_mut()
        .inner()
        .get_record_signature(&record.key);

    let sig = match sig_meta {
        Some(s) => s,
        None => {
            tracing::warn!(
                "DHT record from {} has no signature metadata, rejecting",
                publisher
            );
            return;
        }
    };

    let mut validator = core.validator.write().unwrap();
    if !validator.validate_dht_record(crate::p2p::validator::DhtRecordValidationParams {
        publisher,
        key: &record.key,
        record_value: &record.value,
        signature: &sig.signature,
        timestamp: sig.timestamp,
        salt: &sig.salt,
    }) {
        tracing::warn!("Rejected DHT record from {}: validation failed", publisher);
        return;
    }

    tracing::debug!(
        "Validated DHT record from {} (size: {} bytes)",
        publisher,
        record.value.len()
    );
}

// ========== 消息接收处理 ==========

/// 处理入站请求消息
///
/// 流程：验证发送者 → 验证签名 → 解密 → 按 msgtype 分发
async fn handle_incoming_request(
    core: &mut ChatCore,
    peer: libp2p::PeerId,
    channel: libp2p::request_response::ResponseChannel<ChatResponse>,
    request: ChatMessage,
) {
    tracing::info!("收到: {:?} from {}", request.msgtype, peer);

    let pool = match storage::pool() {
        Some(pool) => pool,
        None => {
            tracing::warn!("数据库连接不可用，无法处理消息");
            return;
        }
    };

    // 从消息中提取发送方的 ML-DSA 公钥
    let sender_mldsa_pubkey_hex = hex::encode(&request.sender_public_key);

    // 获取当前身份的 identity_id
    let owner_identity_id = match storage::get_current_identity(pool).await.ok().flatten() {
        Some(id) => id,
        None => {
            tracing::warn!("未找到当前身份，无法处理入站消息");
            send_response(core, channel);
            return;
        }
    };

    // 检查是否是已添加的联系人
    let is_known_contact =
        storage::is_contact_exists(pool, &owner_identity_id, &sender_mldsa_pubkey_hex)
            .await
            .unwrap_or(false);

    if !is_known_contact {
        tracing::warn!("收到来自未知用户 {} 的消息，已拒绝", peer);
        send_response(core, channel);
        return;
    }

    // 验证消息签名
    if !handle_message_verification(core, &request, &peer).await {
        return;
    }

    // 解密并处理消息
    handle_decrypted_message(core, pool, &request, &sender_mldsa_pubkey_hex).await;

    // 发送响应确认
    send_response(core, channel);

    // 对于文本消息，发送送达回执
    if request.msgtype == ChatMessageType::Text {
        // 使用消息哈希作为回执内容，让发送方标记对应的待发送消息
        let receipt_data = {
            let hash_input = format!(
                "{}:{}",
                sender_mldsa_pubkey_hex,
                String::from_utf8_lossy(&request.data)
            );
            let mut hasher = sha2::Sha256::new();
            use sha2::Digest;
            hasher.update(hash_input.as_bytes());
            hex::encode(hasher.finalize())
        };

        // 通过 DHT 查找发送方的 PeerID 并发回回执
        if let Ok(store) = core.get_dht_store() {
            if let Ok(Some(sender_peer_id)) = store.get_peerid_by_pubkey(&sender_mldsa_pubkey_hex) {
                let receipt_msg = match core
                    .build_signed_message(
                        ChatMessageType::DeliveryReceipt,
                        receipt_data.into_bytes(),
                    )
                    .await
                {
                    Ok(msg) => msg,
                    Err(e) => {
                        tracing::warn!("构建送达回执消息失败: {}", e);
                        return;
                    }
                };
                core.send_message(sender_peer_id, receipt_msg);
                tracing::info!("已向 {} 发送送达回执", &sender_mldsa_pubkey_hex[..16]);
            } else {
                tracing::debug!(
                    "未找到发送方 {} 的 PeerID，无法发送送达回执",
                    &sender_mldsa_pubkey_hex[..16]
                );
            }
        }
    }
}

/// 验证消息签名、新鲜度以及 DHT 身份绑定
///
/// 使用 verify_with_identity_binding() 进行完整验证链路：
/// 1. 消息新鲜度（防止重放攻击）
/// 2. 数据完整性（Hash 匹配）
/// 3. ML-DSA 签名有效性
/// 4. sender_public_key 在 DHT 中的身份绑定记录
/// 5. 消息来源 PeerID 与 DHT 绑定一致
///
/// 返回 true 表示所有验证通过，false 表示任一验证失败
async fn handle_message_verification(
    core: &mut ChatCore,
    request: &ChatMessage,
    peer: &libp2p::PeerId,
) -> bool {
    // 获取 DHT store 进行身份绑定验证
    let store = match core.get_dht_store() {
        Ok(s) => s,
        Err(e) => {
            let msg = format!("获取 DHT store 失败，无法验证消息身份绑定: {}", e);
            tracing::warn!("{}", msg);
            core.send_warning_mpsc(msg).await;
            return false;
        }
    };

    match request.verify_with_identity_binding(&store, Some(peer)) {
        Ok(true) => {
            let sender_hex = hex::encode(&request.sender_public_key);
            tracing::debug!(
                "消息验证通过: sender={}.., peer={}",
                &sender_hex[..16],
                peer
            );
            true
        }
        Ok(false) => {
            let msg = format!(
                "来自 {} 的消息验证失败（签名/哈希/身份绑定不匹配），已忽略",
                peer
            );
            tracing::warn!("{}", msg);
            core.send_warning_mpsc(msg).await;
            false
        }
        Err(e) => {
            let msg = format!("验证来自 {} 的消息时出错: {}", peer, e);
            tracing::warn!("{}", msg);
            core.send_warning_mpsc(msg).await;
            false
        }
    }
}

/// 解密消息并按 msgtype 分发处理
async fn handle_decrypted_message(
    core: &mut ChatCore,
    pool: &sqlx::Pool<sqlx::Sqlite>,
    request: &ChatMessage,
    sender_mldsa_pubkey_hex: &str,
) {
    // 获取当前身份的 identity_id
    let identity_id = match storage::get_current_identity(pool).await.ok().flatten() {
        Some(id) => id,
        None => {
            tracing::warn!("未找到当前身份");
            return;
        }
    };

    // 从安全存储中获取 ML-KEM 私钥
    let private_key_handle = match rootcell::identity::PrivateKeyHandle::load(
        &core.data_dir.to_string_lossy(),
        &format!("{}_mlkem", identity_id),
        None,
    ) {
        Ok(handle) => handle,
        Err(e) => {
            tracing::warn!("获取 ML-KEM 私钥失败: {}", e);
            return;
        }
    };

    let private_key_bytes = private_key_handle.get_private_key();

    // 解密消息
    let decrypted_data = match crypto::decrypt_message(&request.data, private_key_bytes) {
        Ok(data) => data,
        Err(e) => {
            let msg = format!(
                "解密来自 {} 的消息失败: {}。可能是对方的 ML-KEM 公钥已过期，请让对方重新添加你为好友以交换新密钥。",
                &sender_mldsa_pubkey_hex[..16],
                e
            );
            tracing::warn!("{}", msg);
            core.send_warning_mpsc(msg).await;
            return;
        }
    };

    // 按 msgtype 分发
    match request.msgtype {
        ChatMessageType::Text => {
            handle_text_message(core, pool, sender_mldsa_pubkey_hex, decrypted_data).await;
        }
        ChatMessageType::FileHash => {
            handle_file_hash_message(core, sender_mldsa_pubkey_hex, decrypted_data).await;
        }
        ChatMessageType::FileStream => {
            handle_file_stream_message(core, decrypted_data).await;
        }
        ChatMessageType::FileDownloadRequest => {
            handle_file_download_request(core, sender_mldsa_pubkey_hex, decrypted_data).await;
        }
        ChatMessageType::DeliveryReceipt => {
            handle_delivery_receipt(core, pool, decrypted_data).await;
        }
    }
}

/// 处理文本消息：UTF-8 解码 → 存储 → 通知 UI
async fn handle_text_message(
    core: &mut ChatCore,
    pool: &sqlx::Pool<sqlx::Sqlite>,
    sender_mldsa_pubkey_hex: &str,
    data: Vec<u8>,
) {
    match String::from_utf8(data) {
        Ok(text) => {
            // 使用消息内容哈希进行去重（结合发送方和内容）
            let hash_input = format!("{}:{}", sender_mldsa_pubkey_hex, text);
            let message_hash = {
                let mut hasher = sha2::Sha256::new();
                use sha2::Digest;
                hasher.update(hash_input.as_bytes());
                hex::encode(hasher.finalize())
            };

            // 获取当前身份的 identity_id
            let owner_identity_id = match storage::get_current_identity(pool).await.ok().flatten() {
                Some(id) => id,
                None => {
                    tracing::warn!("未找到当前身份，无法保存接收的消息");
                    return;
                }
            };

            match storage::add_message_with_hash(
                pool,
                &owner_identity_id,
                sender_mldsa_pubkey_hex,
                &text,
                false,
                false,
                &message_hash,
            )
            .await
            {
                Ok(Some(_id)) => {
                    // 新消息，正常处理
                }
                Ok(None) => {
                    // 重复消息，跳过
                    tracing::debug!("跳过重复消息: {}", &text[..text.len().min(50)]);
                    return;
                }
                Err(e) => {
                    tracing::warn!("保存接收消息失败: {}", e);
                }
            }

            // 发送结构化消息（枚举），上层负责序列化为 JSON
            core.send_message_mpsc(crate::command::IncomingMessage::Text {
                text,
                sender: sender_mldsa_pubkey_hex.to_string(),
            })
            .await;
        }
        Err(e) => {
            tracing::warn!("解密后的消息不是合法 UTF-8: {}", e);
        }
    }
}

/// 处理文件哈希消息：解析 FileHashInfo → 通知 UI（包含结构化数据供前端渲染可点击下载）
async fn handle_file_hash_message(
    core: &mut ChatCore,
    sender_mldsa_pubkey_hex: &str,
    data: Vec<u8>,
) {
    match postcard::from_bytes::<crate::message::FileHashInfo>(&data) {
        Ok(file_info) => {
            tracing::info!(
                "收到文件哈希分享: file_id={:?}, filename={}, size={}, hash={:?}",
                file_info.file_id,
                file_info.filename,
                file_info.total_size,
                file_info.file_hash,
            );

            // 发送结构化消息（枚举），上层负责序列化为 JSON
            let file_id_hex = hex::encode(file_info.file_id);
            let file_hash_hex = hex::encode(file_info.file_hash);
            core.send_message_mpsc(crate::command::IncomingMessage::FileShare {
                filename: file_info.filename,
                file_id: file_id_hex,
                file_hash: file_hash_hex,
                total_size: file_info.total_size,
                sender: sender_mldsa_pubkey_hex.to_string(),
            })
            .await;
        }
        Err(e) => {
            tracing::warn!("解析 FileHashInfo 失败: {}", e);
        }
    }
}

/// 处理文件流消息：解析 FileStreamChunk → 写入文件
///
/// 注意：data 是序列化后的 FileStreamChunk（未压缩），
/// 内部 chunk_data 字段已在 from_file() 中压缩，
/// 解压缩由 FileStreamChunk::decompress_to_file() 处理
async fn handle_file_stream_message(core: &mut ChatCore, data: Vec<u8>) {
    match postcard::from_bytes::<crate::message::FileStreamChunk>(&data) {
        Ok(chunk) => {
            tracing::info!(
                "收到文件分片: file_id={:?}, chunk={}/{}, filename={}",
                chunk.file_id,
                chunk.chunk_index,
                chunk.total_chunks,
                chunk.filename,
            );
            // 写入文件
            if let Err(e) = core.handle_file_stream_chunk(chunk).await {
                tracing::warn!("写入文件分片失败: {}", e);
            }
        }
        Err(e) => {
            tracing::warn!("解析 FileStreamChunk 失败: {}", e);
        }
    }
}

/// 处理文件下载请求（发送方收到接收方的下载请求）
/// 解析 ChunkResponse → 查找文件 → 根据已接收分片列表跳过已发送分片
/// 使用 FileStreamChunk::from_file 只发送缺失的分片
///
/// 断点续传支持：
/// - 接收方在 ChunkResponse 中携带已接收的分片序号列表
/// - 发送方跳过这些分片，只发送缺失的分片
async fn handle_file_download_request(
    core: &mut ChatCore,
    sender_mldsa_pubkey_hex: &str,
    data: Vec<u8>,
) {
    // 解析 ChunkResponse（携带已接收分片列表）
    let chunk_response: crate::message::ChunkResponse = match postcard::from_bytes(&data) {
        Ok(resp) => resp,
        Err(e) => {
            tracing::warn!("解析 ChunkResponse 失败: {}", e);
            return;
        }
    };

    let file_id_hex = hex::encode(chunk_response.file_id);
    let received_chunks: std::collections::HashSet<u32> =
        chunk_response.received_chunks.iter().copied().collect();

    tracing::info!(
        "收到文件下载请求: file_id={}.., 已接收 {}/{} 分片",
        &file_id_hex[..16],
        received_chunks.len(),
        "?"
    );

    // 查找文件路径
    let file_path = match core.file_path_map.get(&chunk_response.file_id) {
        Some(path) => path.clone(),
        None => {
            tracing::warn!("未找到 file_id {}.. 对应的文件路径", &file_id_hex[..16]);
            return;
        }
    };

    // === 直连协商：在发送文件分片前，尝试与接收方建立直连 ===
    // 文件传输数据量大，如果当前连接经过 relay，大量分片通过 relay 中转会导致性能瓶颈。
    // 通过 DHT 获取接收方的多地址并主动 dial，尝试建立直连（含 NAT 穿透）。
    // dial() 是同步入队操作，libp2p 后台异步处理连接建立；
    // 后续 rr_msg.send_request() 会自动利用新建立的直连发送分片。
    if let Ok(store) = core.get_dht_store() {
        if let Ok(Some(recipient_peer_id)) = store.get_peerid_by_pubkey(sender_mldsa_pubkey_hex) {
            if !core.swarm.is_connected(&recipient_peer_id) {
                if let Ok(addrs) = store.get_multiaddrs(&recipient_peer_id)
                    && !addrs.is_empty()
                {
                    tracing::info!(
                        "文件传输：尝试与 {}.. 建立直连，发现 {} 个地址",
                        &sender_mldsa_pubkey_hex[..16],
                        addrs.len()
                    );
                    for addr in &addrs {
                        let dial_addr = addr
                            .clone()
                            .with_p2p(recipient_peer_id)
                            .unwrap_or(addr.clone());
                        match core.swarm.dial(dial_addr) {
                            Ok(()) => tracing::info!(
                                "文件传输：正在 dial {}.. 地址: {}",
                                &sender_mldsa_pubkey_hex[..16],
                                addr
                            ),
                            Err(e) => tracing::debug!("文件传输：dial {} 失败: {}", addr, e),
                        }
                    }
                }
            } else {
                tracing::debug!(
                    "文件传输：与 {}.. 已建立连接，无需额外 dial",
                    &sender_mldsa_pubkey_hex[..16]
                );
            }
        } else {
            tracing::debug!(
                "文件传输：未找到 {}.. 的 PeerID，跳过直连协商",
                &sender_mldsa_pubkey_hex[..16]
            );
        }
    }
    // === 直连协商结束 ===

    // 检查文件是否存在
    if !file_path.exists() {
        tracing::warn!("文件不存在: {:?}", file_path);
        return;
    }

    // 获取文件元信息
    let metadata = match tokio::fs::metadata(&file_path).await {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!("获取文件元信息失败: {:?}: {}", file_path, e);
            return;
        }
    };
    let file_size = metadata.len();

    // 计算分片参数
    // 使用固定分片大小 256KB
    let chunk_size: u32 = 256 * 1024; // 256KB 固定分片
    let total_chunks = file_size.div_ceil(chunk_size as u64) as u32;

    // 计算文件哈希（用于验证完整性）
    let file_hash = match crate::transfer::compute_file_hash(&file_path).await {
        Ok(hash) => hash,
        Err(e) => {
            tracing::warn!("计算文件哈希失败: {}", e);
            return;
        }
    };

    let filename = file_path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "unknown".to_string());

    // 根据消息类型选择压缩等级（FileStream 使用 zstd level=3）
    let compression_level = crate::compression::compression_level(ChatMessageType::FileStream);

    tracing::info!(
        "开始发送文件: {}, size={}, chunks={}, chunk_size={}, compression_level={}, 已接收={}",
        filename,
        file_size,
        total_chunks,
        chunk_size,
        compression_level,
        received_chunks.len(),
    );

    // 打开文件
    let mut file = match tokio::fs::File::open(&file_path).await {
        Ok(f) => f,
        Err(e) => {
            tracing::warn!("打开文件失败: {:?}: {}", file_path, e);
            return;
        }
    };

    // 逐分片读取并发送（使用 FileStreamChunk::from_file）
    // 跳过接收方已接收的分片（断点续传）
    let mut sent_count = 0u32;
    for chunk_index in 0..total_chunks {
        // 断点续传：如果接收方已接收此分片，跳过
        if received_chunks.contains(&chunk_index) {
            tracing::debug!("跳过已接收分片 {}/{}", chunk_index + 1, total_chunks);
            continue;
        }

        let offset = chunk_index as u64 * chunk_size as u64;

        let config = crate::message::ChunkReadConfig {
            file_id: chunk_response.file_id,
            filename: filename.clone(),
            total_size: file_size,
            total_chunks,
            chunk_size,
            chunk_index,
            offset,
            file_hash,
        };

        let (chunk, bytes_read) =
            match crate::message::FileStreamChunk::from_file(&mut file, &config, compression_level)
                .await
            {
                Ok(result) => result,
                Err(e) => {
                    tracing::warn!("读取文件分片 {} 失败: {}", chunk_index, e);
                    return;
                }
            };

        // 序列化 FileStreamChunk（包含压缩后的 chunk_data）
        let chunk_data = match postcard::to_allocvec(&chunk) {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!("序列化 FileStreamChunk 失败: {}", e);
                return;
            }
        };

        // 发送 FileStream 消息
        // 注意：FileStreamChunk.chunk_data 已经是压缩后的数据（在 from_file() 中压缩）
        // 整个 FileStreamChunk 序列化后发送，prepare_data 对 FileStream 类型不再压缩
        // （避免双重压缩：chunk_data 已压缩，外层不应再压缩）
        if let Err(e) = core
            .send_text(
                sender_mldsa_pubkey_hex,
                ChatMessageType::FileStream,
                chunk_data,
            )
            .await
        {
            tracing::warn!("发送文件分片 {} 失败: {}", chunk_index, e);
            return;
        }

        sent_count += 1;

        tracing::debug!(
            "已发送分片 {}/{} (offset={}, size={})",
            chunk_index + 1,
            total_chunks,
            offset,
            bytes_read,
        );

        if chunk.is_last {
            break;
        }

        // 小延迟避免拥塞
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    tracing::info!(
        "文件发送完成: {}, total_chunks={}, sent_chunks={}, skipped_chunks={}",
        filename,
        total_chunks,
        sent_count,
        received_chunks.len(),
    );

    // 清理 file_path_map 中的条目，防止内存泄漏
    // 文件发送完成后，不再需要保留文件路径映射
    core.file_path_map.remove(&chunk_response.file_id);
}

/// 发送签名响应确认
fn send_response(
    core: &mut ChatCore,
    channel: libp2p::request_response::ResponseChannel<ChatResponse>,
) {
    let response = match build_signed_response(core) {
        Ok(resp) => resp,
        Err(e) => {
            tracing::error!("构建签名响应失败: {}，无法发送响应", e);
            return;
        }
    };
    if let Err(e) = core
        .swarm
        .behaviour_mut()
        .rr_msg
        .send_response(channel, response)
    {
        tracing::error!("发送响应失败: {:?}", e);
    }
}

/// 构建带 ML-DSA 签名的 ChatResponse
fn build_signed_response(core: &ChatCore) -> anyhow::Result<ChatResponse> {
    let mldsa_private_key = core
        .mldsa_private_key
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("ML-DSA private key not cached"))?;
    let mldsa_public_key =
        crate::identity::extract_public_key_from_private(mldsa_private_key, true)?;
    ChatResponse::new_signed(mldsa_private_key, &mldsa_public_key)
}

/// 处理消息送达回执：将对应的待发送消息标记为已发送，并通知 UI
async fn handle_delivery_receipt(
    core: &mut ChatCore,
    pool: &sqlx::Pool<sqlx::Sqlite>,
    data: Vec<u8>,
) {
    // 回执数据格式：原始消息的 message_hash（SHA256 hex）
    match String::from_utf8(data) {
        Ok(receipt_msg_hash) => {
            tracing::info!("收到送达回执，消息哈希: {}", &receipt_msg_hash[..16]);

            // 查找并标记对应的待发送消息为已发送
            match storage::list_pending(pool).await {
                Ok(pending_msgs) => {
                    for msg in &pending_msgs {
                        if let Some(ref hash) = msg.message_hash
                            && hash == &receipt_msg_hash
                        {
                            if let Err(e) = storage::mark_sent(pool, msg.id).await {
                                tracing::warn!("标记消息 {} 为已发送失败: {}", msg.id, e);
                            } else {
                                tracing::info!("消息 {} 已通过送达回执标记为已发送", msg.id);
                                // 通知 UI 消息已送达
                                core.send_message_mpsc(
                                    crate::command::IncomingMessage::DeliveryReceipt {
                                        message_hash: receipt_msg_hash.clone(),
                                        peer_id: msg.peer_pubkey_hex.clone(),
                                    },
                                )
                                .await;
                            }
                            break;
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!("查询待发送消息列表失败: {}", e);
                }
            }
        }
        Err(e) => {
            tracing::warn!("送达回执数据不是合法 UTF-8: {}", e);
        }
    }
}
