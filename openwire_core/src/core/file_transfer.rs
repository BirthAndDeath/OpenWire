use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::{
    command::{FileTransferProgress, MessageEvent},
    core::ChatCore,
    error::CoreError,
    message::{ChatMessageType, FileStreamChunk},
    transfer::{FileTransferState, TransferStatus},
};

/// 最大文件名长度（字节），防止超长文件名导致的资源耗尽
const MAX_FILENAME_BYTES: usize = 512;

/// 清理文件名，移除路径分隔符和危险字符，防止路径遍历攻击
///
/// # 安全保证
/// - 移除所有 `/` 和 `\` 路径分隔符
/// - 拒绝 `..` 父目录引用
/// - 拒绝空文件名
/// - 拒绝以 `.` 开头的隐藏文件
/// - 截断超长文件名
fn sanitize_filename(filename: &str) -> Option<String> {
    let filename = filename.trim();

    // 拒绝空文件名
    if filename.is_empty() {
        return None;
    }

    // 拒绝以点开头的隐藏文件/目录穿越
    if filename.starts_with('.') {
        return None;
    }

    // 拒绝包含路径分隔符或父目录引用的文件名
    if filename.contains('/') || filename.contains('\\') || filename.contains("..") {
        return None;
    }

    // 截断超长文件名
    let truncated: String = filename.chars().take(MAX_FILENAME_BYTES).collect();

    Some(truncated)
}

/// 验证路径是否在预期的基目录内，防止路径遍历
///
/// 对路径进行规范化（canonicalize）后检查是否以基目录开头。
/// 如果路径尚不存在，则先尝试解析其父目录。
fn validate_path_within_base(path: &Path, base: &Path) -> bool {
    // 如果路径已存在，直接 canonicalize
    if let Ok(canonical) = path.canonicalize() {
        return canonical.starts_with(base);
    }

    // 如果路径不存在，尝试 canonicalize 父目录
    if let Some(parent) = path.parent()
        && let Ok(canonical_parent) = parent.canonicalize()
    {
        // 检查父目录是否在基目录内
        if !canonical_parent.starts_with(base) {
            return false;
        }
        // 检查文件名部分是否安全（不含路径分隔符）
        if let Some(file_name) = path.file_name() {
            let name_str = file_name.to_string_lossy();
            return !name_str.contains('/') && !name_str.contains('\\') && !name_str.contains("..");
        }
    }

    false
}

impl ChatCore {
    /// 处理文件下载请求（接收方发起 -> 发送方响应）
    /// 1. 接收方通过 FileHash 消息获取 file_id，然后发送 FileDownloadRequest
    /// 2. 发送方收到请求后，查找 file_path_map 获取文件路径
    /// 3. 发送方读取文件，分片发送 FileStream 消息
    ///
    /// 断点续传支持：
    /// - 检查本地是否存在部分下载的临时文件
    /// - 如果存在，读取已接收的分片列表并携带在 ChunkResponse 中
    /// - 发送方只发送缺失的分片
    pub(crate) async fn handle_file_download_request(
        &mut self,
        sender_mldsa_pubkey_hex: &str,
        file_hash: [u8; 32],
        save_path: Option<PathBuf>,
    ) {
        let file_hash_hex = hex::encode(file_hash);

        // 检查是否已有相同 file_hash 的传输在进行中
        if self.file_transfers.contains_key(&file_hash_hex) {
            tracing::warn!(
                "File transfer already in progress for file_hash: {}..",
                &file_hash_hex[..16]
            );
            let msg = format!("文件 {}.. 已在下载中", &file_hash_hex[..16]);
            self.send_warning_mpsc(msg).await;
            return;
        }

        // 并发传输限制
        let active_count = self.file_transfers.len();
        if active_count >= crate::transfer::MAX_CONCURRENT_TRANSFERS {
            tracing::warn!(
                "并发传输数已达上限 ({}/{}), 拒绝新的下载请求 file_hash: {}..",
                active_count,
                crate::transfer::MAX_CONCURRENT_TRANSFERS,
                &file_hash_hex[..16]
            );
            let msg = format!(
                "并发下载数已达上限 ({}), 请等待当前下载完成后再试",
                crate::transfer::MAX_CONCURRENT_TRANSFERS
            );
            self.send_warning_mpsc(msg).await;
            return;
        }

        // 确定保存目录
        let download_dir = match &save_path {
            Some(path) => {
                if path.is_dir() {
                    let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.clone());
                    let base = std::fs::canonicalize(&self.download_dir)
                        .unwrap_or_else(|_| self.download_dir.clone());
                    if canonical.starts_with(&base) {
                        canonical
                    } else {
                        tracing::warn!(
                            "save_path {:?} outside download_dir {:?}, falling back to default",
                            canonical, base
                        );
                        self.download_dir.clone()
                    }
                } else if let Some(parent) = path.parent() {
                    let canonical_parent =
                        std::fs::canonicalize(parent).unwrap_or_else(|_| parent.to_path_buf());
                    let base = std::fs::canonicalize(&self.download_dir)
                        .unwrap_or_else(|_| self.download_dir.clone());
                    if canonical_parent.starts_with(&base) {
                        parent.to_path_buf()
                    } else {
                        tracing::warn!(
                            "save_path {:?} outside download_dir {:?}, falling back to default",
                            path, base
                        );
                        self.download_dir.clone()
                    }
                } else {
                    self.download_dir.clone()
                }
            }
            None => self.download_dir.clone(),
        };

        // 断点续传：检查本地是否存在部分下载的临时文件
        let temp_path = download_dir.join(format!(".{}.tmp", &file_hash_hex[..16]));
        let state_path = download_dir.join(format!(".{}.state", &file_hash_hex[..16]));
        let existing_received_chunks: HashSet<u32> = if temp_path.exists() && state_path.exists() {
            match std::fs::read_to_string(&state_path) {
                Ok(content) => {
                    let chunks: HashSet<u32> = content
                        .split(',')
                        .filter_map(|s| s.trim().parse::<u32>().ok())
                        .collect();
                    tracing::info!(
                        "发现部分下载的临时文件: {:?}，已接收 {} 个分片，将尝试断点续传",
                        temp_path,
                        chunks.len()
                    );
                    chunks
                }
                Err(e) => {
                    tracing::warn!("读取分片状态文件失败: {e}，将重新下载所有分片");
                    HashSet::new()
                }
            }
        } else {
            HashSet::new()
        };

        // 创建传输状态
        let received_vec: Vec<u32> = existing_received_chunks.iter().copied().collect();
        let state = FileTransferState {
            file_id: file_hash,
            filename: String::new(),
            total_size: 0,
            total_chunks: 0,
            chunk_size: 0,
            file_hash,
            received_chunks: existing_received_chunks,
            temp_path: temp_path.clone(),
            output_path: PathBuf::new(),
            status: TransferStatus::Requesting,
            started_at: Instant::now(),
        };
        self.file_transfers.insert(file_hash_hex.clone(), state);

        // 发送简化版 DownloadRequest
        let request = crate::message::DownloadRequest { file_hash };
        let data = match postcard::to_allocvec(&request) {
            Ok(d) => d,
            Err(e) => {
                tracing::error!("Failed to serialize DownloadRequest: {e}");
                self.file_transfers.remove(&file_hash_hex);
                return;
            }
        };

        if let Err(e) = self
            .send_text(
                sender_mldsa_pubkey_hex,
                ChatMessageType::FileDownloadRequest,
                data,
            )
            .await
        {
            tracing::error!("Failed to send FileDownloadRequest: {e}");
            self.file_transfers.remove(&file_hash_hex);
            let msg = format!("文件下载请求发送失败: {}", e);
            self.send_warning_mpsc(msg).await;
        } else {
            tracing::info!(
                "File download request sent for file_hash: {}..",
                &file_hash_hex[..16]
            );
        }
    }

    /// 处理文件下载响应（接收方收到发送方的同意）
    pub(crate) async fn handle_file_download_response(
        &mut self,
        response: crate::message::DownloadResponse,
    ) {
        let file_hash_hex = hex::encode(response.file_hash);
        let Some(state) = self.file_transfers.get_mut(&file_hash_hex) else {
            tracing::warn!(
                "收到 DownloadResponse 但无对应传输状态: file_hash={}..",
                &file_hash_hex[..16]
            );
            return;
        };

        let filename = response.filename.unwrap_or_else(|| "unknown".to_string());
        let total_size = response.total_size.unwrap_or(0);
        let total_chunks = response.total_chunks.unwrap_or(0);
        let chunk_size = response.chunk_size.unwrap_or(0);

        state.filename = filename.clone();
        state.total_size = total_size;
        state.total_chunks = total_chunks;
        state.chunk_size = chunk_size;

        // 检查文件是否已存在
        let output_path = self.download_dir.join(&filename);
        if output_path.exists() {
            if let Ok(existing_hash) = crate::transfer::compute_file_hash(&output_path).await {
                if existing_hash == response.file_hash {
                    tracing::info!(
                        "文件已存在且哈希匹配，跳过下载: {:?}",
                        output_path
                    );
                    self.file_transfers.remove(&file_hash_hex);
                    let msg = format!("文件已存在: {}", filename);
                    self.send_log_mpsc(msg).await;
                    return;
                }
            }
        }

        state.output_path = output_path;
        state.status = crate::transfer::TransferStatus::Downloading { received_bytes: 0 };

        tracing::info!(
            "开始接收文件: {}, size={}, chunks={}, chunk_size={}",
            filename,
            total_size,
            total_chunks,
            chunk_size,
        );
    }

    /// 处理文件流分片写入（接收方收到 FileStream 分片后写入临时文件）
    pub(crate) async fn handle_file_stream_chunk(
        &mut self,
        chunk: FileStreamChunk,
    ) -> Result<(), CoreError> {
        let file_id_hex = hex::encode(chunk.file_id);

        // ========== 安全校验：文件名路径遍历防护 ==========
        // 对远端传入的 filename 进行严格校验，防止路径遍历攻击
        // 攻击者可能构造包含 ../ 或绝对路径的 filename 来写入任意位置
        let safe_filename = sanitize_filename(&chunk.filename).ok_or_else(|| {
            let err_msg = format!(
                "拒绝不安全的文件名: '{}' (file_id: {}..)",
                chunk.filename,
                &file_id_hex[..16]
            );
            tracing::error!("{}", err_msg);
            CoreError::FileTransferFailed(err_msg)
        })?;

        // ========== 安全校验：分片元数据一致性 ==========
        // 验证 chunk_index 在有效范围内，防止越界
        if chunk.total_chunks == 0 {
            let err_msg = format!(
                "拒绝无效的分片元数据: total_chunks=0 (file_id: {}..)",
                &file_id_hex[..16]
            );
            tracing::error!("{}", err_msg);
            return Err(CoreError::FileTransferFailed(err_msg));
        }
        if chunk.chunk_index >= chunk.total_chunks {
            let err_msg = format!(
                "拒绝无效的分片索引: chunk_index={} >= total_chunks={} (file_id: {}..)",
                chunk.chunk_index,
                chunk.total_chunks,
                &file_id_hex[..16]
            );
            tracing::error!("{}", err_msg);
            return Err(CoreError::FileTransferFailed(err_msg));
        }
        // 验证 chunk_size 不为零，防止除零或无限循环
        if chunk.chunk_size == 0 {
            let err_msg = format!(
                "拒绝无效的分片大小: chunk_size=0 (file_id: {}..)",
                &file_id_hex[..16]
            );
            tracing::error!("{}", err_msg);
            return Err(CoreError::FileTransferFailed(err_msg));
        }
        // 验证 offset 与 chunk_index 一致（每个分片的 offset = chunk_index * chunk_size）
        let expected_offset = (chunk.chunk_index as u64).saturating_mul(chunk.chunk_size as u64);
        if chunk.offset != expected_offset {
            let err_msg = format!(
                "拒绝 offset 不匹配的分片: chunk_index={}, offset={}, expected_offset={} (file_id: {}..)",
                chunk.chunk_index,
                chunk.offset,
                expected_offset,
                &file_id_hex[..16]
            );
            tracing::error!("{}", err_msg);
            return Err(CoreError::FileTransferFailed(err_msg));
        }
        // 验证 total_size 不为零
        if chunk.total_size == 0 {
            let err_msg = format!(
                "拒绝无效的文件总大小: total_size=0 (file_id: {}..)",
                &file_id_hex[..16]
            );
            tracing::error!("{}", err_msg);
            return Err(CoreError::FileTransferFailed(err_msg));
        }
        // ========== 文件大小限制：防止 DoS 攻击 ==========
        // 拒绝超过 MAX_FILE_SIZE 的文件，防止攻击者通过大文件耗尽磁盘空间
        if chunk.total_size > crate::transfer::MAX_FILE_SIZE {
            let err_msg = format!(
                "拒绝过大的文件: total_size={} > MAX_FILE_SIZE={} (file_id: {}..)",
                chunk.total_size,
                crate::transfer::MAX_FILE_SIZE,
                &file_id_hex[..16]
            );
            tracing::error!("{}", err_msg);
            return Err(CoreError::FileTransferFailed(err_msg));
        }
        // 验证最后一个分片的 offset + decompressed_size <= total_size
        // （非最后一个分片的大小应等于 chunk_size）
        if chunk.is_last {
            // 最后一个分片：解压后大小应 <= chunk_size（可能小于）
            // 但我们需要先解压才能知道实际大小，这里先做粗略检查
            if chunk.offset > chunk.total_size {
                let err_msg = format!(
                    "拒绝最后一个分片: offset={} > total_size={} (file_id: {}..)",
                    chunk.offset,
                    chunk.total_size,
                    &file_id_hex[..16]
                );
                tracing::error!("{}", err_msg);
                return Err(CoreError::FileTransferFailed(err_msg));
            }
        }

        // ========== 超时检查：清理超时的传输 ==========
        // 检查所有活跃传输是否超时，超时的标记为失败并清理
        // 防止因发送方断开连接或网络故障导致传输永远挂起
        let now = Instant::now();
        let timed_out_ids: Vec<String> = self
            .file_transfers
            .iter()
            .filter(|(_, state)| {
                now.duration_since(state.started_at) > crate::transfer::TRANSFER_TIMEOUT
            })
            .map(|(id, _)| id.clone())
            .collect();
        for id in &timed_out_ids {
            if let Some(state) = self.file_transfers.get_mut(id) {
                tracing::warn!(
                    "文件传输超时: file_id={}.., filename={}, started_at={:?}",
                    &hex::encode(state.file_id)[..16],
                    state.filename,
                    state.started_at,
                );
                state.status = TransferStatus::Failed;
                // 清理临时文件和状态文件
                let _ = std::fs::remove_file(&state.temp_path);
                let state_path = self
                    .download_dir
                    .join(format!(".{}.state", &hex::encode(state.file_id)[..16]));
                let _ = std::fs::remove_file(&state_path);
            }
            self.file_transfers.remove(id);
        }
        if !timed_out_ids.is_empty() {
            tracing::info!("已清理 {} 个超时的文件传输", timed_out_ids.len());
        }

        // 获取或创建传输状态
        let is_new = !self.file_transfers.contains_key(&file_id_hex);
        if is_new {
            let temp_path = self
                .download_dir
                .join(format!(".{}.tmp", &file_id_hex[..16]));
            let output_path = self.download_dir.join(&safe_filename);

            // 验证输出路径在 download_dir 内
            if !validate_path_within_base(&output_path, &self.download_dir) {
                let err_msg = format!(
                    "输出路径不在下载目录内: {:?} (file_id: {}..)",
                    output_path,
                    &file_id_hex[..16]
                );
                tracing::error!("{}", err_msg);
                return Err(CoreError::FileTransferFailed(err_msg));
            }

            let state = FileTransferState {
                file_id: chunk.file_id,
                filename: safe_filename.clone(),
                total_size: chunk.total_size,
                total_chunks: chunk.total_chunks,
                chunk_size: chunk.chunk_size,
                file_hash: chunk.file_hash,
                received_chunks: HashSet::new(),
                temp_path: temp_path.clone(),
                output_path,
                status: TransferStatus::Downloading { received_bytes: 0 },
                started_at: Instant::now(),
            };
            self.file_transfers.insert(file_id_hex.clone(), state);
        }

        // 断点续传：如果该分片已接收过，跳过（幂等处理）
        // 但仍需发送进度事件到前端，让 UI 保持更新
        if let Some(state) = self.file_transfers.get(&file_id_hex)
            && state.received_chunks.contains(&chunk.chunk_index)
        {
            tracing::debug!(
                "分片 {} 已接收，跳过（断点续传幂等处理）",
                chunk.chunk_index
            );
            // 发送进度事件（结构化数据，上层负责序列化为 JSON）
            let progress_event = FileTransferProgress {
                filename: state.filename.clone(),
                chunk_index: chunk.chunk_index,
                total_chunks: state.total_chunks,
                received_bytes: state.received_chunks.len() as u64 * chunk.chunk_size as u64,
                total_size: state.total_size,
                status: "downloading".to_string(),
            };
            if let Err(e) = self
                .tx_message
                .try_send(MessageEvent::FileTransferProgress(progress_event))
            {
                tracing::error!("Failed to send file transfer progress: {e}");
            }
            return Ok(());
        }

        // 解压分片数据（chunk_data 是压缩后的分片数据）
        let decompressed = crate::compression::decompress(&chunk.chunk_data).await?;

        // ========== 安全校验：解压后数据大小验证 ==========
        // 防止压缩炸弹攻击：验证解压后的大小是否合理
        let decompressed_size = decompressed.len() as u64;
        if chunk.is_last {
            // 最后一个分片：解压后大小应 <= chunk_size，且 offset + size <= total_size
            if decompressed_size > chunk.chunk_size as u64 {
                let err_msg = format!(
                    "拒绝过大的最后一个分片: decompressed_size={} > chunk_size={} (file_id: {}..)",
                    decompressed_size,
                    chunk.chunk_size,
                    &file_id_hex[..16]
                );
                tracing::error!("{}", err_msg);
                return Err(CoreError::FileTransferFailed(err_msg));
            }
            if chunk.offset + decompressed_size > chunk.total_size {
                let err_msg = format!(
                    "拒绝导致文件过大的最后一个分片: offset={} + decompressed_size={} > total_size={} (file_id: {}..)",
                    chunk.offset,
                    decompressed_size,
                    chunk.total_size,
                    &file_id_hex[..16]
                );
                tracing::error!("{}", err_msg);
                return Err(CoreError::FileTransferFailed(err_msg));
            }
        } else {
            // 非最后一个分片：解压后大小应 == chunk_size
            if decompressed_size != chunk.chunk_size as u64 {
                let err_msg = format!(
                    "拒绝大小不匹配的分片: decompressed_size={} != chunk_size={} (file_id: {}..)",
                    decompressed_size,
                    chunk.chunk_size,
                    &file_id_hex[..16]
                );
                tracing::error!("{}", err_msg);
                return Err(CoreError::FileTransferFailed(err_msg));
            }
        }

        // 从状态中获取临时文件路径（复用已存储的路径，避免重复构造）
        let temp_path = self
            .file_transfers
            .get(&file_id_hex)
            .map(|s| s.temp_path.clone())
            .unwrap_or_else(|| {
                self.download_dir
                    .join(format!(".{}.tmp", &file_id_hex[..16]))
            });

        // 写入临时文件（offset-based 写入，支持断点续传）
        // 注意：不使用 truncate(true)，避免清空已有数据
        // 使用 write(true) 模式，通过 seek 定位到指定 offset 写入
        use tokio::io::{AsyncSeekExt, AsyncWriteExt};
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .truncate(false) // 不截断，保留已有数据（断点续传关键）
            .write(true)
            .open(&temp_path)
            .await?;

        file.seek(std::io::SeekFrom::Start(chunk.offset)).await?;
        file.write_all(&decompressed).await?;
        file.flush().await?;
        drop(file);

        // 更新传输状态 - 先提取需要的数据，避免借用冲突
        let (is_complete, filename, total_chunks, total_size, received_bytes) =
            if let Some(state) = self.file_transfers.get_mut(&file_id_hex) {
                state.received_chunks.insert(chunk.chunk_index);
                // 精确计算已接收字节数：最后一个分片可能小于 chunk_size
                let last_chunk_index = state.total_chunks - 1;
                let last_chunk_size = if state.total_size % state.chunk_size as u64 == 0 {
                    state.chunk_size as u64
                } else {
                    state.total_size % state.chunk_size as u64
                };
                let received_bytes = state
                    .received_chunks
                    .iter()
                    .map(|&idx| {
                        if idx == last_chunk_index {
                            last_chunk_size
                        } else {
                            state.chunk_size as u64
                        }
                    })
                    .sum();
                state.status = TransferStatus::Downloading { received_bytes };

                // 持久化已接收分片列表到状态文件（断点续传支持）
                let state_path = self
                    .download_dir
                    .join(format!(".{}.state", &file_id_hex[..16]));
                let chunks_csv: String = state
                    .received_chunks
                    .iter()
                    .map(|c| c.to_string())
                    .collect::<Vec<_>>()
                    .join(",");
                if let Err(e) = std::fs::write(&state_path, &chunks_csv) {
                    tracing::warn!("写入分片状态文件失败: {e}");
                }

                let is_complete = state.received_chunks.len() as u32 >= state.total_chunks;
                let filename = state.filename.clone();
                let total_chunks = state.total_chunks;
                let total_size = state.total_size;

                (
                    is_complete,
                    filename,
                    total_chunks,
                    total_size,
                    received_bytes,
                )
            } else {
                return Ok(());
            };

        // 发送进度事件（结构化数据，上层负责序列化为 JSON）
        let progress_event = FileTransferProgress {
            filename: filename.clone(),
            chunk_index: chunk.chunk_index,
            total_chunks,
            received_bytes,
            total_size,
            status: "downloading".to_string(),
        };
        if let Err(e) = self
            .tx_message
            .try_send(MessageEvent::FileTransferProgress(progress_event))
        {
            tracing::error!("Failed to send file transfer progress: {e}");
        }

        // 检查是否所有分片都已接收
        if is_complete {
            // 验证完整文件哈希（从传输状态中获取期望的 file_hash）
            let expected_file_hash = self
                .file_transfers
                .get(&file_id_hex)
                .map(|s| s.file_hash)
                .unwrap_or([0u8; 32]);

            // 计算已下载文件的 SHA256 哈希
            let computed_file_hash = match crate::transfer::compute_file_hash(&temp_path).await {
                Ok(hash) => hash,
                Err(e) => {
                    tracing::error!("计算下载文件哈希失败: {e}");
                    return Err(CoreError::FileTransferFailed(format!(
                        "计算文件哈希失败: {}",
                        e
                    )));
                }
            };

            // 恒定时间比较，防止时序攻击
            if !crate::crypto::constant_time_compare(&computed_file_hash, &expected_file_hash) {
                tracing::error!(
                    "文件哈希验证失败: expected={:?}, computed={:?}",
                    expected_file_hash,
                    computed_file_hash
                );
                // 删除损坏的临时文件
                let _ = tokio::fs::remove_file(&temp_path).await;
                return Err(CoreError::FileHashMismatch);
            }

            tracing::info!("文件哈希验证通过");

            // 生成最终输出路径，如果文件已存在则添加数字后缀避免覆盖
            let output_path = self.download_dir.join(&filename);
            let final_path = if output_path.exists() {
                let stem = output_path
                    .file_stem()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_else(|| "file".to_string());
                let ext = output_path
                    .extension()
                    .map(|e| format!(".{}", e.to_string_lossy()))
                    .unwrap_or_default();
                let mut counter = 1;
                loop {
                    let candidate = self
                        .download_dir
                        .join(format!("{} ({}){}", stem, counter, ext));
                    if !candidate.exists() {
                        break candidate;
                    }
                    counter += 1;
                }
            } else {
                output_path
            };

            // 重命名临时文件为最终文件名
            if let Err(e) = tokio::fs::rename(&temp_path, &final_path).await {
                tracing::error!("Failed to rename temp file: {e}");
                return Err(CoreError::FileRenameFailed(format!(
                    "重命名临时文件失败: {}",
                    e
                )));
            }

            tracing::info!("File download completed: {} -> {:?}", filename, final_path);

            // 发送完成事件
            let msg = format!("文件 {} 下载完成，已保存到 {:?}", filename, final_path);
            self.send_log_mpsc(msg).await;

            // 发送完成进度事件（结构化数据，上层负责序列化为 JSON）
            let complete_event = FileTransferProgress {
                filename: filename.clone(),
                chunk_index: total_chunks,
                total_chunks,
                received_bytes: total_size,
                total_size,
                status: "completed".to_string(),
            };
            if let Err(e) = self
                .tx_message
                .try_send(MessageEvent::FileTransferProgress(complete_event))
            {
                tracing::error!("Failed to send file transfer complete: {e}");
            }

            // 清理传输状态和临时文件
            self.file_transfers.remove(&file_id_hex);
            // 清理分片状态文件
            let state_path = self
                .download_dir
                .join(format!(".{}.state", &file_id_hex[..16]));
            let _ = std::fs::remove_file(&state_path);
        }

        Ok(())
    }
}
