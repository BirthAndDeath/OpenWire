use crate::actor::p2p::P2pCommand;
use crate::error::{P2pError, P2pResult};
use crate::{
    ChatCore, ChatMessage, ChatMessageType, ChatResponse, crypto, storage,
};

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
            send_response(core, channel).await;
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
        send_response(core, channel).await;
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

    // 先发送响应确认，避免 request-response 协议超时重试
    // 注意：即使后续解密失败，也发送响应确认，因为协议层需要响应
    send_response(core, channel).await;

    // 解密并处理消息
    // handle_decrypted_message 返回 true 表示解密成功，false 表示失败
    let decryption_success = handle_decrypted_message(
        core, pool, peer, &request, &sender_mldsa_pubkey_hex,
    )
    .await;

    // 只有解密成功后才发送送达回执
    if decryption_success && request.msgtype == ChatMessageType::Text {
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
                core.send_message(sender_peer_id, receipt_msg).await;
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
///
/// 返回 true 表示解密成功并已处理，false 表示解密失败
async fn handle_decrypted_message(
    core: &mut ChatCore,
    pool: &sqlx::Pool<sqlx::Sqlite>,
    _peer: libp2p::PeerId,
    request: &ChatMessage,
    sender_mldsa_pubkey_hex: &str,
) -> bool {
    // 获取当前身份的 identity_id（保留用于后续可能的用途）
    let _identity_id = match storage::get_current_identity(pool).await.ok().flatten() {
        Some(id) => id,
        None => {
            tracing::warn!("未找到当前身份");
            return false;
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
        ChatMessageType::OnlineStatus => {
            // OnlineStatus 消息通过 gossipsub 协议传输，不会通过 request-response 到达这里
            tracing::debug!("OnlineStatus 消息通过 request-response 到达，忽略");
        }
    }

    true
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
            // 通过 P2pActor 检查连接状态并拨号
            // 注意：这里不再直接访问 core.swarm，而是通过 P2pActor 发送命令
            // 由于需要同步检查连接状态，暂时通过 P2pActor 发送 Dial 命令
            // 如果已连接，dial 会被 libp2p 忽略
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
                    let _ = core.p2p_handle.send(
                        crate::actor::ActorCommand::Custom(P2pCommand::DialAddr {
                            addr: dial_addr,
                        }),
                    ).await;
                }
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
async fn send_response(
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
    // 通过 P2pActor 发送响应
    // 注意：这里需要将 response channel 发送给 P2pActor 来处理
    // 由于 send_response 需要访问 swarm，而 ChatCore 不再持有 swarm，
    // 我们需要通过 P2pActor 来发送响应
    let _ = core.p2p_handle.send(
        crate::actor::ActorCommand::Custom(P2pCommand::SendResponse {
            channel,
            response,
        }),
    ).await;
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
