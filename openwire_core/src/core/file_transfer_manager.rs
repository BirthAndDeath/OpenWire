use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Instant;

use sha2::Digest;
use tokio::sync::mpsc;

use crate::{
    command::{MessageEvent, TransferProgressStatus},
    crypto::constant_time_compare,
    error::CoreError,
    message::FileStreamChunk,
    transfer::{FileTransferState, ResumeMeta, TransferStatus, MAX_CONCURRENT_TRANSFERS},
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
    // 文件尚不存在：校验其父目录一定位于基准目录内，且文件名不含路径分隔符/穿越
    let Some(parent) = path.parent() else {
        return false;
    };
    let Ok(canonical_parent) = parent.canonicalize() else {
        return false;
    };
    if !canonical_parent.starts_with(base) {
        return false;
    }
    let Some(file_name) = path.file_name() else {
        return false;
    };
    let s = file_name.to_string_lossy();
    !s.contains('/') && !s.contains('\\') && !s.contains("..")
}

fn load_resume_chunks(hash_hex: &str, dir: &Path) -> (PathBuf, HashSet<u32>) {
    let temp = dir.join(format!(".{}.tmp", hash_hex));
    let state = dir.join(format!(".{}.state", hash_hex));
    let chunks = if temp.exists() && state.exists() {
        match std::fs::read_to_string(&state) {
            Ok(c) => crate::transfer::parse_chunks_csv(&c),
            Err(_) => HashSet::new(),
        }
    } else {
        HashSet::new()
    };
    (temp, chunks)
}

/// 探针检测目标目录可写：实际创建并写入再删除，
/// 避免只读目录/无权限在后续分片写入时才暴露。
fn ensure_dir_writable(dir: &Path) -> Result<(), String> {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let probe = dir.join(format!(".ow-probe-{}-{}", std::process::id(), nanos));
    match std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&probe)
    {
        Ok(mut f) => {
            use std::io::Write;
            let _ = f.write_all(b"probe");
            let _ = std::fs::remove_file(&probe);
            Ok(())
        }
        Err(e) => Err(format!("目标目录不可写 {:?}: {e}", dir)),
    }
}

pub(crate) struct FileTransferManager {
    pub file_transfers: HashMap<String, FileTransferState>,
    pub outbound_file_count: usize,
    pub inbound_file_count: usize,
    last_file_timeout_scan: Instant,
    /// 规范化后的默认下载目录（构造时解析一次，避免每个分片重复 fs 调用）
    downloads_dir: PathBuf,
    event_sink: mpsc::Sender<MessageEvent>,
    /// 进度事件按显示粒度（1%）记录已发射百分比（按 file_id 隔离；
    /// 无锁 HashMap 由 &mut self 独占访问）。事件数≈100 级、与分片速率无关，
    /// 不随分片高频占用 IPC 通道。
    last_progress_pct: std::collections::HashMap<String, u8>,
    /// 已取消传输的 file_id_hex（记录取消时刻，用于清理与后续分片拒绝）
    cancelled: std::collections::HashMap<String, std::time::Instant>,
}

/// 取消标记在接收方侧的保留期：超过后允许同名文件重新下载/被推送
const CANCELLED_RETENTION: std::time::Duration = std::time::Duration::from_secs(120);

impl FileTransferManager {
    pub fn new(data_dir: PathBuf, event_sink: mpsc::Sender<MessageEvent>) -> Self {
        let dir = data_dir.join("downloads");
        let _ = std::fs::create_dir_all(&dir);
        // 规范化锚点：防止 data_dir 链上符号链接导致路径比较失真（TOCTOU 防护）
        let downloads_dir = std::fs::canonicalize(&dir).unwrap_or(dir);
        Self {
            file_transfers: HashMap::new(),
            outbound_file_count: 0,
            inbound_file_count: 0,
            last_file_timeout_scan: Instant::now(),
            downloads_dir,
            event_sink,
            last_progress_pct: std::collections::HashMap::new(),
            cancelled: std::collections::HashMap::new(),
        }
    }

    fn downloads_dir(&self) -> PathBuf {
        self.downloads_dir.clone()
    }

    fn send_warning(&self, msg: String) {
        let _ = self.event_sink.try_send(MessageEvent::Warning(msg));
    }

    fn send_log(&self, msg: String) {
        let _ = self.event_sink.try_send(MessageEvent::Log(msg));
    }

    /// 发射传输进度事件（按显示粒度聚合）。
    ///
    /// 仅当"百分比（1%）变化"或"终态/末分片"时才发射：事件总数 ≈ 文件百分比
    /// 级数（≤100），与分片速率无关。不再按时间节流——大文件数千分片也只会
    /// 产生约 100 条事件，避免高频占用 IPC 通道/前端渲染。
    fn emit_progress(&mut self, file_id: &str, progress: &crate::command::FileTransferProgress) {
        let is_terminal = matches!(
            progress.status,
            TransferProgressStatus::Completed | TransferProgressStatus::Error
        );
        let is_last = progress.chunk_index + 1 >= progress.total_chunks;
        let pct = if progress.total_size > 0 {
            ((progress.received_bytes as f64 / progress.total_size as f64) * 100.0).round() as u8
        } else {
            0
        };
        let key = file_id.to_string();
        if !is_terminal
            && !is_last
            && self.last_progress_pct.get(&key).copied() == Some(pct)
        {
            return;
        }
        self.last_progress_pct.insert(key, pct);
        let _ = self.event_sink.try_send(MessageEvent::FileTransferProgress(progress.clone()));
    }

    /// 发射失败进度事件（终态不经节流）。供 state 释放前/后均可调用，
    /// 保证 HashMismatch/RenameFailed 等路径前端仍能收到 Error 态。
    fn emit_error_progress(
        &mut self,
        file_id_hex: &str,
        filename: &str,
        total_chunks: u32,
        total_size: u64,
        sender_pubkey_hex: &str,
    ) {
        self.emit_progress(
            file_id_hex,
            &crate::command::FileTransferProgress {
                file_id: file_id_hex.to_string(),
                filename: filename.to_string(),
                chunk_index: total_chunks,
                total_chunks,
                received_bytes: total_size,
                total_size,
                sender_pubkey_hex: sender_pubkey_hex.to_string(),
                status: TransferProgressStatus::Error,
            },
        );
    }

    fn release_transfer(&mut self, file_id_hex: &str) {
        if let Some(state) = self.file_transfers.remove(file_id_hex) {
            if state.is_outbound {
                self.outbound_file_count = self.outbound_file_count.saturating_sub(1);
            } else {
                self.inbound_file_count = self.inbound_file_count.saturating_sub(1);
            }
        }
        self.last_progress_pct.remove(file_id_hex);
    }

    /// 收发共享同一并发预算（全局上限 MAX_CONCURRENT_TRANSFERS）
    fn can_start_new_transfer(&self) -> bool {
        self.outbound_file_count.saturating_add(self.inbound_file_count) < MAX_CONCURRENT_TRANSFERS
    }

    /// 准备下载请求：检查并发限制、目录可写、创建状态、序列化请求。
    /// 返回 (hash_hex, 序列化的 DownloadRequest 字节)。
    /// 调用方负责发送请求字节，并在发送失败时调用 cancel_download。
    pub fn try_start_download(
        &mut self,
        file_hash: [u8; 32],
        save_path: &Path,
        sender_pubkey_hex: &str,
    ) -> Result<(String, Vec<u8>), String> {
        let hash_hex = hex::encode(file_hash);

        if self.file_transfers.contains_key(&hash_hex) {
            return Err(format!("文件 {}.. 已在下载中", &hash_hex[..16]));
        }
        if !self.can_start_new_transfer() {
            return Err(format!("并发传输数已达上限 ({MAX_CONCURRENT_TRANSFERS})，请等待当前传输完成"));
        }

        let dir = self.resolve_download_dir(save_path);
        ensure_dir_writable(&dir)?;
        let (temp_path, existing) = load_resume_chunks(&hash_hex, &dir);

        // 续传恢复（received_chunks 非空）时聚合哈希无法重建 → None，
        // complete_transfer 回退磁盘重算；全新传输 → 边收边哈希。
        let resumed = !existing.is_empty();
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
            sender_pubkey_hex: sender_pubkey_hex.to_string(),
            chunk_data_hasher: if resumed { None } else { Some(sha2::Sha256::new()) },
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
    /// 先发射 Error 终态进度事件（驱动前端气泡显示失败态），再移除状态。
    pub fn cancel_download(&mut self, hash_hex: &str) {
        if let Some(state) = self.file_transfers.remove(hash_hex) {
            self.emit_error_progress(
                hash_hex,
                &state.filename,
                state.total_chunks,
                state.total_size,
                &state.sender_pubkey_hex,
            );
            self.outbound_file_count = self.outbound_file_count.saturating_sub(1);
        }
    }

    /// 该 file_id 是否已被用户取消（后续分片应拒绝并回 Ack=false）
    pub fn is_cancelled(&self, file_id_hex: &str) -> bool {
        self.cancelled.contains_key(file_id_hex)
    }

    /// 取消传输（下载或被推送均可）：清状态/计数/.tmp/.state 并登记 cancelled，
    /// 后续到达的分片将被拒绝（Ack=false）。返回是否有活动传输被取消。
    pub fn cancel_transfer(&mut self, file_id_hex: &str) -> bool {
        // 先取 temp_path 与方向再移除状态，确保自定义目录清理到位且按方向扣计数
        let temp = self
            .file_transfers
            .get(file_id_hex)
            .map(|s| s.temp_path.clone())
            .unwrap_or_default();
        let is_outbound = self
            .file_transfers
            .get(file_id_hex)
            .map(|s| s.is_outbound)
            .unwrap_or(true);
        // 克隆展示字段用于失败终态事件（emit_error_progress 需要 &mut self）
        let (filename, total_chunks, total_size, sender_pubkey_hex) = self
            .file_transfers
            .get(file_id_hex)
            .map(|s| {
                (
                    s.filename.clone(),
                    s.total_chunks,
                    s.total_size,
                    s.sender_pubkey_hex.clone(),
                )
            })
            .unwrap_or_default();
        let removed = self.file_transfers.remove(file_id_hex).is_some();
        if removed {
            if is_outbound {
                self.outbound_file_count = self.outbound_file_count.saturating_sub(1);
            } else {
                self.inbound_file_count = self.inbound_file_count.saturating_sub(1);
            }
            // 取消即失败：发终态事件，前端卡片明确显示"失败"，可重试或删除
            self.emit_error_progress(
                file_id_hex,
                &filename,
                total_chunks,
                total_size,
                &sender_pubkey_hex,
            );
        }
        let state_path = temp
            .parent()
            .map(|p| p.join(format!(".{}.state", file_id_hex)))
            .unwrap_or_else(|| self.downloads_dir().join(format!(".{}.state", file_id_hex)));
        let _ = std::fs::remove_file(&temp);
        let _ = std::fs::remove_file(&state_path);
        self.last_progress_pct.remove(file_id_hex);
        // 无论是否取消到活动状态都要登记，防止取消后到达的分片重建传输
        self.cancelled
            .insert(file_id_hex.to_string(), Instant::now());
        removed
    }

    /// 清理超过保留期的取消防记录（防止 cancelled 集无界增长）
    fn prune_cancelled(&mut self) {
        let now = Instant::now();
        self.cancelled
            .retain(|_, t| now.duration_since(*t) < CANCELLED_RETENTION);
    }

    /// 扫描未完成的下载（`.{hash}.tmp` + `.{hash}.state` 配对），供启动续传。
    ///
    /// 递归遍历默认下载目录（自定义保存路径均被约束在其内），带深度/数量上限。
    /// 仅返回含 sender 元数据的新格式状态文件；旧格式（纯 CSV）仍可被
    /// load_resume_chunks 用于手动重下时恢复分片，故不清理。
    pub fn scan_resumable(&self) -> Vec<([u8; 32], ResumeMeta)> {
        const MAX_VISITED: usize = 20_000;
        const MAX_DEPTH: usize = 4;
        let mut out: Vec<([u8; 32], ResumeMeta)> = Vec::new();
        let mut stack: Vec<PathBuf> = vec![self.downloads_dir().clone()];
        let mut visited = 0usize;
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                visited += 1;
                if visited > MAX_VISITED {
                    return out;
                }
                let path = entry.path();
                if path.is_dir() {
                    if stack.len() < MAX_DEPTH {
                        stack.push(path);
                    }
                    continue;
                }
                let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                    continue;
                };
                // 形如 ".{64hex}.state"
                if name.len() == 71 && name.starts_with('.') && name.ends_with(".state") {
                    let hash_hex = &name[1..65];
                    let tmp = dir.join(format!(".{}.tmp", hash_hex));
                    if !tmp.exists() {
                        continue;
                    }
                    if let Some((meta, _)) = crate::transfer::load_resume_state(&path)
                        && !meta.sender_pubkey_hex.is_empty()
                        && let Ok(bytes) = hex::decode(hash_hex)
                        && bytes.len() == 32
                    {
                        let mut file_hash = [0u8; 32];
                        file_hash.copy_from_slice(&bytes);
                        out.push((file_hash, meta));
                    }
                }
            }
        }
        out
    }

    pub async fn handle_download_response(&mut self, response: crate::message::DownloadResponse) {
        let hash_hex = hex::encode(response.file_hash);
        let raw_filename = response.filename.as_deref().unwrap_or("unknown");
        let safe = sanitize_filename(raw_filename).unwrap_or_else(|| "unknown".to_string());
        let downloads_dir = self.downloads_dir();
        let Some(state) = self.file_transfers.get_mut(&hash_hex) else {
            tracing::warn!("DownloadResponse 无对应状态: {}..", &hash_hex[..16]);
            return;
        };
        // 输出与 temp/state 同目录：以所选保存目录（try_start_download 经
        // resolve_download_dir 决定并已规范化）为基底，而非默认下载目录，
        // 保证"保存到所选文件夹"语义一致且 rename 同卷原子完成。
        let output_dir = state
            .temp_path
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or(downloads_dir);
        let output_path = output_dir.join(&safe);
        if !validate_path_within_base(&output_path, &output_dir) {
            tracing::warn!("DownloadResponse 文件名不安全: {}", raw_filename);
            self.send_warning(format!("下载响应文件名校验失败: {}", raw_filename));
            self.cancel_download(&hash_hex);
            return;
        }
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
            let temp_path = state.temp_path.clone();
            let _ = state;
            // 清理断点残留（.tmp/.state），避免下次启动误续传并重发 DownloadRequest
            let state_path = temp_path
                .parent()
                .map(|p| p.join(format!(".{}.state", hash_hex)))
                .unwrap_or_default();
            let _ = std::fs::remove_file(&temp_path);
            let _ = std::fs::remove_file(&state_path);
            self.cancel_download(&hash_hex);
            self.send_log(format!("文件已存在: {}", filename));
            return;
        }
        state.output_path = output_path;
        state.status = TransferStatus::Downloading { received_bytes: 0 };
    }

    pub fn scan_timeout_transfers(&mut self) {
        let now = Instant::now();
        self.prune_cancelled();
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
            // 状态文件与临时文件同目录（与 state.temp_path 基底一致），
            // 而非重拼默认 downloads_dir，避免自定义保存目录时清理错位置。
            let temp_path = self
                .file_transfers
                .get(id)
                .map(|s| s.temp_path.clone())
                .unwrap_or_default();
            let state_path = temp_path
                .parent()
                .map(|p| p.join(format!(".{}.state", id)))
                .unwrap_or(self.downloads_dir().join(format!(".{}.state", id)));
            // 克隆展示字段：emit_error_progress 需要 &mut self，不能与 get_mut 借用共存
            let (filename, total_chunks, total_size, sender_pubkey_hex) = self
                .file_transfers
                .get(id)
                .map(|s| {
                    (
                        s.filename.clone(),
                        s.total_chunks,
                        s.total_size,
                        s.sender_pubkey_hex.clone(),
                    )
                })
                .unwrap_or_default();
            if let Some(s) = self.file_transfers.get_mut(id) {
                s.status = TransferStatus::Failed;
                let _ = std::fs::remove_file(&s.temp_path);
                let _ = std::fs::remove_file(&state_path);
            }
            // 超时失败同样发终态事件：前端明确显示"失败"，否则文件卡片永远卡在下载中
            self.emit_error_progress(id, &filename, total_chunks, total_size, &sender_pubkey_hex);
            if self.file_transfers.get(id).map(|s| s.is_outbound).unwrap_or(true) {
                self.outbound_file_count = self.outbound_file_count.saturating_sub(1);
            } else {
                self.inbound_file_count = self.inbound_file_count.saturating_sub(1);
            }
            self.file_transfers.remove(id);
            self.last_progress_pct.remove(id);
            self.send_warning(format!("文件传输超时已清理（{}..）", &id[..16.min(id.len())]));
        }
        if !timed_out.is_empty() {
            tracing::info!("已清理 {} 个超时传输", timed_out.len());
        }
    }

    fn validate_chunk(chunk: &FileStreamChunk) -> Result<String, CoreError> {
        let fid = &hex::encode(chunk.file_hash)[..16.min(hex::encode(chunk.file_hash).len())];
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
        // total_chunks 必须与 total_size/chunk_size 一致：既防恶意方声明 u32::MAX
        // 分片数夸大 state（received_bytes/last_chunk_size 依赖该一致性），
        // 也防止声明过小导致完整校验永远无法达成。
        let expected_chunks = chunk.total_size.div_ceil(chunk.chunk_size as u64);
        if chunk.total_chunks as u64 != expected_chunks {
            return Err(e(format!(
                "总分片数与文件大小不一致: total_chunks={} 预期={} (size={}, chunk_size={})",
                chunk.total_chunks, expected_chunks, chunk.total_size, chunk.chunk_size
            )));
        }
        if chunk.is_last && chunk.offset > chunk.total_size {
            return Err(e(format!("末分片 offset>total_size: {}>{}", chunk.offset, chunk.total_size)));
        }
        Ok(safe)
    }

    /// 处理入站文件分片（发送方 = sender_pubkey_hex，写入续传元数据）。
    /// 任何失败路径统一上报 Warning + Error 进度事件到前端。
    pub async fn handle_file_stream_chunk(
        &mut self,
        chunk: FileStreamChunk,
        sender_pubkey_hex: &str,
    ) -> Result<(), CoreError> {
        let id_hex = hex::encode(chunk.file_hash);
        let chunk_index = chunk.chunk_index;
        let result = self
            .handle_file_stream_chunk_inner(chunk, sender_pubkey_hex)
            .await;
        if result.is_err() {
            // 警告文案由调用方 handle_incoming_file_chunk 统一发送（含来源 peer 上下文），
            // 此处仅补发 Error 进度事件驱动前端气泡显示失败态，避免重复 toast。
            if let Some(state) = self.file_transfers.get(&id_hex) {
                let progress = crate::command::FileTransferProgress {
                    file_id: id_hex.clone(),
                    filename: state.filename.clone(),
                    chunk_index,
                    total_chunks: state.total_chunks,
                    received_bytes: state.received_bytes(),
                    total_size: state.total_size,
                    sender_pubkey_hex: sender_pubkey_hex.to_string(),
                    status: TransferProgressStatus::Error,
                };
                self.emit_progress(&id_hex, &progress);
            }
        }
        result
    }

    async fn handle_file_stream_chunk_inner(
        &mut self,
        chunk: FileStreamChunk,
        sender_pubkey_hex: &str,
    ) -> Result<(), CoreError> {
        let id_hex = hex::encode(chunk.file_hash);
        let safe_name = Self::validate_chunk(&chunk)?;
        self.scan_timeout_transfers();

        // 已取消的传输：拒绝写入且不回建状态（Ack=false 由 event_loop 的 is_cancelled 决定）
        if self.cancelled.contains_key(&id_hex) {
            return Ok(());
        }

        let is_new = !self.file_transfers.contains_key(&id_hex);
        if is_new {
            if !self.can_start_new_transfer() {
                return Err(CoreError::FileTransferFailed(format!("并发传输数已达上限 ({MAX_CONCURRENT_TRANSFERS})")));
            }
            ensure_dir_writable(&self.downloads_dir())
                .map_err(CoreError::FileTransferFailed)?;
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
                    sender_pubkey_hex: sender_pubkey_hex.to_string(),
                    // 全新传输：边收边聚合分片哈希，完成时验"分片 hash 合为总 hash"
                    chunk_data_hasher: Some(sha2::Sha256::new()),
                },
            );
            self.inbound_file_count += 1;
        }

        if let Some(state) = self.file_transfers.get(&id_hex)
            && state.received_chunks.contains(&chunk.chunk_index)
        {
            let recv = state.received_bytes();
            // 提前克隆字段：emit_progress/complete_transfer 需要 &mut self，
            // 不能与 file_transfers 的不可变借用同时存在
            let progress = crate::command::FileTransferProgress {
                file_id: id_hex.clone(),
                filename: state.filename.clone(),
                chunk_index: chunk.chunk_index,
                total_chunks: state.total_chunks,
                received_bytes: recv,
                total_size: state.total_size,
                sender_pubkey_hex: sender_pubkey_hex.to_string(),
                status: TransferProgressStatus::Downloading,
            };
            let is_resume_complete = state.total_chunks > 0
                && state.received_chunks.len() as u32 >= state.total_chunks;
            let fname = state.filename.clone();
            let total_chunks_c = state.total_chunks;
            let total_size_c = state.total_size;
            self.emit_progress(&id_hex, &progress);
            // 续传后对端重发全部分片，逐片命中去重早返回；
            // 若 state 已含全量分片（persist 与 rename 之间崩溃），此处必须补完成。
            if is_resume_complete {
                self.complete_transfer(&id_hex, &fname, total_chunks_c, total_size_c)
                    .await?;
            }
            return Ok(());
        }

        let decompressed = crate::compression::decompress(&chunk.chunk_data).await?;
        let dsz = decompressed.len() as u64;

        // 逐分片长度上界校验：
        // 1) 分片序号不得超出总分片数（对端多发 total_chunks 之外的分片直接拒绝）；
        // 2) 分片写入区间（offset+dsz）不得超过文件总大小（已有，含末片取整）。
        if chunk.chunk_index >= chunk.total_chunks {
            return Err(CoreError::FileTransferFailed(format!(
                "分片序号越界: {} >= 总分片数 {}",
                chunk.chunk_index, chunk.total_chunks
            )));
        }
        if (chunk.is_last && dsz > chunk.chunk_size as u64)
            || (!chunk.is_last && dsz != chunk.chunk_size as u64)
        {
            return Err(CoreError::FileTransferFailed(format!("解压后大小不匹配: {dsz} != {}", chunk.chunk_size)));
        }
        if chunk.is_last && chunk.offset + dsz > chunk.total_size {
            return Err(CoreError::FileTransferFailed(format!("末分片越界: {} + {} > {}", chunk.offset, dsz, chunk.total_size)));
        }
        if !chunk.is_last && chunk.offset + dsz > chunk.total_size {
            return Err(CoreError::FileTransferFailed(format!("非末分片越界: {} + {} > {}", chunk.offset, dsz, chunk.total_size)));
        }

        let computed_hash = sha2::Sha256::digest(&decompressed);
        if !constant_time_compare(&computed_hash, &chunk.chunk_hash) {
            return Err(CoreError::FileTransferFailed(format!("分片哈希校验失败: chunk={}", chunk.chunk_index)));
        }

        let temp = self.file_transfers.get(&id_hex).map(|s| s.temp_path.clone())
            .unwrap_or_else(|| self.downloads_dir().join(format!(".{}.tmp", id_hex)));

        // 分片写入移出事件循环：磁盘 I/O 全部在 spawn_blocking 中执行，
        // 避免大文件下载时 open/seek/flush 阻塞消息收发、ack 与 load_messages。
        let write_offset = chunk.offset;
        // 聚合哈希在写盘成功后才喂入（见下方状态块）；此处克隆一份明文备用，
        // 避免"写失败→发送方重试→同分片重复喂入聚合器"导致完成时哈希假失配。
        let hasher_feed = decompressed.clone();
        let data = decompressed;
        match tokio::task::spawn_blocking(move || {
            use std::io::{Seek, SeekFrom, Write};
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .truncate(false)
                .write(true)
                .open(&temp)
                .map_err(|e| format!("打开临时文件失败: {e}"))?;
            file.seek(SeekFrom::Start(write_offset))
                .map_err(|e| format!("定位写入位置失败: {e}"))?;
            file.write_all(&data).map_err(|e| format!("写入分片失败: {e}"))?;
            file.flush().map_err(|e| format!("刷盘失败: {e}"))?;
            Ok::<(), String>(())
        })
        .await
        {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                return Err(CoreError::FileTransferFailed(format!("写入文件分片失败: {e}")));
            }
            Err(e) => {
                return Err(CoreError::FileTransferFailed(format!("分片写入任务异常: {e}")));
            }
        }

        let dir = self.downloads_dir();
        let (is_complete, fname, total_chunks, total_size, recv) =
            if let Some(state) = self.file_transfers.get_mut(&id_hex) {
                state.received_chunks.insert(chunk.chunk_index);
                // 写盘成功才喂入聚合哈希（与 received_chunks 提交同点）：
                // 保证每个分片恰好聚合一次，且顺序 = 分片序号序。
                if let Some(hasher) = state.chunk_data_hasher.as_mut() {
                    use sha2::Digest;
                    hasher.update(&hasher_feed);
                }
                state.started_at = Instant::now();
                let recv = state.received_bytes();
                state.status = TransferStatus::Downloading { received_bytes: recv };
                // 断点状态降频写盘：每 16 个分片（≈4MB）持久化一次，
                // 避免每个 256KB 分片都做一次同步文件 I/O 阻塞事件循环；
                // 完成传输由 complete_transfer 处理，这里无需兜底写。
                if state.received_chunks.len() % 16 == 0 {
                    // 状态文件与临时文件同目录（与 state.temp_path 基底一致）
                    let state_file = state
                        .temp_path
                        .parent()
                        .map(|p| p.join(format!(".{}.state", id_hex)))
                        .unwrap_or_else(|| dir.join(format!(".{}.state", id_hex)));
                    // 断点写盘属崩溃恢复的尽力而为：序列化在循环内完成（纯内存），
                    // fs::write 移出事件循环异步执行，失败静默忽略（与旧行为一致）。
                    let json = state.state_json();
                    tokio::task::spawn_blocking(move || {
                        let _ = std::fs::write(&state_file, json);
                    });
                }
                // total_chunks>0 守卫：DownloadResponse 尚未到达时（请求先于响应、
                // 响应 rr_msg 与分片 rr_file 事件顺序不保证），state 的 total_chunks
                // 仍为 0。此时首个分片会让 len>=0 恒真 → 提前 complete_transfer →
                // 哈希必然不匹配 → 删临时文件并释放状态 → 该分片永久缺失、下载卡死。
                // 守卫使状态在元数据就绪前只累积分片，待 DownloadResponse 补齐后正常完成。
                (
                    state.total_chunks > 0
                        && state.received_chunks.len() as u32 >= state.total_chunks,
                    state.filename.clone(),
                    state.total_chunks,
                    state.total_size,
                    recv,
                )
            } else {
                return Ok(());
            };

        self.emit_progress(&id_hex, &crate::command::FileTransferProgress {
            file_id: id_hex.clone(),
            filename: fname.clone(),
            chunk_index: chunk.chunk_index,
            total_chunks,
            received_bytes: recv,
            total_size,
            sender_pubkey_hex: sender_pubkey_hex.to_string(),
            status: TransferProgressStatus::Downloading,
        });

        if is_complete {
            self.complete_transfer(&id_hex, &fname, total_chunks, total_size).await?;
        }
        Ok(())
    }

    async fn complete_transfer(&mut self, file_id_hex: &str, filename: &str, total_chunks: u32, total_size: u64) -> Result<(), CoreError> {
        // 从 state 派生 temp/output 路径（与 try_start_download 中保存目录一致），
        // 而非重拼默认 downloads_dir，避免自定义保存目录时路径漂移。
        let Some(state) = self.file_transfers.get(file_id_hex) else {
            return Ok(());
        };
        let expected = state.file_hash;
        let temp = state.temp_path.clone();
        let sender = state.sender_pubkey_hex.clone();

        // 验证"分片 hash 合为总 hash"：全新传输用增量聚合哈希（无需重读磁盘）；
        // 续传恢复（聚合不可重建）回退磁盘重算。
        let computed = {
            use sha2::Digest;
            let acc = self
                .file_transfers
                .get(file_id_hex)
                .and_then(|s| s.chunk_data_hasher.as_ref());
            match acc {
                Some(h) => h.clone().finalize().into(),
                None => match crate::transfer::compute_file_hash(&temp).await {
                    Ok(h) => h,
                    Err(e) => {
                        self.emit_error_progress(file_id_hex, filename, total_chunks, total_size, &sender);
                        self.release_transfer(file_id_hex);
                        return Err(CoreError::FileTransferFailed(format!("计算哈希失败: {e}")));
                    }
                },
            }
        };
        // 顺带验证长度：已收字节数必须等于文件总大小（与哈希互为印证；
        // 长度不符时给出比哈希失配更明确的错误定位）。超出/不足分别报错，
        // 超出通常来自对端多发了 total_chunks 之外的分片。
        let recv_bytes = self
            .file_transfers
            .get(file_id_hex)
            .map(|s| s.received_bytes())
            .unwrap_or(0);
        if recv_bytes > total_size {
            self.emit_error_progress(file_id_hex, filename, total_chunks, total_size, &sender);
            self.release_transfer(file_id_hex);
            return Err(CoreError::FileTransferFailed(format!(
                "文件长度超出: 已收 {recv_bytes} 字节 > 总大小 {total_size} 字节"
            )));
        }
        if recv_bytes < total_size {
            self.emit_error_progress(file_id_hex, filename, total_chunks, total_size, &sender);
            self.release_transfer(file_id_hex);
            return Err(CoreError::FileTransferFailed(format!(
                "文件长度不足: 已收 {recv_bytes} 字节 < 总大小 {total_size} 字节"
            )));
        }

        if !constant_time_compare(&computed, &expected) {
            let _ = tokio::fs::remove_file(&temp).await;
            self.emit_error_progress(file_id_hex, filename, total_chunks, total_size, &sender);
            self.release_transfer(file_id_hex);
            return Err(CoreError::FileHashMismatch);
        }
        tracing::info!("文件哈希验证通过");

        // 输出路径优先使用 state.output_path（如 try_start_download 中已按 save_path 解析），
        // 空则回退到默认下载目录下的 sanitize 文件名。
        let state_output = self.file_transfers.get(file_id_hex).map(|s| s.output_path.clone()).unwrap_or_default();
        let downloads_dir = self.downloads_dir();
        let output = if !state_output.as_os_str().is_empty() {
            state_output
        } else {
            downloads_dir.join(filename)
        };
        let final_path = if output.exists() {
            // 编号变体必须落在输出所在目录（output.parent()），而非默认 downloads_dir，
            // 否则自定义保存目录下同名文件会被改写到默认目录，用户打开的是空白/旧档。
            let dir = output
                .parent()
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|| downloads_dir.clone());
            let stem = output.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "file".to_string());
            let ext = output.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
            (1..10000)
                .map(|i| dir.join(format!("{} ({}){}", stem, i, ext)))
                .find(|p| !p.try_exists().unwrap_or(true))
                .unwrap_or_else(|| {
                    let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis();
                    dir.join(format!("{} ({}).{}", stem, ts, ext))
                })
        } else {
            output
        };
        if let Err(e) = tokio::fs::rename(&temp, &final_path).await {
            self.emit_error_progress(file_id_hex, filename, total_chunks, total_size, &sender);
            self.release_transfer(file_id_hex);
            return Err(CoreError::FileRenameFailed(format!("重命名失败: {e}")));
        }

        tracing::info!("下载完成: {} -> {:?}", filename, final_path);
        self.send_log(format!("文件 {} 下载完成", filename));
        self.emit_progress(file_id_hex, &crate::command::FileTransferProgress {
            file_id: file_id_hex.to_string(),
            filename: filename.to_string(),
            chunk_index: total_chunks,
            total_chunks,
            received_bytes: total_size,
            total_size,
            sender_pubkey_hex: sender,
            status: TransferProgressStatus::Completed,
        });
        self.release_transfer(file_id_hex);
        // 状态文件与临时文件同目录（与写入路径一致），
        // 自定义保存目录时也清理到位，而非重拼默认 downloads_dir
        let state_path = temp
            .parent()
            .map(|p| p.join(format!(".{}.state", file_id_hex)))
            .unwrap_or_else(|| downloads_dir.join(format!(".{}.state", file_id_hex)));
        let _ = std::fs::remove_file(state_path);
        Ok(())
    }

    /// 计算保存目录：保存位置由用户经"另存为"对话框选择（可任意位置），
    /// 目录无法规范化时回退默认下载目录（可写性由 ensure_dir_writable 兜底）
    fn resolve_download_dir(&self, save_path: &Path) -> PathBuf {
        let raw = if save_path.is_dir() {
            save_path.to_path_buf()
        } else if let Some(p) = save_path.parent() {
            p.to_path_buf()
        } else {
            return self.downloads_dir();
        };
        let _ = std::fs::create_dir_all(&raw);
        // 保存位置由用户经"另存为"对话框选择，可在任意位置；
        // 目录无法规范化时回退默认下载目录（可写性由 ensure_dir_writable 兜底）。
        std::fs::canonicalize(&raw).unwrap_or_else(|_| self.downloads_dir())
    }
}