use crate::actor::p2p::P2pCommand;
use crate::error::{P2pError, P2pResult};
use crate::{ChatCore, ChatMessage, ChatMessageType, ChatResponse, crypto, storage};

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
                &sender_mldsa_pubkey_hex[..16],
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
        &sender_mldsa_pubkey_hex[..16],
        peer
    );

    // 先发送响应确认（绑定到请求哈希防止跨请求重放）
    send_response(core, channel, Some(&request.hash)).await;

    // 解密并处理消息
    // handle_decrypted_message 返回 true 表示解密成功，false 表示失败
    let decryption_success =
        handle_decrypted_message(core, pool, peer, &request, &sender_mldsa_pubkey_hex).await;

    // 只有解密成功后才发送送达回执
    if decryption_success && request.msgtype == ChatMessageType::Text {
        // 使用 ChatMessage 自身的 hash 字段作为回执内容，
        // 与发送方 save_pending_message_with_hash 中使用的哈希一致。
        let receipt_data = hex::encode(&request.hash);

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
            core.send_message(sender_peer_id, receipt_msg, None).await;
            tracing::info!("已向 {} 发送加密的送达回执", &sender_mldsa_pubkey_hex[..16]);
        } else {
            tracing::debug!(
                "未找到发送方 {} 的 PeerID，无法发送送达回执",
                &sender_mldsa_pubkey_hex[..16]
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
///
/// 返回 true 表示解密成功并已处理，false 表示解密失败
async fn handle_decrypted_message(
    core: &mut ChatCore,
    pool: &sqlx::Pool<sqlx::Sqlite>,
    _peer: libp2p::PeerId,
    request: &ChatMessage,
    sender_mldsa_pubkey_hex: &str,
) -> bool {
    // 确认当前身份存在（解密需要当前身份的 ML-KEM 私钥）
    if storage::current_identity_id(pool).await.is_none() {
        tracing::warn!("未找到当前身份");
        return false;
    }

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
                &sender_mldsa_pubkey_hex[..16],
                e
            );
            return false;
        }
    };

    // 按 msgtype 分发
    match request.msgtype {
        ChatMessageType::Text => {
            handle_text_message(core, pool, sender_mldsa_pubkey_hex, &request.hash, decrypted_data).await;
        }
        ChatMessageType::FileHash => {
            handle_file_hash_message(core, pool, sender_mldsa_pubkey_hex, decrypted_data).await;
        }
        ChatMessageType::FileStream => {
            handle_file_stream_message(core, decrypted_data).await;
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
) {
    match String::from_utf8(data) {
        Ok(text) => {
            // 使用 ChatMessage 自身 hash（= SHA256(msgtype‖ts‖nonce‖ciphertext)）作为去重键：
            // 同一消息重传 → 同 hash → 去重；新发相同内容 → 新 hash → 不误杀
            let message_hash = hex::encode(request_hash);

            // 获取当前身份的 identity_id
            let Some(owner_identity_id) = storage::current_identity_id(pool).await else {
                tracing::warn!("未找到当前身份，无法保存接收的消息");
                return;
            };

            match storage::add_message_with_hash(
                pool,
                &owner_identity_id,
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
                    tracing::debug!("跳过重复消息 (hash={}..)", &message_hash[..16]);
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

            // 存入数据库，使重启后仍在历史中可见
            let Some(owner_identity_id) = storage::current_identity_id(pool).await else {
                tracing::warn!("未找到当前身份，无法保存收到的文件消息");
                return;
            };
            let content_hex = hex::encode(&data);
            let _ = storage::add_message_with_hash(
                pool,
                &owner_identity_id,
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

/// 处理文件流消息：解析 FileStreamChunk → 写入文件
///
/// 注意：data 是序列化后的 FileStreamChunk，chunk_data 已在 from_file() 中压缩，
/// 解压缩与哈希校验在 handle_file_stream_chunk 中完成
async fn handle_file_stream_message(core: &mut ChatCore, data: Vec<u8>) {
    match postcard::from_bytes::<crate::message::FileStreamChunk>(&data) {
        Ok(chunk) => {
            tracing::info!(
                "收到文件分片: file_id={:?}, chunk={}/{}, filename={}",
                chunk.file_hash,
                chunk.chunk_index,
                chunk.total_chunks,
                chunk.filename,
            );
            // 写入文件
            if let Err(e) = core.file_transfer.handle_file_stream_chunk(chunk).await {
                tracing::warn!("写入文件分片失败: {}", e);
            }
        }
        Err(e) => {
            tracing::warn!("解析 FileStreamChunk 失败: {}", e);
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
    tracing::info!("收到文件下载请求: file_hash={}..", &file_hash_hex[..16]);

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
            &file_hash_hex[..16]
        );
        send_reject_response(core, sender_mldsa_pubkey_hex, request.file_hash, "文件未授权：该文件不在发送历史中").await;
        return;
    };

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
            &sender_mldsa_pubkey_hex[..16],
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

    // 逐分片读取并发送
    let compression_level = crate::compression::compression_level(ChatMessageType::FileStream);
    let mut file = match tokio::fs::File::open(&file_path).await {
        Ok(f) => f,
        Err(e) => {
            tracing::warn!("打开文件失败: {:?}: {}", file_path, e);
            return;
        }
    };

    let mut sent_count = 0u32;
    for chunk_index in 0..total_chunks {
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
                    return;
                }
            };

        let chunk_data = match postcard::to_allocvec(&chunk) {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!("序列化 FileStreamChunk 失败: {}", e);
                return;
            }
        };

        let mut send_ok = false;
        for attempt in 0..3 {
            if attempt > 0 {
                tokio::time::sleep(std::time::Duration::from_millis(300 * attempt)).await;
            }
            match core
                .send_text(
                    sender_mldsa_pubkey_hex,
                    ChatMessageType::FileStream,
                    chunk_data.clone(),
                    false,
                )
                .await
            {
                Ok(_) => {
                    send_ok = true;
                    break;
                }
                Err(e) => {
                    tracing::debug!(
                        "发送文件分片 {} 失败 (尝试 {}/3): {}",
                        chunk_index,
                        attempt + 1,
                        e
                    );
                }
            }
        }
        if !send_ok {
            tracing::error!(
                "发送文件分片 {} 失败（3次尝试均已失败），传输中止",
                chunk_index
            );
            return;
        }

        sent_count += 1;

        if chunk.is_last {
            break;
        }
        // 每发送 10 个分片主动让出控制权，让事件循环处理其他消息
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
}

/// 发送拒绝下载的响应
async fn send_reject_response(
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
            &file_hash_hex[..16]
        );
        // 释放状态与并发传输槽位，避免恶意对端长期占用 MAX_CONCURRENT_TRANSFERS
        core.file_transfer.cancel_download(&file_hash_hex);
        core.send_warning_mpsc(format!("下载被拒绝: {reason}")).await;
        return;
    }

    tracing::info!(
        "发送方接受了下载请求: file_hash={}.., filename={}, size={}, chunks={}",
        &hex::encode(response.file_hash)[..16],
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
    // 我们需要通过 P2pActor 来发送响应
    let _ = core
        .p2p_handle
        .send(
            P2pCommand::SendResponse { channel, response },
        )
        .await;
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
    // 回执数据格式：原始消息的 message_hash（SHA256 hex）
    match String::from_utf8(data) {
        Ok(receipt_msg_hash) => {
            tracing::info!("收到送达回执，消息哈希: {}", &receipt_msg_hash[..16.min(receipt_msg_hash.len())]);

            // 直接按哈希查询（索引命中），避免遍历全部 pending 消息
            match storage::get_message_by_hash(pool, &receipt_msg_hash).await {
                Ok(Some(msg)) => {
                    if msg.pending != 0 {
                        if let Err(e) = storage::mark_sent(pool, msg.id).await {
                            tracing::warn!("标记消息 {} 为已发送失败: {}", msg.id, e);
                        } else {
                            tracing::info!("消息 {} 已通过送达回执标记为已发送", msg.id);
                        }
                    }
                    core.send_message_mpsc(crate::command::IncomingMessage::DeliveryReceipt {
                        message_hash: receipt_msg_hash.clone(),
                        peer_id: msg.peer_pubkey_hex.clone(),
                    })
                    .await;
                }
                Ok(None) => {
                    tracing::warn!(
                        "未找到哈希 {} 对应的消息，送达回执无法匹配",
                        &receipt_msg_hash[..16.min(receipt_msg_hash.len())]
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
