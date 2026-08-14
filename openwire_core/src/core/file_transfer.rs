use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::{
    command::{FileTransferProgress, MessageEvent, TransferProgressStatus},
    core::ChatCore,
    crypto::constant_time_compare,
    error::CoreError,
    message::{ChatMessageType, FileStreamChunk},
    transfer::{FileTransferState, TransferStatus},
};
use sha2::Digest;

const MAX_FILENAME_BYTES: usize = 512;

fn sanitize_filename(filename: &str) -> Option<String> {
    let f = filename.trim();
    if f.is_empty() || f.starts_with('.') || f.contains('/') || f.contains('\\') || f.contains("..")
    {
        return None;
    }
    let sanitized: String = f
        .chars()
        .filter(|&c| c.is_alphanumeric() || c == '-' || c == '_' || c == '.' || c == ' ' || c == '(' || c == ')')
        .take(MAX_FILENAME_BYTES)
        .collect();
    if sanitized.is_empty() { None } else { Some(sanitized) }
}

fn validate_path_within_base(path: &Path, base: &Path) -> bool {
    if let Ok(canonical) = path.canonicalize() {
        return canonical.starts_with(base);
    }
    if let Some(parent) = path.parent()
        && let Ok(canonical_parent) = parent.canonicalize()
    {
        if !canonical_parent.starts_with(base) {
            return false;
        }
        if let Some(file_name) = path.file_name() {
            let s = file_name.to_string_lossy();
            return !s.contains('/') && !s.contains('\\') && !s.contains("..");
        }
    }
    false
}

impl ChatCore {
    fn downloads_dir(&self) -> PathBuf {
        self.data_dir.join("downloads")
    }

    /// 规范化并验证保存路径在 data_dir 内，否则回退到默认下载目录
    async fn resolve_download_dir(&mut self, save_path: &Path) -> PathBuf {
        let raw = if save_path.is_dir() {
            save_path.to_path_buf()
        } else if let Some(p) = save_path.parent() {
            p.to_path_buf()
        } else {
            return self.downloads_dir();
        };

        let _ = std::fs::create_dir_all(&raw);
        let canonical = std::fs::canonicalize(&raw).unwrap_or_else(|_| self.downloads_dir());
        let data = std::fs::canonicalize(&self.data_dir).unwrap_or_else(|_| self.data_dir.clone());
        if canonical.starts_with(&data) {
            return canonical;
        }
        let fallback = self.downloads_dir();
        let _ = std::fs::create_dir_all(&fallback);
        tracing::warn!(
            "保存路径 {:?} 不在 data_dir 内，回退到默认目录 {:?}",
            canonical,
            fallback
        );
        self.send_warning_mpsc(format!(
            "保存路径不在 data_dir 内，已回退到默认下载目录 {:?}",
            fallback
        ))
        .await;
        fallback
    }

    /// 检查断点续传状态：读取已接收的分片列表
    fn load_resume_chunks(hash_hex: &str, dir: &Path) -> (PathBuf, PathBuf, HashSet<u32>) {
        let temp = dir.join(format!(".{}.tmp", &hash_hex[..16]));
        let state = dir.join(format!(".{}.state", &hash_hex[..16]));
        let chunks = if temp.exists() && state.exists() {
            match std::fs::read_to_string(&state) {
                Ok(c) => {
                    let chunks: HashSet<u32> =
                        c.split(',').filter_map(|s| s.trim().parse().ok()).collect();
                    tracing::info!("断点续传: {:?} 已接收 {} 个分片", temp, chunks.len());
                    chunks
                }
                Err(e) => {
                    tracing::warn!("读取状态文件失败: {e}，重新下载");
                    HashSet::new()
                }
            }
        } else {
            HashSet::new()
        };
        (temp, state, chunks)
    }
}

impl ChatCore {
    pub(crate) async fn handle_file_download_request(
        &mut self,
        sender_mldsa_pubkey_hex: &str,
        file_hash: [u8; 32],
        save_path: PathBuf,
    ) {
        let hash_hex = hex::encode(file_hash);

        if self.file_transfers.contains_key(&hash_hex) {
            tracing::warn!("下载已在进行: {}..", &hash_hex[..16]);
            self.send_warning_mpsc(format!("文件 {}.. 已在下载中", &hash_hex[..16]))
                .await;
            return;
        }
        if self.outbound_file_count >= crate::transfer::MAX_CONCURRENT_TRANSFERS {
            let max = crate::transfer::MAX_CONCURRENT_TRANSFERS;
            tracing::warn!("并发下载数已达上限 {max}");
            self.send_warning_mpsc(format!("并发下载数已达上限 ({max})，请等待当前下载完成"))
                .await;
            return;
        }

        let dir = self.resolve_download_dir(&save_path).await;
        let (temp_path, _state_path, existing) = Self::load_resume_chunks(&hash_hex, &dir);

        let state = FileTransferState {
            file_id: file_hash,
            filename: String::new(),
            total_size: 0,
            total_chunks: 0,
            chunk_size: 0,
            file_hash,
            received_chunks: existing,
            temp_path: temp_path.clone(),
            output_path: PathBuf::new(),
            status: TransferStatus::Requesting,
            started_at: Instant::now(),
        };
        self.file_transfers.insert(hash_hex.clone(), state);
        self.outbound_file_count += 1;

        let request = crate::message::DownloadRequest { file_hash };
        let data = match postcard::to_allocvec(&request) {
            Ok(d) => d,
            Err(e) => {
                tracing::error!("序列化 DownloadRequest 失败: {e}");
                self.file_transfers.remove(&hash_hex);
                self.outbound_file_count = self.outbound_file_count.saturating_sub(1);
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
            tracing::error!("发送 FileDownloadRequest 失败: {e}");
            self.file_transfers.remove(&hash_hex);
            self.outbound_file_count = self.outbound_file_count.saturating_sub(1);
            self.send_warning_mpsc(format!("文件下载请求发送失败: {e}"))
                .await;
        }
    }

    pub(crate) async fn handle_file_download_response(
        &mut self,
        response: crate::message::DownloadResponse,
    ) {
        let hash_hex = hex::encode(response.file_hash);
        let raw_filename = response.filename.as_deref().unwrap_or("unknown");
        let safe = sanitize_filename(raw_filename).unwrap_or_else(|| "unknown".to_string());
        let downloads_dir = self.downloads_dir();
        let output_path = downloads_dir.join(&safe);
        if !validate_path_within_base(&output_path, &downloads_dir) {
            tracing::warn!("DownloadResponse 文件名不安全: {}", raw_filename);
            self.send_warning_mpsc(format!("下载响应文件名校验失败: {}", raw_filename)).await;
            self.file_transfers.remove(&hash_hex);
            self.outbound_file_count = self.outbound_file_count.saturating_sub(1);
            return;
        }
        let Some(state) = self.file_transfers.get_mut(&hash_hex) else {
            tracing::warn!("DownloadResponse 无对应状态: {}..", &hash_hex[..16]);
            return;
        };
        state.filename = safe;
        state.total_size = response.total_size.unwrap_or(0);
        state.total_chunks = response.total_chunks.unwrap_or(0);
        state.chunk_size = response.chunk_size.unwrap_or(0);

        if output_path.exists()
            && (crate::transfer::compute_file_hash(&output_path)
                .await
                .ok() == Some(response.file_hash))
        {
            tracing::info!("文件已存在且哈希匹配: {:?}", output_path);
            let filename = state.filename.clone();
            let file_transfers = &mut self.file_transfers;
            file_transfers.remove(&hash_hex);
            self.outbound_file_count = self.outbound_file_count.saturating_sub(1);
            self.send_log_mpsc(format!("文件已存在: {}", filename))
                .await;
            return;
        }
        state.output_path = output_path;
        state.status = TransferStatus::Downloading { received_bytes: 0 };
    }

    /// 扫描并清理超时传输
    fn scan_timeout_transfers(&mut self) {
        let now = Instant::now();
        if now.duration_since(self.last_file_timeout_scan) <= std::time::Duration::from_secs(60) {
            return;
        }
        self.last_file_timeout_scan = now;
        let timed_out: Vec<String> = self
            .file_transfers
            .iter()
            .filter(|(_, s)| now.duration_since(s.started_at) > crate::transfer::TRANSFER_TIMEOUT)
            .map(|(id, _)| id.clone())
            .collect();
        for id in &timed_out {
            let state_path = self.downloads_dir().join(format!(
                ".{}.state",
                &hex::encode(
                    self.file_transfers
                        .get(id)
                        .map(|s| s.file_id)
                        .unwrap_or([0u8; 32])
                )[..16]
            ));
            if let Some(s) = self.file_transfers.get_mut(id) {
                s.status = TransferStatus::Failed;
                let _ = std::fs::remove_file(&s.temp_path);
                let _ = std::fs::remove_file(&state_path);
            }
            self.file_transfers.remove(id);
            self.outbound_file_count = self.outbound_file_count.saturating_sub(1);
            self.inbound_file_count = self.inbound_file_count.saturating_sub(1);
        }
        if !timed_out.is_empty() {
            tracing::info!("已清理 {} 个超时传输", timed_out.len());
        }
    }

    /// 校验分片元数据合法性，返回安全的文件名
    fn validate_chunk(chunk: &FileStreamChunk) -> Result<String, CoreError> {
        let fid = &hex::encode(chunk.file_id)[..16];
        let e = |msg| CoreError::FileTransferFailed(format!("{msg} (file_id: {fid})"));

        let safe = sanitize_filename(&chunk.filename)
            .ok_or_else(|| e(format!("不安全文件名: '{}'", chunk.filename)))?;

        if chunk.total_chunks == 0 {
            return Err(e("total_chunks=0".to_string()));
        }
        if chunk.chunk_index >= chunk.total_chunks {
            return Err(e(format!(
                "分片索引越界: {}/{}",
                chunk.chunk_index, chunk.total_chunks
            )));
        }
        if chunk.chunk_size == 0 {
            return Err(e("chunk_size=0".to_string()));
        }
        let expected = (chunk.chunk_index as u64).saturating_mul(chunk.chunk_size as u64);
        if chunk.offset != expected {
            return Err(e(format!(
                "offset 不匹配: {} != {}",
                chunk.offset, expected
            )));
        }
        if chunk.total_size == 0 {
            return Err(e("total_size=0".to_string()));
        }
        if chunk.total_size > crate::transfer::MAX_FILE_SIZE {
            return Err(e(format!(
                "文件过大: {} > {}",
                chunk.total_size,
                crate::transfer::MAX_FILE_SIZE
            )));
        }
        if chunk.is_last && chunk.offset > chunk.total_size {
            return Err(e(format!(
                "末分片 offset>total_size: {}>{}",
                chunk.offset, chunk.total_size
            )));
        }

        Ok(safe)
    }

    /// 发送进度事件到前端
    fn emit_progress(
        &self,
        filename: &str,
        chunk_idx: u32,
        total: u32,
        recv: u64,
        total_sz: u64,
        status: TransferProgressStatus,
    ) {
        let ev = FileTransferProgress {
            filename: filename.to_string(),
            chunk_index: chunk_idx,
            total_chunks: total,
            received_bytes: recv,
            total_size: total_sz,
            status,
        };
        if let Err(e) = self
            .tx_message
            .try_send(MessageEvent::FileTransferProgress(ev))
        {
            tracing::warn!("发送进度事件失败: {e}");
        }
    }

    /// 下载完成：验证哈希、重命名、清理
    async fn complete_transfer(
        &mut self,
        file_id_hex: &str,
        filename: &str,
        total_chunks: u32,
        total_size: u64,
    ) -> Result<(), CoreError> {
        let expected = self
            .file_transfers
            .get(file_id_hex)
            .map(|s| s.file_hash)
            .unwrap_or([0u8; 32]);
        let temp = self
            .downloads_dir()
            .join(format!(".{}.tmp", &file_id_hex[..16]));

        let computed = crate::transfer::compute_file_hash(&temp)
            .await
            .map_err(|e| CoreError::FileTransferFailed(format!("计算哈希失败: {e}")))?;
        if !constant_time_compare(&computed, &expected) {
            let _ = tokio::fs::remove_file(&temp).await;
            return Err(CoreError::FileHashMismatch);
        }
        tracing::info!("文件哈希验证通过");

        let output = self.downloads_dir().join(filename);
        let final_path = if output.exists() {
            let stem = output
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| "file".to_string());
            let ext = output
                .extension()
                .map(|e| format!(".{}", e.to_string_lossy()))
                .unwrap_or_default();
            (1..10000)
                .map(|i| {
                    self.downloads_dir()
                        .join(format!("{} ({}){}", stem, i, ext))
                })
                .find(|p| !p.try_exists().unwrap_or(true))
                .unwrap_or_else(|| {
                    let ts = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis();
                    self.downloads_dir().join(format!("{} ({}).{}", stem, ts, ext))
                })
        } else {
            output
        };
        tokio::fs::rename(&temp, &final_path)
            .await
            .map_err(|e| CoreError::FileRenameFailed(format!("重命名失败: {e}")))?;

        tracing::info!("下载完成: {} -> {:?}", filename, final_path);
        self.send_log_mpsc(format!("文件 {} 下载完成", filename))
            .await;
        self.emit_progress(
            filename,
            total_chunks,
            total_chunks,
            total_size,
            total_size,
            TransferProgressStatus::Completed,
        );
        self.file_transfers.remove(file_id_hex);
        self.inbound_file_count = self.inbound_file_count.saturating_sub(1);
        let _ = std::fs::remove_file(
            self.downloads_dir()
                .join(format!(".{}.state", &file_id_hex[..16])),
        );
        Ok(())
    }

    pub(crate) async fn handle_file_stream_chunk(
        &mut self,
        chunk: FileStreamChunk,
    ) -> Result<(), CoreError> {
        let id_hex = hex::encode(chunk.file_id);
        let safe_name = Self::validate_chunk(&chunk)?;
        self.scan_timeout_transfers();

        let is_new = !self.file_transfers.contains_key(&id_hex);
        if is_new {
            if self.inbound_file_count >= crate::transfer::MAX_CONCURRENT_TRANSFERS {
                return Err(CoreError::FileTransferFailed(format!(
                    "入站并发传输数已达上限 ({})", crate::transfer::MAX_CONCURRENT_TRANSFERS
                )));
            }
            let temp = self.downloads_dir().join(format!(".{}.tmp", &id_hex[..16]));
            let out = self.downloads_dir().join(&safe_name);
            if !validate_path_within_base(&out, &self.downloads_dir()) {
                return Err(CoreError::FileTransferFailed(format!(
                    "输出路径不在下载目录内: {:?}",
                    out
                )));
            }
            self.file_transfers.insert(
                id_hex.clone(),
                FileTransferState {
                    file_id: chunk.file_id,
                    filename: safe_name.clone(),
                    total_size: chunk.total_size,
                    total_chunks: chunk.total_chunks,
                    chunk_size: chunk.chunk_size,
                    file_hash: chunk.file_hash,
                    received_chunks: HashSet::new(),
                    temp_path: temp.clone(),
                    output_path: out,
                    status: TransferStatus::Downloading { received_bytes: 0 },
                    started_at: Instant::now(),
                },
            );
            self.inbound_file_count += 1;
        }

        if let Some(state) = self.file_transfers.get(&id_hex)
            && state.received_chunks.contains(&chunk.chunk_index)
        {
            let recv = state.received_bytes();
            self.emit_progress(
                &state.filename,
                chunk.chunk_index,
                state.total_chunks,
                recv,
                state.total_size,
                TransferProgressStatus::Downloading,
            );
            return Ok(());
        }

        let decompressed = crate::compression::decompress(&chunk.chunk_data).await?;
        let dsz = decompressed.len() as u64;

        if (chunk.is_last && dsz > chunk.chunk_size as u64)
            || (!chunk.is_last && dsz != chunk.chunk_size as u64)
        {
            return Err(CoreError::FileTransferFailed(format!(
                "解压后大小不匹配: {dsz} != {}",
                chunk.chunk_size
            )));
        }
        if chunk.is_last && chunk.offset + dsz > chunk.total_size {
            return Err(CoreError::FileTransferFailed(format!(
                "末分片越界: {} + {} > {}",
                chunk.offset, dsz, chunk.total_size
            )));
        }

        let computed_hash = sha2::Sha256::digest(&decompressed);
        if !constant_time_compare(&computed_hash, &chunk.chunk_hash) {
            return Err(CoreError::FileTransferFailed(format!(
                "分片哈希校验失败: chunk={}",
                chunk.chunk_index
            )));
        }

        let temp = self
            .file_transfers
            .get(&id_hex)
            .map(|s| s.temp_path.clone())
            .unwrap_or_else(|| self.downloads_dir().join(format!(".{}.tmp", &id_hex[..16])));

        use tokio::io::{AsyncSeekExt, AsyncWriteExt};
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&temp)
            .await?;
        file.seek(std::io::SeekFrom::Start(chunk.offset)).await?;
        file.write_all(&decompressed).await?;
        file.flush().await?;
        drop(file);

        let dir = self.downloads_dir();
        let (is_complete, fname, total_chunks, total_size, recv) =
            if let Some(state) = self.file_transfers.get_mut(&id_hex) {
                state.received_chunks.insert(chunk.chunk_index);
                let recv = state.received_bytes();
                state.status = TransferStatus::Downloading {
                    received_bytes: recv,
                };
                state.persist_state(&dir.join(format!(".{}.state", &id_hex[..16])));
                (
                    state.received_chunks.len() as u32 >= state.total_chunks,
                    state.filename.clone(),
                    state.total_chunks,
                    state.total_size,
                    recv,
                )
            } else {
                return Ok(());
            };

        self.emit_progress(
            &fname,
            chunk.chunk_index,
            total_chunks,
            recv,
            total_size,
            TransferProgressStatus::Downloading,
        );

        if is_complete {
            self.complete_transfer(&id_hex, &fname, total_chunks, total_size)
                .await?;
        }
        Ok(())
    }
}
