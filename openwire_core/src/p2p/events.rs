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
            // 更新在线连接集合
            core.connected_peers.insert(peer_id);
            // 尝试从 DHT 反向查找该 PeerID 对应的 ML-DSA 公钥并缓存到内存
            if !core.peerid_to_pubkey.contains_key(&peer_id) {
                if let Ok(store) = core.get_dht_store() {
                    if let Ok(Some(pubkey_hex)) = store.get_pubkey_by_peerid(&peer_id) {
                        core.peerid_to_pubkey.insert(peer_id, pubkey_hex);
                    }
                }
            }
            // 发送在线状态更新（包含每个在线联系人的 ML-DSA 公钥 hex）
            core.send_online_status().await;
            // 有新连接建立时，尝试重发待发送消息
            let cmd_tx = core.core_handle.cmd_tx.clone();
            let _ = cmd_tx.try_send(ChatCommand::RetryPendingMessages);

            // === 连接建立后主动发布身份到 DHT ===
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
        }

        // 连接关闭
        SwarmEvent::ConnectionClosed { peer_id, .. } => {
            tracing::info!("Connection closed with {}", peer_id);
            core.connected_peers.remove(&peer_id);
            // 清理内存缓存（保留 DHT 持久化映射，下次连接时可重新加载）
            core.peerid_to_pubkey.remove(&peer_id);
            // 发送在线状态更新（包含每个在线联系人的 ML-DSA 公钥 hex）
            core.send_online_status().await;

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
            Ok(kad::GetProvidersOk::FoundProviders { key, providers }) => {
                // GetProviders 成功：缓存 provider 的 PeerID
                // provider key 是 ML-DSA 公钥 hex
                let key_str = std::str::from_utf8(key.as_ref()).unwrap_or("");
                if !key_str.is_empty() && !providers.is_empty() {
                    tracing::debug!(
                        "GetProviders: found {} providers for key {}..",
                        providers.len(),
                        &key_str[..16.min(key_str.len())]
                    );
                    if let Some(store) = open_dht_store(core) {
                        for provider in &providers {
                            let _ = store.set_pubkey_peerid(key_str, provider);
                        }
                    }
                    // 通过 oneshot channel 通知等待的 dht_lookup_peerid
                    if let Ok(mut callbacks) = crate::p2p::DHT_PROVIDER_CALLBACKS.lock() {
                        if let Some(sender) = callbacks.remove(key_str) {
                            // 发送第一个找到的 provider
                            if let Some(first_provider) = providers.iter().next() {
                                let _ = sender.send(*first_provider);
                            }
                        }
                    }
                    // === 自动连接：对每个找到的 provider 主动 dial ===
                    // 当 DHT 查询成功找到 provider 时，说明对方在线。
                    // 主动 dial 建立连接，这样后续的 RetryPendingMessages 可以直接发送消息。
                    // dial 失败是正常的（可能对方暂时不可达，或已建立连接），不影响后续重试。
                    // 直接 dial PeerID，libp2p 会通过 Kademlia routing table 查找地址。
                    for provider in &providers {
                        let dial_result = core.swarm.dial(*provider);
                        match dial_result {
                            Ok(()) => tracing::debug!(
                                "自动连接: 正在 dial provider {}..",
                                &key_str[..16.min(key_str.len())]
                            ),
                            Err(e) => tracing::debug!(
                                "自动连接: dial provider {}.. 失败: {}（可能已连接或正在连接）",
                                &key_str[..16.min(key_str.len())],
                                e
                            ),
                        }
                    }
                    // === 触发离线消息重试 ===
                    // 本地数据库已缓存 pubkey→PeerID 映射，且已发起 dial，
                    // 触发 RetryPendingMessages 重试之前进入离线队列的消息。
                    let cmd_tx = core.core_handle.cmd_tx.clone();
                    let _ = cmd_tx.try_send(ChatCommand::RetryPendingMessages);
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

/// 处理 GetRecord 查询结果
///
/// 只处理 `mlkem:{pubkey_hex}` 格式的记录。
/// PeerID 发现已改用 Kademlia 原生 provider 机制（GetProviders），不再使用 GetRecord。
fn handle_get_record_result(
    result: Result<kad::GetRecordOk, kad::GetRecordError>,
    core: &mut ChatCore,
) {
    match result {
        Ok(GetRecordOk::FoundRecord(record)) => {
            let record_key_str = std::str::from_utf8(record.record.key.as_ref()).unwrap_or("");

            // 只处理 mlkem: 前缀的记录
            if let Some(pubkey_hex) = record_key_str.strip_prefix("mlkem:") {
                // ML-KEM 记录直接存储（无 SignedIdentityRecord 包装）
                let mlkem_hex = match std::str::from_utf8(&record.record.value) {
                    Ok(v) => v.to_string(),
                    Err(_) => {
                        tracing::warn!(
                            "DHT GetRecord: ML-KEM value is not valid UTF-8 for pubkey {}",
                            &pubkey_hex[..16]
                        );
                        return;
                    }
                };

                tracing::info!(
                    "DHT GetRecord: found ML-KEM pubkey for pubkey {}",
                    &pubkey_hex[..16]
                );

                // 写入本地数据库缓存
                if let Some(store) = open_dht_store(core) {
                    let _ = store.set_mlkem_pubkey(pubkey_hex, &mlkem_hex);
                }

                // ML-KEM 公钥已缓存，触发离线消息重试
                // 注意：如果之前消息发送失败是因为缺少 ML-KEM 公钥（而非 PeerID），
                // 现在密钥已缓存，可以重试发送
                let cmd_tx = core.core_handle.cmd_tx.clone();
                let _ = cmd_tx.try_send(ChatCommand::RetryPendingMessages);

                // 异步更新 contacts 表（确保下次发送消息时使用最新密钥）
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
            } else {
                tracing::trace!(
                    "DHT GetRecord: ignoring non-ML-KEM record: {}",
                    record_key_str
                );
            }
        }
        Ok(GetRecordOk::FinishedWithNoAdditionalRecord { .. }) => {
            tracing::debug!("DHT query finished with no additional records");
        }
        Err(e) => tracing::warn!("DHT get record query failed: {:?}", e),
    }
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

    // === 收到消息后，将发送方的 (ML-DSA 公钥 → PeerID) 映射缓存到本地 DHT 数据库 ===
    // 这样当回复消息时，dht_lookup_peerid 的步骤 1（connected_peers 反向查找）能直接命中，
    // 无需等待 DHT 网络查询完成，解决两个在线节点之间 DHT 记录尚未传播时的通信问题。
    if let Ok(store) = core.get_dht_store() {
        let _ = store.set_pubkey_peerid(&sender_mldsa_pubkey_hex, &peer);
        // 同步更新内存缓存，如果该 PeerID 已连接则触发在线状态刷新
        core.update_peerid_pubkey_mapping(peer, sender_mldsa_pubkey_hex.clone())
            .await;
        tracing::debug!(
            "已缓存发送方身份绑定: {}.. -> PeerID={}",
            &sender_mldsa_pubkey_hex[..16],
            peer
        );
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

/// 验证消息签名和新鲜度
///
/// 验证链路：
/// 1. 消息新鲜度（防止重放攻击）
/// 2. 数据完整性（Hash 匹配）
/// 3. ML-DSA 签名有效性
///
/// 注意：不再验证 DHT 身份绑定（verify_with_identity_binding 已删除）。
/// 身份绑定验证在消息发送时通过 Kademlia provider 机制隐式完成：
/// - 发送方通过 start_providing(pubkey_hex) 发布自己的 PeerID
/// - 接收方通过 get_providers(pubkey_hex) 查询发送方的 PeerID
/// - 如果攻击者冒充，其 ML-DSA 签名会验证失败（没有发送方的私钥）
///
/// 返回 true 表示所有验证通过，false 表示任一验证失败
async fn handle_message_verification(
    core: &mut ChatCore,
    request: &ChatMessage,
    peer: &libp2p::PeerId,
) -> bool {
    let sender_pubkey_hex = hex::encode(&request.sender_public_key);

    // 验证消息签名、哈希和新鲜度
    match request.verify() {
        Ok(true) => {
            tracing::debug!(
                "消息验证通过: sender={}.., peer={}",
                &sender_pubkey_hex[..16],
                peer
            );
            true
        }
        Ok(false) => {
            let msg = format!("来自 {} 的消息签名验证失败，已忽略", peer);
            tracing::warn!("{}", msg);
            core.send_warning_mpsc(msg).await;
            false
        }
        Err(e) => {
            let msg = format!("验证来自 {} 的消息签名时出错: {}", peer, e);
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
    _peer: libp2p::PeerId,
    request: &ChatMessage,
    sender_mldsa_pubkey_hex: &str,
) {
    // 获取当前身份的 identity_id（保留用于后续可能的用途）
    let _identity_id = match storage::get_current_identity(pool).await.ok().flatten() {
        Some(id) => id,
        None => {
            tracing::warn!("未找到当前身份");
            return;
        }
    };

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
