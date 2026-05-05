use libp2p::kad::{self, GetRecordOk, QueryResult};
use libp2p::request_response::{Event as RequestResponseEvent, Message as RequestResponseMessage};
use libp2p::swarm::SwarmEvent;
use libp2p::{identify, mdns};
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::behaviour::MyBehaviourEvent;
use super::dht::RedbRecordStore;
use crate::error::{P2pError, P2pResult};
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
            core.send_online_status(count).await;
            // 有新连接建立时，尝试重发待发送消息
            let cmd_tx = core.core_handle.cmd_tx.clone();
            let _ = cmd_tx.try_send(ChatCommand::RetryPendingMessages);

            // === 修复：连接建立后主动发布身份到 DHT ===
            // 确保对方能通过 DHT 查询到我们的最新 PeerID，避免因 DHT 注册延迟导致消息进入离线队列
            if let (Some(pubkey), Some(pid)) = (core.mldsa_pubkey_hex.clone(), core.current_peer_id)
            {
                let mlkem = core.mlkem_pubkey_hex.clone().unwrap_or_default();
                let _ = cmd_tx.try_send(ChatCommand::DhtPublishIdentity {
                    mldsa_pubkey_hex: pubkey,
                    peer_id: pid.to_string(),
                    mlkem_pubkey_hex: mlkem,
                });
            }

            // === 新增：连接建立后向对方发送当前 ML-KEM 公钥 ===
            // 确保对方能立即获取到我们最新的临时加密公钥，避免因公钥过期导致解密失败
            if let Some(ref mlkem_hex) = core.mlkem_pubkey_hex {
                if let Ok(msg) = core
                    .build_signed_message(
                        ChatMessageType::MlkemKeyExchange,
                        mlkem_hex.as_bytes().to_vec(),
                    )
                    .await
                {
                    core.send_message(peer_id, msg);
                    tracing::debug!("已向 {} 发送 ML-KEM 公钥交换消息", peer_id);
                }
            }
        }

        // 连接关闭
        SwarmEvent::ConnectionClosed { peer_id, .. } => {
            tracing::info!("Connection closed with {}", peer_id);
            core.connected_peers.remove(&peer_id);
            let count = core.connected_peers.len();
            core.send_online_status(count).await;

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
///
/// 优先使用 ChatCore 中已打开的共享数据库连接，避免重复打开导致文件锁冲突。
/// 如果共享连接不可用，回退到直接打开文件。
fn open_dht_store(core: &ChatCore) -> Option<RedbRecordStore> {
    // 优先使用共享连接
    if let Some(ref db) = core.dht_db {
        return Some(RedbRecordStore::new(db.clone()));
    }
    // 回退：直接打开文件（仅在 ChatCore 初始化完成前使用）
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
    // 异步更新 contacts 表（确保下次发送消息时使用最新密钥）
    let mlkem_hex = signed.value.clone();
    let pubkey_hex_owned = pubkey_hex.to_string();
    tokio::spawn(async move {
        if let Some(pool) = storage::pool() {
            if let Ok(Some(identity_id)) = storage::get_current_identity(pool).await {
                if let Ok(mlkem_bytes) = hex::decode(&mlkem_hex) {
                    let _ = storage::update_contact_mlkem_pubkey(
                        pool,
                        &identity_id,
                        &pubkey_hex_owned,
                        &mlkem_bytes,
                    )
                    .await;
                    tracing::debug!(
                        "DHT ML-KEM 记录已同步到 contacts 表: {}..",
                        &pubkey_hex_owned[..16]
                    );
                }
            }
        }
    });
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
    let data_preview = if !request.data.is_empty() {
        let preview_len = std::cmp::min(16, request.data.len());
        hex::encode(&request.data[..preview_len])
    } else {
        "empty".to_string()
    };
    tracing::info!(
        "收到: {:?} from {}, data_len={}, data_preview={}, hash_preview={}",
        request.msgtype,
        peer,
        request.data.len(),
        data_preview,
        hex::encode(&request.hash[..std::cmp::min(8, request.hash.len())]),
    );

    let pool = match storage::pool() {
        Some(pool) => pool,
        None => {
            tracing::warn!("数据库连接不可用，无法处理消息");
            return;
        }
    };

    // 从消息中提取发送方的 ML-DSA 公钥
    let sender_mldsa_pubkey_hex = hex::encode(&request.sender_public_key);

    // === MlkemKeyExchange 消息特殊处理 ===
    // ML-KEM 公钥交换消息在连接建立时发送，数据是明文的 hex 字符串。
    // 它必须绕过常规的消息处理流程（DHT 验证 + 解密），因为：
    //   1. 连接已建立，peer_id 已知，不需要 DHT 身份绑定验证
    //   2. 数据是明文 hex，不需要解密（此时双方还没有对方的 ML-KEM 公钥）
    //   3. DHT 查询可能超时（NAT 后的节点），导致密钥交换失败
    if request.msgtype == ChatMessageType::MlkemKeyExchange {
        // 先验证签名（确保消息确实来自声称的发送方）
        match request.verify() {
            Ok(true) => {
                tracing::info!(
                    "ML-KEM 公钥交换消息签名验证通过 from {}..",
                    &sender_mldsa_pubkey_hex[..16]
                );
            }
            Ok(false) => {
                tracing::warn!("ML-KEM 公钥交换消息签名验证失败 from {}", peer);
                send_response(core, channel);
                return;
            }
            Err(e) => {
                tracing::warn!("ML-KEM 公钥交换消息签名验证出错 from {}: {}", peer, e);
                send_response(core, channel);
                return;
            }
        }

        // 获取当前身份的 identity_id
        let owner_identity_id = match storage::get_current_identity(pool).await.ok().flatten() {
            Some(id) => id,
            None => {
                tracing::warn!("未找到当前身份，无法处理 ML-KEM 公钥交换消息");
                send_response(core, channel);
                return;
            }
        };

        // 直接处理 ML-KEM 公钥交换（跳过解密和 DHT 验证）
        // 传入 peer 参数，以便缓存发送方的 PeerID 绑定到 DHT 本地数据库
        handle_mlkem_key_exchange(
            core,
            pool,
            &request,
            &sender_mldsa_pubkey_hex,
            &owner_identity_id,
            &peer,
        )
        .await;
        send_response(core, channel);
        return;
    }

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
    handle_decrypted_message(core, pool, peer, &request, &sender_mldsa_pubkey_hex).await;

    // 发送响应确认
    send_response(core, channel);

    // 对于文本消息，发送送达回执
    if request.msgtype == ChatMessageType::Text {
        // 使用 ChatMessage 自身的 hash 字段作为回执内容，
        // 与发送方 save_pending_message_with_hash 中使用的哈希一致。
        let receipt_data = hex::encode(&request.hash);

        // 通过 DHT 查找发送方的 PeerID 和 ML-KEM 公钥，并发回加密的回执
        if let Ok(store) = core.get_dht_store() {
            if let Ok(Some(sender_peer_id)) = store.get_peerid_by_pubkey(&sender_mldsa_pubkey_hex) {
                // 获取发送方的 ML-KEM 公钥，用于加密回执数据
                let sender_mlkem_pubkey = match store.get_mlkem_pubkey(&sender_mldsa_pubkey_hex) {
                    Ok(Some(hex_str)) if !hex_str.is_empty() => match hex::decode(&hex_str) {
                        Ok(key) => key,
                        Err(e) => {
                            tracing::warn!("发送方 ML-KEM 公钥 hex 解码失败: {}", e);
                            return;
                        }
                    },
                    _ => {
                        tracing::warn!(
                            "未找到发送方 {} 的 ML-KEM 公钥，无法加密送达回执",
                            &sender_mldsa_pubkey_hex[..16]
                        );
                        return;
                    }
                };

                // 用发送方的 ML-KEM 公钥加密回执数据
                let encrypted_receipt =
                    match crypto::encrypt_message(receipt_data.as_bytes(), &sender_mlkem_pubkey) {
                        Ok(data) => data,
                        Err(e) => {
                            tracing::warn!("加密送达回执失败: {}", e);
                            return;
                        }
                    };

                let receipt_msg = match core
                    .build_signed_message(ChatMessageType::DeliveryReceipt, encrypted_receipt)
                    .await
                {
                    Ok(msg) => msg,
                    Err(e) => {
                        tracing::warn!("构建送达回执消息失败: {}", e);
                        return;
                    }
                };
                core.send_message(sender_peer_id, receipt_msg);
                tracing::info!("已向 {} 发送加密的送达回执", &sender_mldsa_pubkey_hex[..16]);
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

    let sender_pubkey_hex = hex::encode(&request.sender_public_key);

    match request.verify_with_identity_binding(&store, Some(peer)) {
        Ok(true) => {
            tracing::debug!(
                "消息验证通过: sender={}.., peer={}",
                &sender_pubkey_hex[..16],
                peer
            );
            return true;
        }
        Ok(false) => {
            // 基础验证（签名/哈希）可能已通过，但 DHT 身份绑定未找到。
            // 不立即拒绝，而是尝试通过 DHT 网络查询对方的身份绑定记录。
            tracing::info!(
                "消息验证初次失败（可能 DHT 绑定未同步），尝试通过 DHT 网络查询 sender={}.. 的身份绑定",
                &sender_pubkey_hex[..16]
            );
        }
        Err(e) => {
            let msg = format!("验证来自 {} 的消息时出错: {}", peer, e);
            tracing::warn!("{}", msg);
            core.send_warning_mpsc(msg).await;
            return false;
        }
    }

    // === 修复：DHT 绑定未找到时，发起网络查询并重试 ===
    // 先检查签名本身是否有效（如果签名无效，DHT 查询也无意义）
    match request.verify() {
        Ok(true) => {} // 签名有效，继续
        Ok(false) => {
            let msg = format!("来自 {} 的消息签名验证失败，已忽略", peer);
            tracing::warn!("{}", msg);
            core.send_warning_mpsc(msg).await;
            return false;
        }
        Err(e) => {
            let msg = format!("验证来自 {} 的消息签名时出错: {}", peer, e);
            tracing::warn!("{}", msg);
            core.send_warning_mpsc(msg).await;
            return false;
        }
    }

    // 发起 DHT 网络查询，获取发送方的 PeerID 绑定记录
    let record_key = format!("peerid:{}", sender_pubkey_hex);
    let key = libp2p::kad::RecordKey::new(&record_key);
    let rx = super::register_dht_query_callback(sender_pubkey_hex.clone());
    let _query_id = core.swarm.behaviour_mut().kademlia.get_record(key);

    tracing::info!(
        "已发起 DHT 网络查询 sender={}.. 的身份绑定，等待结果...",
        &sender_pubkey_hex[..16]
    );

    // 等待 DHT 查询结果（超时 10 秒）
    let dht_result = tokio::time::timeout(std::time::Duration::from_secs(10), rx).await;

    // 清理回调（防止泄漏）
    let _ = super::dht_query_callbacks()
        .lock()
        .unwrap()
        .remove(&sender_pubkey_hex);

    match dht_result {
        Ok(Ok(Some(found_peer_id))) => {
            tracing::info!(
                "DHT 网络查询成功: sender={}.. -> PeerID={}",
                &sender_pubkey_hex[..16],
                found_peer_id
            );

            // 验证查询到的 PeerID 是否与消息来源一致
            if &found_peer_id != peer {
                tracing::warn!(
                    "DHT 查询到的 PeerID {} 与消息来源 {} 不匹配，消息验证失败",
                    found_peer_id,
                    peer
                );
                let msg = format!("来自 {} 的消息验证失败：DHT 身份绑定与消息来源不匹配", peer);
                core.send_warning_mpsc(msg).await;
                return false;
            }

            // 重新获取 DHT store 并再次验证（此时本地已有缓存）
            match core.get_dht_store() {
                Ok(store2) => match request.verify_with_identity_binding(&store2, Some(peer)) {
                    Ok(true) => {
                        tracing::info!(
                            "DHT 查询后重试验证通过: sender={}.., peer={}",
                            &sender_pubkey_hex[..16],
                            peer
                        );
                        true
                    }
                    Ok(false) => {
                        tracing::warn!(
                            "DHT 查询后重试验证仍然失败: sender={}..",
                            &sender_pubkey_hex[..16]
                        );
                        let msg = format!("来自 {} 的消息验证失败（DHT 绑定不一致），已忽略", peer);
                        core.send_warning_mpsc(msg).await;
                        false
                    }
                    Err(e) => {
                        tracing::warn!("DHT 查询后重试验证出错: {}", e);
                        let msg = format!("验证来自 {} 的消息时出错: {}", peer, e);
                        core.send_warning_mpsc(msg).await;
                        false
                    }
                },
                Err(e) => {
                    tracing::warn!("DHT 查询后获取 store 失败: {}", e);
                    false
                }
            }
        }
        Ok(Ok(None)) => {
            tracing::warn!(
                "DHT 网络查询未找到 sender={}.. 的身份绑定记录",
                &sender_pubkey_hex[..16]
            );
            let msg = format!(
                "来自 {} 的消息验证失败：DHT 网络中未找到发送方的身份绑定记录",
                peer
            );
            core.send_warning_mpsc(msg).await;
            false
        }
        Ok(Err(_)) => {
            tracing::warn!(
                "DHT 网络查询 sender={}.. 的回调通道已关闭",
                &sender_pubkey_hex[..16]
            );
            let msg = format!("来自 {} 的消息验证失败：DHT 查询被取消", peer);
            core.send_warning_mpsc(msg).await;
            false
        }
        Err(_) => {
            tracing::warn!(
                "DHT 网络查询 sender={}.. 超时（10秒），消息验证失败",
                &sender_pubkey_hex[..16]
            );
            let msg = format!("来自 {} 的消息验证失败：DHT 网络查询超时", peer);
            core.send_warning_mpsc(msg).await;
            false
        }
    }
}

/// 解密消息并按 msgtype 分发处理
async fn handle_decrypted_message(
    core: &mut ChatCore,
    pool: &sqlx::Pool<sqlx::Sqlite>,
    peer: libp2p::PeerId,
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

    // === MlkemKeyExchange 消息特殊处理 ===
    // ML-KEM 公钥交换消息在连接建立时发送，数据是明文的 hex 字符串（非加密数据）。
    // 因为此时双方还没有对方的 ML-KEM 公钥，无法加密，所以必须跳过解密步骤。
    if request.msgtype == ChatMessageType::MlkemKeyExchange {
        handle_mlkem_key_exchange(
            core,
            pool,
            request,
            sender_mldsa_pubkey_hex,
            &identity_id,
            &peer,
        )
        .await;
        return;
    }

    // 使用 ChatCore 中缓存的 DecapsulationKey 对象解密消息
    // 注意：不能通过序列化/反序列化私钥字节来重建 DecapsulationKey，
    // 因为 aws-lc-rs 的 key_bytes() 输出格式与 DecapsulationKey::new() 输入格式不兼容。
    // 解决方案是在 ChatCore 中缓存 DecapsulationKey 对象，直接传入引用。
    let decap_key = match &core.mlkem_decap_key {
        Some(key) => key,
        None => {
            tracing::warn!("ML-KEM 解封装密钥未初始化，无法解密消息");
            return;
        }
    };

    // 解密消息
    let decrypted_data = match crypto::decrypt_message(&request.data, decap_key) {
        Ok(data) => data,
        Err(e) => {
            // 输出详细的数据诊断信息
            let data_len = request.data.len();
            let data_preview = if data_len > 0 {
                let preview_len = std::cmp::min(16, data_len);
                hex::encode(&request.data[..preview_len])
            } else {
                "empty".to_string()
            };
            tracing::warn!(
                "解密失败诊断: msgtype={:?}, data_len={}, data_preview={}, sender={}.., error={}",
                request.msgtype,
                data_len,
                data_preview,
                &sender_mldsa_pubkey_hex[..16],
                e
            );
            let msg = format!(
                "解密来自 {} 的消息失败: {}。对方的 ML-KEM 公钥可能已过期（双方重启后密钥会更新），正在尝试发送当前 ML-KEM 公钥给对方以修复旧密钥...",
                &sender_mldsa_pubkey_hex[..16],
                e
            );
            tracing::warn!("{}", msg);
            core.send_warning_mpsc(msg).await;

            if let Some(ref mlkem_hex) = core.mlkem_pubkey_hex {
                match core
                    .build_signed_message(
                        ChatMessageType::MlkemKeyExchange,
                        mlkem_hex.as_bytes().to_vec(),
                    )
                    .await
                {
                    Ok(exchange_msg) => {
                        core.send_message(peer, exchange_msg);
                        tracing::info!("向 {} 发送 ML-KEM 公钥交换消息以修复可能的旧密钥", peer);
                    }
                    Err(err) => {
                        tracing::warn!("构建 ML-KEM 公钥交换消息失败: {}", err);
                    }
                }
            } else {
                tracing::warn!("当前本地 ML-KEM 公钥不可用，无法发送 ML-KEM 公钥交换消息");
            }
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
        ChatMessageType::MlkemKeyExchange => {
            // 不会执行到这里，已在函数开头提前处理
            unreachable!("MlkemKeyExchange 应在函数开头提前处理");
        }
    }
}

/// 处理 ML-KEM 公钥交换消息（明文数据，无需解密）
///
/// 除了更新 ML-KEM 公钥外，还会将发送方的 PeerID 绑定缓存到 DHT 本地数据库，
/// 这样后续 send_text_impl 可以直接从本地数据库找到对方的 PeerID，
/// 无需再次发起 DHT 网络查询。
async fn handle_mlkem_key_exchange(
    core: &mut ChatCore,
    pool: &sqlx::Pool<sqlx::Sqlite>,
    request: &ChatMessage,
    sender_mldsa_pubkey_hex: &str,
    identity_id: &str,
    peer: &libp2p::PeerId,
) {
    let data = &request.data;
    tracing::info!(
        "收到 ML-KEM 公钥交换消息 from {}.., data_len={}, peer={}",
        &sender_mldsa_pubkey_hex[..16],
        data.len(),
        peer
    );

    // === 缓存发送方的 PeerID 绑定到 DHT 本地数据库 ===
    // 这是关键修复：当收到 MlkemKeyExchange 消息时，连接已建立，peer 已知。
    // 将 peer->pubkey 绑定缓存到 DHT 本地数据库，后续 send_text_impl
    // 可以直接从本地数据库找到对方的 PeerID，无需 DHT 网络查询。
    if let Ok(store) = core.get_dht_store() {
        let _ = store.set_pubkey_peerid(sender_mldsa_pubkey_hex, peer);
        tracing::info!(
            "已缓存 {}.. 的 PeerID 绑定: {}（通过 ML-KEM 密钥交换）",
            &sender_mldsa_pubkey_hex[..16],
            peer
        );
    }

    // ML-KEM 公钥交换消息的数据是明文的 hex 字符串
    match String::from_utf8(data.clone()) {
        Ok(mlkem_hex) => {
            tracing::info!(
                "ML-KEM 公钥交换消息内容: {}..",
                &mlkem_hex[..std::cmp::min(16, mlkem_hex.len())]
            );
            // 解码 ML-KEM 公钥
            if let Ok(mlkem_bytes) = hex::decode(&mlkem_hex) {
                // 更新 contacts 表
                let _ = storage::update_contact_mlkem_pubkey(
                    pool,
                    identity_id,
                    sender_mldsa_pubkey_hex,
                    &mlkem_bytes,
                )
                .await;
                // 更新 DHT 本地数据库
                if let Ok(store) = core.get_dht_store() {
                    let _ = store.set_mlkem_pubkey(sender_mldsa_pubkey_hex, &mlkem_hex);
                }
                tracing::info!(
                    "已更新联系人 {}.. 的 ML-KEM 公钥（通过连接建立时的密钥交换）",
                    &sender_mldsa_pubkey_hex[..16]
                );
            } else {
                tracing::warn!(
                    "ML-KEM 公钥交换消息内容不是合法 hex: {}..",
                    &mlkem_hex[..std::cmp::min(16, mlkem_hex.len())]
                );
            }
        }
        Err(e) => {
            tracing::warn!("ML-KEM 公钥交换消息不是合法 UTF-8: {}", e);
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
fn build_signed_response(core: &ChatCore) -> P2pResult<ChatResponse> {
    let mldsa_private_key = core
        .mldsa_private_key
        .as_ref()
        .ok_or(P2pError::MlDsaPrivateKeyNotCached)?;
    let mldsa_public_key =
        crate::identity::extract_public_key_from_private(mldsa_private_key, true)
            .map_err(|e| P2pError::SwarmInitFailed(e.into()))?;
    ChatResponse::new_signed(mldsa_private_key, &mldsa_public_key)
        .map_err(|e| P2pError::SwarmInitFailed(e.into()))
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
