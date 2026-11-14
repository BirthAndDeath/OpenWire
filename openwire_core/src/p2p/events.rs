use crate::actor::p2p::P2pCommand;
use crate::error::{P2pError, P2pResult};
use crate::{ChatCore, ChatMessage, ChatMessageType, ChatResponse, crypto, storage};
use sha2::Digest;
use std::sync::Arc;
use tokio::sync::mpsc;
use zeroize::Zeroizing;

// ========== 消息接收处理 ==========

/// 处理入站请求消息
///
/// 流程：验证发送者 → 验证签名 → 解密 → 按 msgtype 分发
///
/// # 设计说明
/// 消息处理（解密、数据库写入、送达回执）在当前 async 上下文中顺序执行。
/// 不 spawn 到后台任务，因为需要 &mut ChatCore 的独占访问。
/// send_response 在解密之前执行，确保发送方尽快收到响应确认，
/// 避免 request-response 协议超时重试。
pub async fn handle_incoming_request(
    core: &mut ChatCore,
    peer: libp2p::PeerId,
    channel: libp2p::request_response::ResponseChannel<ChatResponse>,
    request: ChatMessage,
) {
    tracing::debug!(
        "收到: {:?} from {}, data_len={}",
        request.msgtype,
        peer,
        request.data.len(),
    );

    let pool = match storage::pool() {
        Some(pool) => pool,
        None => {
            tracing::warn!("数据库连接不可用，无法处理消息");
            send_response(core, channel, None).await;
            return;
        }
    };

    // 从消息中提取发送方的 ML-DSA 公钥
    let sender_mldsa_pubkey_hex = hex::encode(&request.sender_public_key);

    // 获取当前身份的 identity_id
    let Some(owner_identity_id) = storage::current_identity_id(pool).await else {
        tracing::warn!("未找到当前身份，无法处理入站消息");
        send_response(core, channel, None).await;
        return;
    };

    // 每 peer 入站消息滑窗限流：超限释放流槽，保护 ML-DSA 验签与 SQLite 写入
    // 不被已连接对端的高频消息冲刷
    if core.inbound_message_rate_limited(&peer) {
        tracing::warn!("入站消息速率超限，拒绝来自 {} 的消息", peer);
        send_response(core, channel, None).await;
        return;
    }

    // 先验证消息签名（轻量操作，无需数据库），防止非联系人消耗数据库资源
    if !handle_message_verification(core, &request, &peer).await {
        return;
    }

    // 签名验证通过后再检查联系人（需要数据库查询）
    let is_known_contact =
        storage::is_contact_exists(pool, &owner_identity_id, &sender_mldsa_pubkey_hex)
            .await
            .unwrap_or(false);

    if !is_known_contact {
        tracing::warn!("收到来自未知用户 {} 的消息，已拒绝", peer);
        send_response(core, channel, None).await;
        return;
    }

    // === 签名验证通过后，记录身份绑定漂移（不阻断） ===
    // 签名验证已确认发送者持有该公钥对应私钥，因此绑定漂移只可能是同一身份
    // 经多个 PeerID 使用（多设备 / 轮换重叠），故仅告警，不阻断消息处理。
    let store = core.get_dht_store();
    if let Some(existing_peer) = store.get_peerid_by_pubkey(&sender_mldsa_pubkey_hex)
        && existing_peer != peer && core.connected_peers.contains_key(&existing_peer) {
            tracing::warn!(
                "身份绑定漂移: sender={}.. 声称 peer={}, 但缓存中为 {}（仍在线），更新绑定",
                &sender_mldsa_pubkey_hex[..16.min(sender_mldsa_pubkey_hex.len())],
                peer,
                existing_peer
            );
        }

    // 缓存发送方 (ML-DSA 公钥 → PeerID) 映射，用于后续回复消息时直接查找
    store.set_pubkey_peerid(&sender_mldsa_pubkey_hex, &peer);
    // 同步更新内存缓存，如果该 PeerID 已连接则触发在线状态刷新
    core.update_peerid_pubkey_mapping(peer, sender_mldsa_pubkey_hex.clone())
        .await;
    tracing::debug!(
        "已缓存发送方身份绑定: {}.. -> PeerID={}",
        &sender_mldsa_pubkey_hex[..16.min(sender_mldsa_pubkey_hex.len())],
        peer
    );

    // === 多设备消息去重（解密前） ===
    // 发送方对目标账号每台设备独立加密投递，同一逻辑消息携带同一 origin_hash。
    // 提前去重：已见过的副本直接丢弃（不解密、不回 Ack、不发送达回执），
    // 避免为重复消息浪费 ML-KEM 解密与数据库写入。
    // hop 超限丢弃（防环，兼容旧版转发消息）。
    if let Some(routing) = &request.routing {
        if routing.hop >= crate::message::MAX_DEVICE_ROUTING_HOP {
            tracing::debug!(
                "多设备转发 hop 超限 ({} >= {}), 丢弃消息",
                routing.hop,
                crate::message::MAX_DEVICE_ROUTING_HOP
            );
            return;
        }
        if !routing.origin_hash.is_empty() && core.device_seen_origin_hash_check(&routing.origin_hash)
        {
            tracing::debug!(
                "多设备消息重复 (origin_hash={}..), 丢弃",
                &routing.origin_hash[..16.min(routing.origin_hash.len())]
            );
            return;
        }
    }

    // === ML-KEM 会话密钥过期检测 ===
    // 消息附带发送方当前 ML-KEM 公钥指纹（已纳入签名哈希，防篡改）。
    // 与本地缓存比对：不一致 → 发送方会话密钥已轮换（重启/切换身份），
    // 回发 FriendOnline 完成双向密钥交换，避免"一方能发但对方收不到"。
    // 一致 → 正常继续，零额外开销。
    // 限流：持续失配（攻击者用错误公钥轰炸）与消息/分片限流独立计数，
    // 超限后不再回发 FriendOnline（等同骚扰处理）。注意：按账号(pubkey)计数，
    // 多设备下换 PeerID 无法绕过；限流是 30s 滑窗，窗口过期自动恢复——不会
    // 永久中断合法轮换；超限后日志降级为 debug，防借刷日志洪泛（日志量也是 DoS 面）。
    let mismatch_over = core.inbound_mismatch_rate_limited(&sender_mldsa_pubkey_hex);
    let mlkem_stale = !mismatch_over
        && if !request.sender_mlkem_sha256.is_empty() {
            let cached = core
                .peer_cache
                .peerid_to_mlkem
                .get(&peer)
                .map(|hex| sha2::Sha256::digest(hex.as_bytes()).to_vec())
                .unwrap_or_default();
            // 无缓存（从未收到 FriendOnline）视为 stale，主动交换补齐
            cached.is_empty() || cached != request.sender_mlkem_sha256
        } else {
            false
        };
    if mismatch_over {
        tracing::debug!(
            "来自 {} 的加密失配持续超限，不再回发 FriendOnline（30s 窗口过期自动恢复）",
            peer
        );
    } else if mlkem_stale {
        tracing::info!(
            "检测到发送方 {}.. 的 ML-KEM 公钥已轮换，回发 FriendOnline 触发密钥交换",
            &sender_mldsa_pubkey_hex[..16.min(sender_mldsa_pubkey_hex.len())]
        );
        core.send_friend_online(&peer).await;
    }

    // 先发送响应确认（绑定到请求哈希防止跨请求重放）
    send_response(core, channel, Some(&request.hash)).await;

    // 解密并处理消息
    // handle_decrypted_message 返回 true 表示解密成功，false 表示失败
    let decryption_success = handle_decrypted_message(
        core,
        pool,
        peer,
        &request,
        &sender_mldsa_pubkey_hex,
        &owner_identity_id,
    )
    .await;

    // 解密失败：发送方可能用了本方旧 ML-KEM 公钥加密（我方会话密钥已轮换）。
    // 主动回发 FriendOnline 让发送方更新我方最新公钥，打破单向死锁。
    // 复用同一失配限流：持续解密失败（攻击者用错误公钥轰炸）超限后不再回发。
    if !decryption_success && !mismatch_over {
        tracing::warn!(
            "解密失败：回发 FriendOnline 触发发送方 {}.. 更新我方 ML-KEM 公钥",
            &sender_mldsa_pubkey_hex[..16.min(sender_mldsa_pubkey_hex.len())]
        );
        core.send_friend_online(&peer).await;
    } else if !decryption_success {
        // 失配已超限（按账号计数）：仅丢弃，不再回发；30s 窗口过期自动恢复。
        // debug 级别防持续骚扰借日志洪泛。
        tracing::debug!(
            "解密失败且失配超限（账号 {}..），仅丢弃消息，窗口过期自动恢复",
            &sender_mldsa_pubkey_hex[..16.min(sender_mldsa_pubkey_hex.len())]
        );
    }

    // 只有解密成功后才发送送达回执
    if decryption_success && request.msgtype == ChatMessageType::Text {
        // 回执数据：`origin_hash:message_hash`（多设备扇出消息附带 origin_hash，
        // 发送方据此按逻辑消息入账键匹配 pending，不依赖某台设备的密文 hash）；
        // 无 routing（单设备/旧客户端）时保持纯 message_hash 兼容旧格式。
        let receipt_data = match &request.routing {
            Some(r) if !r.origin_hash.is_empty() => {
                format!("{}:{}", r.origin_hash, hex::encode(&request.hash))
            }
            _ => hex::encode(&request.hash),
        };

        // 通过 peerid_to_mlkem 查找发送方的 ML-KEM 公钥，并发回加密的回执
        let store = core.get_dht_store();
        if let Some(sender_peer_id) = store.get_peerid_by_pubkey(&sender_mldsa_pubkey_hex) {
            // 获取发送方的 ML-KEM 公钥（从 FriendOnline 缓存获取，无需 DHT）
            let sender_mlkem_pubkey = match core.peer_cache.peerid_to_mlkem.get(&sender_peer_id) {
                Some(hex_str) if !hex_str.is_empty() => match hex::decode(hex_str) {
                    Ok(key) => key,
                    Err(e) => {
                        tracing::warn!("发送方 ML-KEM 公钥 hex 解码失败: {}", e);
                        return;
                    }
                },
                _ => {
                    tracing::warn!(
                        "未找到发送方 {} 的 ML-KEM 公钥，无法加密送达回执",
                        &sender_mldsa_pubkey_hex[..16.min(sender_mldsa_pubkey_hex.len())]
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
            core.send_message(sender_peer_id, receipt_msg, None).await;
            tracing::info!("已向 {} 发送加密的送达回执", &sender_mldsa_pubkey_hex[..16.min(sender_mldsa_pubkey_hex.len())]);
        } else {
            tracing::debug!(
                "未找到发送方 {} 的 PeerID，无法发送送达回执",
                &sender_mldsa_pubkey_hex[..16.min(sender_mldsa_pubkey_hex.len())]
            );
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
                &sender_pubkey_hex[..16.min(sender_pubkey_hex.len())],
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
///
/// 返回 true 表示解密成功并已处理，false 表示解密失败
async fn handle_decrypted_message(
    core: &mut ChatCore,
    pool: &sqlx::Pool<sqlx::Sqlite>,
    _peer: libp2p::PeerId,
    request: &ChatMessage,
    sender_mldsa_pubkey_hex: &str,
    owner_identity_id: &str,
) -> bool {
    // owner_identity_id 由调用方 handle_incoming_request 已查得（当前身份），
    // 无需在此重复查询数据库（每条入站消息省 1 次 SQLite 往返）。

    // 使用 ChatCore 中缓存的 DecapsulationKey 对象解密消息
    // key_bytes() → new() 的 round-trip 在 aws-lc-rs 1.18+ 已验证可用；
    // 缓存对象仅为性能优化，避免每次解密时重建。
    let decap_key = match &core.mlkem_decap_key {
        Some(key) => key,
        None => {
            tracing::warn!("ML-KEM 解封装密钥未初始化，无法解密消息");
            return false;
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
                &sender_mldsa_pubkey_hex[..16.min(sender_mldsa_pubkey_hex.len())],
                e
            );
            return false;
        }
    };

    // 屏蔽拦截：忽略被屏蔽联系人发来的所有入站消息/文件请求
    // 走带缓存的 fail-closed 查询：DB 短暂故障时不误放行被屏蔽联系人
    if core
        .is_contact_blocked_cached(owner_identity_id, sender_mldsa_pubkey_hex)
        .await
    {
        tracing::info!(
            "忽略被屏蔽联系人 {}.. 的消息",
            &sender_mldsa_pubkey_hex[..16.min(sender_mldsa_pubkey_hex.len())]
        );
        // 返回 false：被屏蔽者不获得送达回执，避免泄露"已处理"信号
        return false;
    }

    // 按 msgtype 分发
    match request.msgtype {
        ChatMessageType::Text => {
            handle_text_message(
                core,
                pool,
                sender_mldsa_pubkey_hex,
                &request.hash,
                decrypted_data,
                owner_identity_id,
            )
            .await;
        }
        ChatMessageType::FileHash => {
            handle_file_hash_message(
                core,
                pool,
                sender_mldsa_pubkey_hex,
                owner_identity_id,
                decrypted_data,
            )
            .await;
        }
        // FileStream 分片已迁移到 /chat/rr_file 协议（EncryptedFileChunk），
        // 不再经 rr_msg 传输，此处无分发逻辑（枚举值保留用于兼容 DB）。
        ChatMessageType::FileStream => {
            tracing::debug!("收到 FileStream 类型 rr_msg（已废弃路径，忽略）");
        }
        ChatMessageType::FileDownloadRequest => {
            handle_file_download_request(core, sender_mldsa_pubkey_hex, decrypted_data).await;
        }
        ChatMessageType::FileDownloadResponse => {
            handle_file_download_response(core, decrypted_data).await;
        }
        ChatMessageType::DeliveryReceipt => {
            handle_delivery_receipt(core, pool, decrypted_data).await;
        }
        ChatMessageType::OnlineStatus => {
            // OnlineStatus 消息通过 gossipsub 协议传输，不会通过 request-response 到达这里
            tracing::debug!("OnlineStatus 消息通过 request-response 到达，忽略");
        }
        ChatMessageType::Unknown => {
            tracing::warn!("收到未知类型消息，忽略");
        }
    }

    true
}

/// 处理文本消息：UTF-8 解码 → 存储 → 通知 UI
async fn handle_text_message(
    core: &mut ChatCore,
    pool: &sqlx::Pool<sqlx::Sqlite>,
    sender_mldsa_pubkey_hex: &str,
    request_hash: &[u8],
    data: Vec<u8>,
    owner_identity_id: &str,
) {
    match String::from_utf8(data) {
        Ok(text) => {
            // 使用 ChatMessage 自身 hash（= SHA256(msgtype‖ts‖nonce‖ciphertext)）作为去重键：
            // 同一消息重传 → 同 hash → 去重；新发相同内容 → 新 hash → 不误杀
            let message_hash = hex::encode(request_hash);

            // owner_identity_id（当前身份）由 handle_incoming_request 查询后贯穿传入，
            // 避免每条入站消息重复查数据库。

            match storage::add_message_with_hash(
                pool,
                owner_identity_id,
                sender_mldsa_pubkey_hex,
                &text,
                false,
                false,
                &message_hash,
                crate::ChatMessageType::Text as i32,
            )
            .await
            {
                Ok(Some(_id)) => {
                    // 新消息，正常处理
                }
                Ok(None) => {
                    // 重复消息，跳过
                    tracing::debug!("跳过重复消息 (hash={}..)", &message_hash[..16.min(message_hash.len())]);
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

/// 处理文件哈希消息：解析 FileHashInfo → 存储到数据库 + 通知 UI
async fn handle_file_hash_message(
    core: &mut ChatCore,
    pool: &sqlx::Pool<sqlx::Sqlite>,
    sender_mldsa_pubkey_hex: &str,
    owner_identity_id: &str,
    data: Vec<u8>,
) {
    match postcard::from_bytes::<crate::message::FileHashInfo>(&data) {
        Ok(file_info) => {
            tracing::info!(
                "收到文件哈希分享: filename={}, size={}, hash={:?}",
                file_info.filename,
                file_info.total_size,
                file_info.file_hash,
            );

            let file_hash_hex = hex::encode(file_info.file_hash);

            // owner_identity_id 由调用方查询后贯穿传入，避免重复查数据库
            let content_hex = hex::encode(&data);
            let _ = storage::add_message_with_hash(
                pool,
                owner_identity_id,
                sender_mldsa_pubkey_hex,
                &content_hex,
                false,
                false,
                "", // 文件消息不需要去重哈希
                crate::ChatMessageType::FileHash as i32,
            )
            .await;

            core.send_message_mpsc(crate::command::IncomingMessage::FileShare {
                filename: file_info.filename,
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

/// 处理文件下载请求（发送方收到接收方的下载请求）
/// 解析 DownloadRequest → 查 sent_files 历史 → 验证文件有效性 → 拒绝/接受并发送分片
async fn handle_file_download_request(
    core: &mut ChatCore,
    sender_mldsa_pubkey_hex: &str,
    data: Vec<u8>,
) {
    let request: crate::message::DownloadRequest = match postcard::from_bytes(&data) {
        Ok(req) => req,
        Err(e) => {
            tracing::warn!("解析 DownloadRequest 失败: {}", e);
            return;
        }
    };

    let file_hash_hex = hex::encode(request.file_hash);
    tracing::info!("收到文件下载请求: file_hash={}..", &file_hash_hex[..16.min(file_hash_hex.len())]);

    // 查 sent_files 历史验证合法性
    let sent_file = match storage::pool() {
        Some(pool) => match storage::get_sent_file(pool, &request.file_hash).await {
            Ok(Some(sent)) => Some(sent),
            _ => None,
        },
        None => None,
    };

    let Some(sent_file) = sent_file else {
        tracing::warn!(
            "拒绝下载请求: file_hash={}.. 不在发送历史中",
            &file_hash_hex[..16.min(file_hash_hex.len())]
        );
        send_reject_response(core, sender_mldsa_pubkey_hex, request.file_hash, "文件未授权：该文件不在发送历史中").await;
        return;
    };

    // 下载授权绑定接收人：请求者必须与发送时记录的接收人一致，
    // 防止"知道哈希即可被任意联系人拉取本机已发送文件"。
    if sent_file.recipient_pubkey_hex.is_empty()
        || sent_file.recipient_pubkey_hex != sender_mldsa_pubkey_hex
    {
        tracing::warn!(
            "拒绝下载请求: file_hash={}.. 请求者不是授权接收人",
            &file_hash_hex[..16.min(file_hash_hex.len())]
        );
        send_reject_response(
            core,
            sender_mldsa_pubkey_hex,
            request.file_hash,
            "文件未授权：你不是该文件的接收人",
        )
        .await;
        return;
    }

    // 验证文件是否仍有效（路径存在且哈希匹配）
    let file_path = std::path::PathBuf::from(&sent_file.file_path);
    match storage::verify_sent_file(&sent_file).await {
        Ok(true) => {}
        Ok(false) => {
            let reason = "下载被拒绝：文件可能已被移动、删除或内容已更改";
            tracing::warn!(
                "拒绝下载请求: file={:?}, reason={reason}",
                sent_file.file_path
            );
            send_reject_response(core, sender_mldsa_pubkey_hex, request.file_hash, reason).await;
            // 由定时器 handle_sent_file_scan 负责清理无效记录，避免并发竞态
            return;
        }
        Err(e) => {
            tracing::warn!("验证文件有效性失败: {e}，拒绝请求但保留记录供下次重试");
            send_reject_response(core, sender_mldsa_pubkey_hex, request.file_hash, "下载被拒绝：暂时无法验证文件有效性，请稍后重试").await;
            return;
        }
    }

    let filename = sent_file.filename.clone();

    // 获取文件信息
    let metadata = match tokio::fs::metadata(&file_path).await {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!("获取文件元信息失败: {:?}: {}", file_path, e);
            return;
        }
    };
    let file_size = metadata.len();
    if file_size > crate::transfer::MAX_FILE_SIZE {
        tracing::warn!(
            "拒绝发送过大文件: {} (size={} > MAX={})",
            filename,
            file_size,
            crate::transfer::MAX_FILE_SIZE
        );
        return;
    }
    let chunk_size: u32 = 256 * 1024;
    let total_chunks = file_size.div_ceil(chunk_size as u64) as u32;

    let file_hash = request.file_hash;

    // 发送接受响应
    let response = crate::message::DownloadResponse {
        file_hash,
        accepted: true,
        filename: Some(filename.clone()),
        total_size: Some(file_size),
        total_chunks: Some(total_chunks),
        chunk_size: Some(chunk_size),
        error_reason: None,
    };
    let response_data = match postcard::to_allocvec(&response) {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!("序列化 DownloadResponse 失败: {}", e);
            return;
        }
    };
    if let Err(e) = core
        .send_text(
            sender_mldsa_pubkey_hex,
            ChatMessageType::FileDownloadResponse,
            response_data,
            false,
        )
        .await
    {
        tracing::warn!("发送 DownloadResponse 失败: {}", e);
        return;
    }

    // === 直连协商 ===
    let store = core.get_dht_store();
    if let Some(recipient_peer_id) = store.get_peerid_by_pubkey(sender_mldsa_pubkey_hex)
        && let addrs = store.get_multiaddrs(&recipient_peer_id)
        && !addrs.is_empty()
    {
        tracing::info!(
            "文件传输：尝试与 {}.. 建立直连，发现 {} 个地址",
            &sender_mldsa_pubkey_hex[..16.min(sender_mldsa_pubkey_hex.len())],
            addrs.len()
        );
        for addr in &addrs {
            let dial_addr = addr
                .clone()
                .with_p2p(recipient_peer_id)
                .unwrap_or(addr.clone());
            if let Err(e) = core
                .p2p_handle
                .tx
                .try_send(P2pCommand::DialAddr {
                    addr: dial_addr,
                })
            {
                tracing::warn!("Failed to send DialAddr during file transfer: {e:?}");
            }
        }
    }

    // 逐分片读取并发送（加密 + 签名后经 /chat/rr_file 传输）
    //
    // 架构互锁修复：发送循环不再在 ChatCore 事件循环内同步执行，
    // 而是快照所需状态后 spawn 到后台任务。否则单文件传输会占死
    // ChatCore 循环（每片等待 rr_file ack 最多 120s×3），使 P2pActor
    // 的 send_event().await 因事件通道满载而阻塞，两个事件循环互相
    // 等待形成死锁。接收方向（IncomingFileChunk 回 Ack）同样用
    // try_send + 有限重试（见 event_loop.rs），不阻塞等待对端通道。
    let compression_level = crate::compression::compression_level(ChatMessageType::FileStream);

    let p2p_tx = core.p2p_handle.tx.clone();
    let filename_owned = filename.clone();
    let sender_pubkey_owned = sender_mldsa_pubkey_hex.to_string();
    let file_hash_hex = hex::encode(file_hash);
    // 去重键含接收方：同一文件可并发发给不同联系人
    let send_key = format!("{sender_mldsa_pubkey_hex}:{file_hash_hex}");
    let store = core.get_dht_store();
    let Some(recipient_peer_id) = store.get_peerid_by_pubkey(sender_mldsa_pubkey_hex) else {
        tracing::warn!(
            "文件传输中止：未找到 {} 的 PeerID，无法加密分片",
            &sender_mldsa_pubkey_hex[..16.min(sender_mldsa_pubkey_hex.len())]
        );
        return;
    };
    let Some(recipient_mlkem_pubkey) =
        core.peer_cache.peerid_to_mlkem.get(&recipient_peer_id).and_then(|hex_str| {
            (!hex_str.is_empty()).then(|| hex::decode(hex_str).ok()).flatten()
        })
    else {
        tracing::warn!(
            "文件传输中止：未找到 {} 的 ML-KEM 公钥，无法加密分片",
            &sender_mldsa_pubkey_hex[..16.min(sender_mldsa_pubkey_hex.len())]
        );
        return;
    };
    let Some(mldsa_private_key) = core.mldsa_private_key.clone() else {
        tracing::error!("文件传输中止：ML-DSA 私钥未缓存，无法签名分片");
        return;
    };
    let mldsa_public_key = match crate::identity::extract_public_key_from_private(
        &mldsa_private_key,
        true,
    ) {
        Ok(k) => k,
        Err(e) => {
            tracing::error!("文件传输中止：提取 ML-DSA 公钥失败: {e}");
            return;
        }
    };

    // 出站发送去重 + 并发上限（按 (接收方, file_hash) 去重，
    // 防止同一文件对同一接收方并发 spawn 多个发送任务，同时允许
    // 同一文件并发发给不同联系人）
    let exceeded_limit = {
        let mut active = core.active_file_sends.lock().unwrap_or_else(|e| e.into_inner());
        if active.len() >= crate::core::MAX_OUTBOUND_FILE_SENDS {
            tracing::warn!(
                "拒绝并发文件发送：当前发送任务数已达上限 ({}), file={}..",
                crate::core::MAX_OUTBOUND_FILE_SENDS,
                &file_hash_hex[..16.min(file_hash_hex.len())]
            );
            true
        } else {
            // 已存在同接收方同文件的发送任务（可能下载方仍在下载中）：静默忽略重复请求
            if active.contains(&send_key) {
                tracing::debug!(
                    "忽略重复的文件下载请求：文件 {}.. 正在发送给该接收方",
                    &file_hash_hex[..16.min(file_hash_hex.len())]
                );
                return;
            }
            active.insert(send_key.clone());
            false
        }
    };
    if exceeded_limit {
        // 并发上限：明确告知下载方稍后重试
        send_reject_response(
            core,
            sender_mldsa_pubkey_hex,
            file_hash,
            "并发发送任务数已达上限，请稍后重试",
        )
        .await;
        return;
    }

    let active_file_sends = core.active_file_sends.clone();
    std::mem::drop(crate::actor::RUNTIME.spawn(async move {
        send_file_chunks_background(
            p2p_tx,
            file_path,
            filename_owned,
            file_size,
            total_chunks,
            chunk_size,
            file_hash,
            recipient_peer_id,
            recipient_mlkem_pubkey,
            mldsa_private_key,
            mldsa_public_key,
            compression_level,
            sender_pubkey_owned,
            send_key,
            active_file_sends,
        )
        .await;
    }));
}

/// 在后台任务中逐分片读取、加密、签名并发送文件（不阻塞 ChatCore 事件循环）。
///
/// 所有依赖 ChatCore 的状态在调用前已快照为 owned 值传入，任务内部
/// 只通过 `p2p_tx` 与 P2pActor 通信，等待每个分片的 rr_file 确认。
#[allow(clippy::too_many_arguments)]
async fn send_file_chunks_background(
    p2p_tx: mpsc::Sender<P2pCommand>,
    file_path: std::path::PathBuf,
    filename: String,
    file_size: u64,
    total_chunks: u32,
    chunk_size: u32,
    file_hash: [u8; 32],
    recipient_peer_id: libp2p::PeerId,
    recipient_mlkem_pubkey: Vec<u8>,
    mldsa_private_key: Zeroizing<Vec<u8>>,
    mldsa_public_key: Vec<u8>,
    compression_level: i32,
    // 下载方 ML-DSA 公钥 hex（失败时用于回发拒绝响应）
    recipient_pubkey_hex: String,
    // 去重键（接收方:file_hash hex，任务结束时移除）
    send_key: String,
    // 共享的出站发送去重集合（任务结束时移除自身）
    active_file_sends: Arc<std::sync::Mutex<std::collections::HashSet<String>>>,
) {
    let mut file = match tokio::fs::File::open(&file_path).await {
        Ok(f) => f,
        Err(e) => {
            tracing::warn!("打开文件失败: {:?}: {}", file_path, e);
            notify_reject(&p2p_tx, &recipient_pubkey_hex, file_hash, "发送方打开文件失败").await;
            finish_file_send(&active_file_sends, &send_key);
            return;
        }
    };

    let mut sent_count = 0u32;
    // 无进展看门狗：任一成功的分片确认后重置；超过 10 分钟无任何分片
    // 送达，判定传输卡死（对端离线/通道静默），主动中止并回发拒绝。
    let mut last_progress = std::time::Instant::now();
    for chunk_index in 0..total_chunks {
        if last_progress.elapsed() > std::time::Duration::from_secs(600) {
            tracing::error!("文件发送 10 分钟无进展，传输中止");
            notify_reject(
                &p2p_tx,
                &recipient_pubkey_hex,
                file_hash,
                "文件发送超时（10 分钟无进展）",
            )
            .await;
            finish_file_send(&active_file_sends, &send_key);
            return;
        }
        let offset = chunk_index as u64 * chunk_size as u64;
        let config = crate::message::ChunkReadConfig {
            file_hash,
            filename: filename.clone(),
            total_size: file_size,
            total_chunks,
            chunk_size,
            chunk_index,
            offset,
        };

        let (chunk, _bytes_read) =
            match crate::message::FileStreamChunk::from_file(&mut file, &config, compression_level)
                .await
            {
                Ok(result) => result,
                Err(e) => {
                    tracing::warn!("读取文件分片 {} 失败: {}", chunk_index, e);
                    notify_reject(&p2p_tx, &recipient_pubkey_hex, file_hash, "读取文件分片失败").await;
                    finish_file_send(&active_file_sends, &send_key);
                    return;
                }
            };

        // 加密 + 签名
        let sealed = match crate::message::EncryptedFileChunk::seal(
            &chunk,
            &recipient_mlkem_pubkey,
            &mldsa_private_key,
            &mldsa_public_key,
        ) {
            Ok(s) => s,
            Err(e) => {
                tracing::error!("加密签名文件分片 {} 失败: {e}", chunk_index);
                notify_reject(&p2p_tx, &recipient_pubkey_hex, file_hash, "加密签名文件分片失败").await;
                finish_file_send(&active_file_sends, &send_key);
                return;
            }
        };

        // 发送并等待对端确认：仅当 rr_file 响应 true 或命令成功投递时视为已发送
        let mut send_ok = false;
        for attempt in 0..5 {
            if attempt > 0 {
                tokio::time::sleep(std::time::Duration::from_millis(300 * attempt)).await;
            }
            let (ack_tx, ack_rx) = tokio::sync::oneshot::channel();
            match p2p_tx.try_send(P2pCommand::SendFileChunk {
                peer_id: recipient_peer_id,
                chunk: sealed.clone(),
                ack: ack_tx,
            }) {
                Ok(()) => {
                    // 命令已进入通道：等 P2pActor 的 rr_file 确认（Response/OutboundFailure）
                    send_ok = match tokio::time::timeout(
                        std::time::Duration::from_secs(120),
                        ack_rx,
                    )
                    .await
                    {
                        Ok(Ok(true)) => true,
                        Ok(Ok(false)) => {
                            tracing::debug!("分片 {} 被对端拒绝 (尝试 {}/5)", chunk_index, attempt + 1);
                            false
                        }
                        Ok(Err(_)) | Err(_) => {
                            tracing::debug!("分片 {} 确认超时 (尝试 {}/5)", chunk_index, attempt + 1);
                            false
                        }
                    };
                    if send_ok {
                        break;
                    }
                }
                Err(e) => {
                    tracing::debug!("发送文件分片 {} 失败 (尝试 {}/5): {:?}", chunk_index, attempt + 1, e);
                }
            }
        }
        if !send_ok {
            tracing::error!(
                "发送文件分片 {} 失败（5次尝试均已失败），传输中止",
                chunk_index
            );
            notify_reject(
                &p2p_tx,
                &recipient_pubkey_hex,
                file_hash,
                "文件分片发送失败，传输中止",
            )
            .await;
            finish_file_send(&active_file_sends, &send_key);
            return;
        }

        // 分片确认成功：刷新无进展看门狗
        last_progress = std::time::Instant::now();
        sent_count += 1;

        if chunk.is_last {
            break;
        }
        // 每发送 10 个分片主动让出控制权
        if sent_count.is_multiple_of(10) {
            tokio::task::yield_now().await;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    tracing::info!(
        "文件发送完成: {}, total_chunks={}, sent_chunks={}",
        filename,
        total_chunks,
        sent_count,
    );
    // 正常结束也要移除去重标记，允许后续再次发送同一文件给同一接收方
    finish_file_send(&active_file_sends, &send_key);
}

/// 向下载方回发"传输中止"拒绝通知（经 P2pActor → ChatCore → send_reject_response）
async fn notify_reject(
    p2p_tx: &mpsc::Sender<P2pCommand>,
    pubkey_hex: &str,
    file_hash: [u8; 32],
    reason: &str,
) {
    if let Err(e) = p2p_tx
        .try_send(P2pCommand::NotifyFileDownloadReject {
            pubkey_hex: pubkey_hex.to_string(),
            file_hash,
            reason: reason.to_string(),
        })
    {
        tracing::warn!("发送文件拒绝通知失败: {e:?}");
    }
}

/// 从共享出站发送集合移除自身（任务结束，无论成败）
fn finish_file_send(
    active_file_sends: &Arc<std::sync::Mutex<std::collections::HashSet<String>>>,
    send_key: &str,
) {
    if let Ok(mut set) = active_file_sends.lock() {
        set.remove(send_key);
    }
}

/// 发送拒绝下载的响应
pub(crate) async fn send_reject_response(
    core: &mut ChatCore,
    sender_mldsa_pubkey_hex: &str,
    file_hash: [u8; 32],
    reason: &str,
) {
    let response = crate::message::DownloadResponse {
        file_hash,
        accepted: false,
        filename: None,
        total_size: None,
        total_chunks: None,
        chunk_size: None,
        error_reason: Some(reason.to_string()),
    };
    if let Ok(data) = postcard::to_allocvec(&response) {
        let _ = core
            .send_text(
                sender_mldsa_pubkey_hex,
                ChatMessageType::FileDownloadResponse,
                data,
                false,
            )
            .await;
    }
}

/// 处理文件下载响应（接收方收到发送方的同意/拒绝）
async fn handle_file_download_response(core: &mut ChatCore, data: Vec<u8>) {
    let response: crate::message::DownloadResponse = match postcard::from_bytes(&data) {
        Ok(resp) => resp,
        Err(e) => {
            tracing::warn!("解析 DownloadResponse 失败: {}", e);
            return;
        }
    };

    if !response.accepted {
        let file_hash_hex = hex::encode(response.file_hash);
        let reason = response.error_reason.unwrap_or_else(|| "未知原因".to_string());
        tracing::warn!(
            "发送方拒绝了下载请求: file_hash={}.., reason={reason}",
            &file_hash_hex[..16.min(file_hash_hex.len())]
        );
        // 释放状态与并发传输槽位，避免恶意对端长期占用 MAX_CONCURRENT_TRANSFERS
        core.file_transfer.cancel_download(&file_hash_hex);
        core.send_warning_mpsc(format!("下载被拒绝: {reason}")).await;
        return;
    }

    tracing::info!(
        "发送方接受了下载请求: file_hash={}.., filename={}, size={}, chunks={}",
        &hex::encode(response.file_hash)[..16.min(hex::encode(response.file_hash).len())],
        response.filename.as_deref().unwrap_or("?"),
        response.total_size.unwrap_or(0),
        response.total_chunks.unwrap_or(0),
    );

    core.file_transfer.handle_download_response(response).await;
}

/// 发送签名响应确认
async fn send_response(
    core: &mut ChatCore,
    channel: libp2p::request_response::ResponseChannel<ChatResponse>,
    request_hash: Option<&[u8]>,
) {
    let response = match build_signed_response(core, request_hash) {
        Ok(resp) => resp,
        Err(e) => {
            tracing::error!("构建签名响应失败: {}，无法发送响应", e);
            return;
        }
    };
    // 通过 P2pActor 发送响应
    // 注意：这里需要将 response channel 发送给 P2pActor 来处理
    // 由于 send_response 需要访问 swarm，而 ChatCore 不再持有 swarm，
    // 我们需要通过 P2pActor 来发送响应。
    // 不能 send().await 阻塞：ChatCore 正处理入站事件，若本循环阻塞在命令通道，
    // 而 P2pActor 又阻塞在向本循环投递事件上，两个有界通道同时占满即死锁。
    // try_send + 短暂重试：失败时丢弃响应，发送方按 rr_msg 超时重试整个请求。
    let mut cmd = P2pCommand::SendResponse { channel, response };
    for attempt in 0..3 {
        match core.p2p_handle.tx.try_send(cmd) {
            Ok(()) => return,
            Err(mpsc::error::TrySendError::Full(c)) => {
                cmd = c;
                tokio::task::yield_now().await;
                if attempt == 2 {
                    tracing::warn!(
                        "回送响应失败：P2pActor 命令通道持续满载，丢弃响应（发送方将按 rr_msg 超时重试）"
                    );
                }
            }
            Err(mpsc::error::TrySendError::Closed(c)) => {
                tracing::warn!("回送响应失败：P2pActor 命令通道已关闭");
                drop(c);
                return;
            }
        }
    }
}

/// 构建带 ML-DSA 签名的 ChatResponse
fn build_signed_response(core: &ChatCore, request_hash: Option<&[u8]>) -> P2pResult<ChatResponse> {
    let mldsa_private_key = core
        .mldsa_private_key
        .as_ref()
        .ok_or(P2pError::MlDsaPrivateKeyNotCached)?;
    let mldsa_public_key =
        crate::identity::extract_public_key_from_private(mldsa_private_key, true)
            .map_err(|e| P2pError::SwarmInitFailed(e.into()))?;
    ChatResponse::new_signed(mldsa_private_key, &mldsa_public_key, request_hash)
        .map_err(|e| P2pError::SwarmInitFailed(e.into()))
}

/// 处理消息送达回执：将对应的待发送消息标记为已发送，并通知 UI
async fn handle_delivery_receipt(
    core: &mut ChatCore,
    pool: &sqlx::Pool<sqlx::Sqlite>,
    data: Vec<u8>,
) {
    // 回执数据格式：
    //   - 多设备扇出：`origin_hash:message_hash`（冒号分隔，origin_hash 是逻辑消息入账键）
    //   - 单设备/旧客户端：纯 message_hash（兼容旧格式）
    match String::from_utf8(data) {
        Ok(receipt_text) => {
            let (query_key, receipt_msg_hash) = match receipt_text.split_once(':') {
                Some((origin, hash)) => (origin.to_string(), hash.to_string()),
                None => (receipt_text.clone(), receipt_text),
            };
            tracing::info!(
                "收到送达回执，查询键: {}.. (hash={}..)",
                &query_key[..16.min(query_key.len())],
                &receipt_msg_hash[..16.min(receipt_msg_hash.len())]
            );

            // 优先按 origin_hash（逻辑消息键）查询，命中即标记该逻辑消息已发送；
            // 未命中再按副本 message_hash 查询（兼容旧客户端纯 hash 回执）
            match storage::get_message_by_hash(pool, &query_key).await {
                Ok(Some(msg)) => {
                    if msg.pending != 0 {
                        if let Err(e) = storage::mark_sent(pool, msg.id).await {
                            tracing::warn!("标记消息 {} 为已发送失败: {}", msg.id, e);
                        } else {
                            tracing::info!("消息 {} 已通过送达回执标记为已发送", msg.id);
                        }
                    }
                    // 前端用该 key 匹配 message-sent 事件设置的 hash → 必须传 query_key
                    core.send_message_mpsc(crate::command::IncomingMessage::DeliveryReceipt {
                        message_hash: query_key,
                        peer_id: msg.peer_pubkey_hex.clone(),
                    })
                    .await;
                }
                Ok(None) => {
                    tracing::warn!(
                        "未找到哈希 {} 对应的消息，送达回执无法匹配",
                        &query_key[..16.min(query_key.len())]
                    );
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
