use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Instant;

use sha2::Digest;
use tokio::sync::mpsc;

use crate::{
    command::{FileTransferProgress, MessageEvent, TransferProgressStatus},
    crypto::constant_time_compare,
    error::CoreError,
    message::FileStreamChunk,
    transfer::{FileTransferState, TransferStatus, MAX_CONCURRENT_TRANSFERS},
};

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
    if sanitized.is_empty() {
        return None;
    }
    let stem = sanitized.split('.').next().unwrap_or(&sanitized);
    const RESERVED: [&str; 22] = [
        "con", "prn", "aux", "nul",
        "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8", "com9",
        "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
    ];
    if RESERVED.contains(&stem.to_ascii_lowercase().as_str()) {
        return None;
    }
    Some(sanitized)
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

fn load_resume_chunks(hash_hex: &str, dir: &Path) -> (PathBuf, HashSet<u32>) {
    let temp = dir.join(format!(".{}.tmp", hash_hex));
    let state = dir.join(format!(".{}.state", hash_hex));
    let chunks = if temp.exists() && state.exists() {
        match std::fs::read_to_string(&state) {
            Ok(c) => c.split(',').filter_map(|s| s.trim().parse().ok()).collect(),
            Err(_) => HashSet::new(),
        }
    } else {
        HashSet::new()
    };
    (temp, chunks)
}

pub(crate) struct FileTransferManager {
    pub file_transfers: HashMap<String, FileTransferState>,
    pub outbound_file_count: usize,
    pub inbound_file_count: usize,
    last_file_timeout_scan: Instant,
    data_dir: PathBuf,
    event_sink: mpsc::Sender<MessageEvent>,
}

impl FileTransferManager {
    pub fn new(data_dir: PathBuf, event_sink: mpsc::Sender<MessageEvent>) -> Self {
        Self {
            file_transfers: HashMap::new(),
            outbound_file_count: 0,
            inbound_file_count: 0,
            last_file_timeout_scan: Instant::now(),
            data_dir,
            event_sink,
        }
    }

    fn downloads_dir(&self) -> PathBuf {
        self.data_dir.join("downloads")
    }

    fn send_warning(&self, msg: String) {
        let _ = self.event_sink.try_send(MessageEvent::Warning(msg));
    }

    fn send_log(&self, msg: String) {
        let _ = self.event_sink.try_send(MessageEvent::Log(msg));
    }

    fn emit_progress(&self, filename: &str, chunk_idx: u32, total: u32, recv: u64, total_sz: u64, status: TransferProgressStatus) {
        let ev = FileTransferProgress {
            filename: filename.to_string(),
            chunk_index: chunk_idx,
            total_chunks: total,
            received_bytes: recv,
            total_size: total_sz,
            status,
        };
        let _ = self.event_sink.try_send(MessageEvent::FileTransferProgress(ev));
    }

    fn release_transfer(&mut self, file_id_hex: &str) {
        if let Some(state) = self.file_transfers.remove(file_id_hex) {
            if state.is_outbound {
                self.outbound_file_count = self.outbound_file_count.saturating_sub(1);
            } else {
                self.inbound_file_count = self.inbound_file_count.saturating_sub(1);
            }
        }
    }

    /// 准备下载请求：检查并发限制、创建状态、序列化请求。
    /// 返回 (hash_hex, 序列化的 DownloadRequest 字节)。
    /// 调用方负责发送请求字节，并在发送失败时调用 cancel_download。
    pub fn try_start_download(&mut self, file_hash: [u8; 32], save_path: &Path) -> Result<(String, Vec<u8>), String> {
        let hash_hex = hex::encode(file_hash);

        if self.file_transfers.contains_key(&hash_hex) {
            return Err(format!("文件 {}.. 已在下载中", &hash_hex[..16]));
        }
        if self.outbound_file_count >= MAX_CONCURRENT_TRANSFERS {
            return Err(format!("并发下载数已达上限 ({MAX_CONCURRENT_TRANSFERS})，请等待当前下载完成"));
        }

        let dir = self.resolve_download_dir(save_path);
        let (temp_path, existing) = load_resume_chunks(&hash_hex, &dir);

        let state = FileTransferState {
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
            is_outbound: true,
        };
        self.file_transfers.insert(hash_hex.clone(), state);
        self.outbound_file_count += 1;

        let request = crate::message::DownloadRequest { file_hash };
        let data = postcard::to_allocvec(&request).map_err(|e| {
            self.file_transfers.remove(&hash_hex);
            self.outbound_file_count = self.outbound_file_count.saturating_sub(1);
            format!("序列化 DownloadRequest 失败: {e}")
        })?;
        Ok((hash_hex, data))
    }

    /// 下载请求发送失败或对端拒绝时回滚状态。
    /// 仅当传输条目存在时递减计数，避免超时扫描已释放后重复递减。
    pub fn cancel_download(&mut self, hash_hex: &str) {
        if self.file_transfers.remove(hash_hex).is_some() {
            self.outbound_file_count = self.outbound_file_count.saturating_sub(1);
        }
    }

    pub async fn handle_download_response(&mut self, response: crate::message::DownloadResponse) {
        let hash_hex = hex::encode(response.file_hash);
        let raw_filename = response.filename.as_deref().unwrap_or("unknown");
        let safe = sanitize_filename(raw_filename).unwrap_or_else(|| "unknown".to_string());
        let downloads_dir = self.downloads_dir();
        let output_path = downloads_dir.join(&safe);
        if !validate_path_within_base(&output_path, &downloads_dir) {
            tracing::warn!("DownloadResponse 文件名不安全: {}", raw_filename);
            self.send_warning(format!("下载响应文件名校验失败: {}", raw_filename));
            self.cancel_download(&hash_hex);
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

        if state.total_size > 0 && (state.chunk_size == 0 || state.total_chunks == 0) {
            let (tsz, csz, tch) = (state.total_size, state.chunk_size, state.total_chunks);
            tracing::warn!("DownloadResponse 字段不一致: total_size={}, chunk_size={}, total_chunks={}", tsz, csz, tch);
            let _ = state;
            self.cancel_download(&hash_hex);
            self.send_warning(format!("下载响应格式异常: 文件大小={}, 分片大小={}, 总分片数={}", tsz, csz, tch));
            return;
        }

        if output_path.exists()
            && (crate::transfer::compute_file_hash(&output_path).await.ok() == Some(response.file_hash))
        {
            tracing::info!("文件已存在且哈希匹配: {:?}", output_path);
            let filename = state.filename.clone();
            let _ = state;
            self.cancel_download(&hash_hex);
            self.send_log(format!("文件已存在: {}", filename));
            return;
        }
        state.output_path = output_path;
        state.status = TransferStatus::Downloading { received_bytes: 0 };
    }

    pub fn scan_timeout_transfers(&mut self) {
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
            let state_path = self.downloads_dir().join(format!(".{}.state", hex::encode(
                self.file_transfers.get(id).map(|s| s.file_hash).unwrap_or([0u8; 32])
            )));
            if let Some(s) = self.file_transfers.get_mut(id) {
                s.status = TransferStatus::Failed;
                let _ = std::fs::remove_file(&s.temp_path);
                let _ = std::fs::remove_file(&state_path);
            }
            if self.file_transfers.get(id).map(|s| s.is_outbound).unwrap_or(true) {
                self.outbound_file_count = self.outbound_file_count.saturating_sub(1);
            } else {
                self.inbound_file_count = self.inbound_file_count.saturating_sub(1);
            }
            self.file_transfers.remove(id);
        }
        if !timed_out.is_empty() {
            tracing::info!("已清理 {} 个超时传输", timed_out.len());
        }
    }

    fn validate_chunk(chunk: &FileStreamChunk) -> Result<String, CoreError> {
        let fid = &hex::encode(chunk.file_hash)[..16];
        let e = |msg| CoreError::FileTransferFailed(format!("{msg} (file_id: {fid})"));

        let safe = sanitize_filename(&chunk.filename)
            .ok_or_else(|| e(format!("不安全文件名: '{}'", chunk.filename)))?;

        if chunk.total_chunks == 0 {
            return Err(e("total_chunks=0".to_string()));
        }
        if chunk.chunk_index >= chunk.total_chunks {
            return Err(e(format!("分片索引越界: {}/{}", chunk.chunk_index, chunk.total_chunks)));
        }
        if chunk.chunk_size == 0 {
            return Err(e("chunk_size=0".to_string()));
        }
        let expected = (chunk.chunk_index as u64).saturating_mul(chunk.chunk_size as u64);
        if chunk.offset != expected {
            return Err(e(format!("offset 不匹配: {} != {}", chunk.offset, expected)));
        }
        if chunk.total_size == 0 {
            return Err(e("total_size=0".to_string()));
        }
        if chunk.total_size > crate::transfer::MAX_FILE_SIZE {
            return Err(e(format!("文件过大: {} > {}", chunk.total_size, crate::transfer::MAX_FILE_SIZE)));
        }
        if chunk.is_last && chunk.offset > chunk.total_size {
            return Err(e(format!("末分片 offset>total_size: {}>{}", chunk.offset, chunk.total_size)));
        }
        Ok(safe)
    }

    pub async fn handle_file_stream_chunk(&mut self, chunk: FileStreamChunk) -> Result<(), CoreError> {
        let id_hex = hex::encode(chunk.file_hash);
        let safe_name = Self::validate_chunk(&chunk)?;
        self.scan_timeout_transfers();

        let is_new = !self.file_transfers.contains_key(&id_hex);
        if is_new {
            if self.inbound_file_count >= MAX_CONCURRENT_TRANSFERS {
                return Err(CoreError::FileTransferFailed(format!("入站并发传输数已达上限 ({MAX_CONCURRENT_TRANSFERS})")));
            }
            let temp = self.downloads_dir().join(format!(".{}.tmp", id_hex));
            let out = self.downloads_dir().join(&safe_name);
            if !validate_path_within_base(&out, &self.downloads_dir()) {
                return Err(CoreError::FileTransferFailed(format!("输出路径不在下载目录内: {:?}", out)));
            }
            self.file_transfers.insert(
                id_hex.clone(),
                FileTransferState {
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
                    is_outbound: false,
                },
            );
            self.inbound_file_count += 1;
        }

        if let Some(state) = self.file_transfers.get(&id_hex)
            && state.received_chunks.contains(&chunk.chunk_index)
        {
            let recv = state.received_bytes();
            self.emit_progress(&state.filename, chunk.chunk_index, state.total_chunks, recv, state.total_size, TransferProgressStatus::Downloading);
            return Ok(());
        }

        let decompressed = crate::compression::decompress(&chunk.chunk_data).await?;
        let dsz = decompressed.len() as u64;

        if (chunk.is_last && dsz > chunk.chunk_size as u64)
            || (!chunk.is_last && dsz != chunk.chunk_size as u64)
        {
            return Err(CoreError::FileTransferFailed(format!("解压后大小不匹配: {dsz} != {}", chunk.chunk_size)));
        }
        if chunk.is_last && chunk.offset + dsz > chunk.total_size {
            return Err(CoreError::FileTransferFailed(format!("末分片越界: {} + {} > {}", chunk.offset, dsz, chunk.total_size)));
        }

        let computed_hash = sha2::Sha256::digest(&decompressed);
        if !constant_time_compare(&computed_hash, &chunk.chunk_hash) {
            return Err(CoreError::FileTransferFailed(format!("分片哈希校验失败: chunk={}", chunk.chunk_index)));
        }

        let temp = self.file_transfers.get(&id_hex).map(|s| s.temp_path.clone())
            .unwrap_or_else(|| self.downloads_dir().join(format!(".{}.tmp", id_hex)));

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
                state.started_at = Instant::now();
                let recv = state.received_bytes();
                state.status = TransferStatus::Downloading { received_bytes: recv };
                state.persist_state(&dir.join(format!(".{}.state", id_hex)));
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

        self.emit_progress(&fname, chunk.chunk_index, total_chunks, recv, total_size, TransferProgressStatus::Downloading);

        if is_complete {
            self.complete_transfer(&id_hex, &fname, total_chunks, total_size).await?;
        }
        Ok(())
    }

    async fn complete_transfer(&mut self, file_id_hex: &str, filename: &str, total_chunks: u32, total_size: u64) -> Result<(), CoreError> {
        let expected = self.file_transfers.get(file_id_hex).map(|s| s.file_hash).unwrap_or([0u8; 32]);
        let temp = self.downloads_dir().join(format!(".{}.tmp", file_id_hex));

        let computed = match crate::transfer::compute_file_hash(&temp).await {
            Ok(h) => h,
            Err(e) => {
                self.release_transfer(file_id_hex);
                return Err(CoreError::FileTransferFailed(format!("计算哈希失败: {e}")));
            }
        };
        if !constant_time_compare(&computed, &expected) {
            let _ = tokio::fs::remove_file(&temp).await;
            self.release_transfer(file_id_hex);
            return Err(CoreError::FileHashMismatch);
        }
        tracing::info!("文件哈希验证通过");

        let output = self.downloads_dir().join(filename);
        let final_path = if output.exists() {
            let stem = output.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "file".to_string());
            let ext = output.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
            (1..10000)
                .map(|i| self.downloads_dir().join(format!("{} ({}){}", stem, i, ext)))
                .find(|p| !p.try_exists().unwrap_or(true))
                .unwrap_or_else(|| {
                    let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis();
                    self.downloads_dir().join(format!("{} ({}).{}", stem, ts, ext))
                })
        } else {
            output
        };
        if let Err(e) = tokio::fs::rename(&temp, &final_path).await {
            self.release_transfer(file_id_hex);
            return Err(CoreError::FileRenameFailed(format!("重命名失败: {e}")));
        }

        tracing::info!("下载完成: {} -> {:?}", filename, final_path);
        self.send_log(format!("文件 {} 下载完成", filename));
        self.emit_progress(filename, total_chunks, total_chunks, total_size, total_size, TransferProgressStatus::Completed);
        self.release_transfer(file_id_hex);
        let _ = std::fs::remove_file(self.downloads_dir().join(format!(".{}.state", file_id_hex)));
        Ok(())
    }

    /// 规范化并验证保存路径在 data_dir 内，否则回退到默认下载目录
    fn resolve_download_dir(&self, save_path: &Path) -> PathBuf {
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
        tracing::warn!("保存路径 {:?} 不在 data_dir 内，回退到默认目录 {:?}", canonical, fallback);
        self.send_warning(format!("保存路径不在 data_dir 内，已回退到默认下载目录 {:?}", fallback));
        fallback
    }
}