//! # 文件传输状态管理
//!
//! 管理文件传输过程中的状态，包括：
//!
//! - [`TransferStatus`]：传输状态枚举（请求中/下载中/已完成/失败）
//! - [`FileTransferState`]：文件传输状态结构体（含分片接收进度）
//! - [`compute_file_hash`]：计算文件 SHA256 哈希
//!
//! ## 设计原则
//!
//! - **接收方侧状态管理**：`FileTransferState` 仅在接收方维护
//! - **断点续传支持**：通过 `received_chunks` 记录已接收分片
//! - **临时文件**：下载过程中写入 `temp_path`，完成后移动到 `output_path`
//!
//! ## 健壮性限制
//!
//! - **文件大小限制**：单文件最大 100MB（`MAX_FILE_SIZE`），防止 DoS
//! - **并发传输限制**：最多 3 个同时传输（`MAX_CONCURRENT_TRANSFERS`）
//! - **超时机制**：传输超过 30 分钟无新分片则标记失败（`TRANSFER_TIMEOUT`）

use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::path::Path;
use std::path::PathBuf;
use tokio::io::AsyncReadExt;

use crate::error::FileTransferResult;

/// 单文件最大大小（100MB），防止 DoS 攻击
/// 接收方拒绝超过此大小的文件下载请求
pub const MAX_FILE_SIZE: u64 = 100 * 1024 * 1024;

/// 最大并发文件传输数（下载与被推送共用同一预算）
/// 超过此限制的新下载/入站传输将被拒绝
pub const MAX_CONCURRENT_TRANSFERS: usize = 3;

/// 文件传输超时时间（30 分钟）
/// 从传输开始计时，超过此时间未完成则标记为失败并清理
pub const TRANSFER_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30 * 60);

/// 计算文件的 SHA256 哈希
///
/// 用于验证文件完整性（下载完成后比对哈希）。
/// 使用 8KB 缓冲区流式读取，避免大文件占用过多内存。
pub async fn compute_file_hash(file_path: &Path) -> FileTransferResult<[u8; 32]> {
    let mut file = {
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            let mut std_opts = std::fs::OpenOptions::new();
            std_opts.read(true);
            std_opts.share_mode(0x00000001 | 0x00000002 | 0x00000004);
            let std_file = std_opts.open(file_path)?;
            tokio::fs::File::from_std(std_file)
        }
        #[cfg(not(windows))]
        {
            tokio::fs::File::open(file_path).await?
        }
    };
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 8192];
    loop {
        let n = file.read(&mut buffer).await?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }
    let result = hasher.finalize();
    Ok(result.into())
}

/// 文件传输状态
#[derive(Debug)]
pub enum TransferStatus {
    /// 等待发送方响应
    Requesting,
    /// 正在接收
    Downloading {
        /// 已接收的字节数
        received_bytes: u64,
    },
    /// 接收完成
    Completed,
    /// 失败（无额外数据，错误信息通过日志记录）
    Failed,
}

/// 文件传输状态管理（接收方侧）
#[derive(Debug)]
pub struct FileTransferState {
    /// 原始文件名
    pub filename: String,
    /// 文件总大小
    pub total_size: u64,
    /// 总分片数
    pub total_chunks: u32,
    /// 每片大小
    pub chunk_size: u32,
    /// 完整文件 SHA256
    pub file_hash: [u8; 32],
    /// 已接收的分片序号
    pub received_chunks: HashSet<u32>,
    /// 临时文件路径
    pub temp_path: PathBuf,
    /// 最终输出路径
    pub output_path: PathBuf,
    /// 传输状态
    pub status: TransferStatus,
    /// 开始时间
    pub started_at: std::time::Instant,
    /// 方向：true=本端发起的下载请求（占用 outbound_file_count），
    /// false=对端主动推送的分片接收（占用 inbound_file_count）
    pub is_outbound: bool,
    /// 发送方 ML-DSA 公钥 hex（用于启动时自动续传重新发起 DownloadRequest）
    pub sender_pubkey_hex: String,
    /// 分片数据增量聚合 SHA-256（边收边哈希分片明文）。
    ///
    /// 完成时先验聚合值 == 完整文件 SHA256（"分片 hash 合为总 hash"），
    /// 避免整文件重读磁盘；`None` 表示续传恢复（无法重建聚合），回退磁盘重算。
    /// 仅内存态，不参与 state_json 序列化。
    pub chunk_data_hasher: Option<sha2::Sha256>,
}

/// 断点续传元数据（state 文件结构化部分）
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ResumeMeta {
    /// 发送方 ML-DSA 公钥 hex
    pub sender_pubkey_hex: String,
    /// 原始文件名
    pub filename: String,
    /// 文件总大小（字节）
    pub total_size: u64,
    /// 保存目录（临时文件所在目录，续传时重新解析路径）
    pub save_dir: String,
}

impl FileTransferState {
    /// 计算已接收字节数（最后一个分片可能小于 chunk_size）
    pub fn received_bytes(&self) -> u64 {
        if self.total_chunks == 0 || self.chunk_size == 0 || self.total_size == 0 {
            return 0;
        }
        let last_chunk_size = if self.total_size.is_multiple_of(self.chunk_size as u64) {
            self.chunk_size as u64
        } else {
            self.total_size % self.chunk_size as u64
        };
        let last_idx = self.total_chunks - 1;
        self.received_chunks
            .iter()
            .map(|&idx| {
                if idx == last_idx {
                    last_chunk_size
                } else {
                    self.chunk_size as u64
                }
            })
            .sum()
    }

    /// 序列化续传状态为 JSON（断点续传 + 启动自动续传）
    pub fn state_json(&self) -> String {
        let meta = ResumeMeta {
            sender_pubkey_hex: self.sender_pubkey_hex.clone(),
            filename: self.filename.clone(),
            total_size: self.total_size,
            save_dir: self
                .temp_path
                .parent()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default(),
        };
        let chunks: Vec<u32> = self.received_chunks.iter().copied().collect();
        serde_json::to_string(&serde_json::json!({ "meta": meta, "chunks": chunks }))
            .unwrap_or_default()
    }

    /// 持久化已接收分片与元数据到状态文件（断点续传 + 启动自动续传）
    pub fn persist_state(&self, state_path: &Path) {
        if let Err(e) = std::fs::write(state_path, self.state_json()) {
            tracing::warn!("写断点续传状态文件失败 {:?}: {e}", state_path);
        }
    }
}

/// 从状态文件加载续传元数据与已接收分片。
///
/// 新格式为 JSON（meta + chunks）；旧格式为纯 CSV 分片号（无 sender，
/// 无法启动自动续传），返回 None 由调用方决定跳过并清理。
pub fn load_resume_state(state_path: &Path) -> Option<(ResumeMeta, HashSet<u32>)> {
    let content = std::fs::read_to_string(state_path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&content).ok()?;
    let meta: ResumeMeta = serde_json::from_value(value.get("meta")?.clone()).ok()?;
    let chunks: Vec<u32> = serde_json::from_value(value.get("chunks")?.clone())
        .unwrap_or_default();
    Some((meta, chunks.into_iter().collect()))
}

/// 解析旧格式断点文件（纯 CSV 分片号）与 JSON 格式的 chunks 字段
pub fn parse_chunks_csv(content: &str) -> HashSet<u32> {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return HashSet::new();
    }
    // JSON 含 "chunks" 字段时取其数组
    if trimmed.starts_with('{')
        && let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed)
        && let Some(arr) = value.get("chunks")
        && let Ok(chunks) = serde_json::from_value::<Vec<u32>>(arr.clone())
    {
        return chunks.into_iter().collect();
    }
    trimmed.split(',').filter_map(|s| s.trim().parse().ok()).collect()
}

/// 发送文件的可复用实现：可访问性鉴权（规范化 + 探读）→ 大小上限 → 计算 hash →
/// 记录 `sent_files`（绑定接收人，防止"知道哈希即可被任意联系人拉取"）→ 发送 FileHash 消息。
///
/// Tauri 命令层与 [`crate::corehandle::CoreHandle::send_file`] 共用此实现，避免两套逻辑漂移。
/// 返回发送的文件名（供 UI 提示）。
pub async fn prepare_and_send_file(
    cmd_tx: &tokio::sync::mpsc::Sender<crate::command::ChatCommand>,
    owner_mldsa_pubkey_hex: &str,
    file_path: &Path,
) -> Result<String, String> {
    let canon = file_path
        .canonicalize()
        .map_err(|e| format!("无效的文件路径: {e}"))?;
    {
        use std::io::Read;
        let mut file = std::fs::File::open(&canon).map_err(|e| format!("文件不可访问: {e}"))?;
        let mut probe = [0u8; 1];
        file.read(&mut probe)
            .map_err(|e| format!("文件读取失败: {e}"))?;
    }
    let meta = std::fs::metadata(&canon).map_err(|e| format!("获取文件信息失败: {e}"))?;
    if !meta.is_file() {
        return Err("仅支持发送普通文件".to_string());
    }
    if meta.len() > MAX_FILE_SIZE {
        return Err(format!(
            "文件过大（超过 {}MB）",
            MAX_FILE_SIZE / (1024 * 1024)
        ));
    }
    let file_hash = compute_file_hash(&canon)
        .await
        .map_err(|e| format!("计算文件 hash 失败: {e}"))?;
    let total_size = meta.len();
    let filename = canon
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown")
        .to_string();

    if let Some(pool) = crate::storage::pool()
        && let Err(e) = crate::storage::add_sent_file(
            pool,
            &file_hash,
            canon.to_str().unwrap_or(""),
            &filename,
            total_size,
            owner_mldsa_pubkey_hex,
        )
        .await
    {
        tracing::warn!("记录已发送文件失败: {e}");
    }

    let file_info = crate::message::FileHashInfo::new(filename.clone(), total_size, file_hash);
    let file_info_bytes = postcard::to_allocvec(&file_info)
        .map_err(|e| format!("序列化 FileHashInfo 失败: {e}"))?;

    cmd_tx
        .send(crate::command::ChatCommand::SendMessage {
            mldsa_pubkey_hex: owner_mldsa_pubkey_hex.to_string(),
            msgtype: crate::message::ChatMessageType::FileHash,
            data: file_info_bytes,
        })
        .await
        .map_err(|e| format!("发送文件消息失败: {e}"))?;

    Ok(filename)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_chunks_csv_old_format() {
        let chunks = parse_chunks_csv("0,2,5");
        assert_eq!(chunks, [0u32, 2, 5].into_iter().collect());
        assert!(parse_chunks_csv("").is_empty());
    }

    #[test]
    fn test_parse_chunks_csv_json_format() {
        let chunks = parse_chunks_csv(r#"{"meta":{},"chunks":[1,3,7]}"#);
        assert_eq!(chunks, [1u32, 3, 7].into_iter().collect());
    }

    #[test]
    fn test_resume_state_roundtrip() {
        let dir = std::env::temp_dir().join("ow_resume_test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(".abcd.state");
        let state = FileTransferState {
            filename: "a.txt".to_string(),
            total_size: 100,
            total_chunks: 4,
            chunk_size: 25,
            file_hash: [1u8; 32],
            received_chunks: [0u32, 2].into_iter().collect(),
            temp_path: dir.join(".abcd.tmp"),
            output_path: dir.join("a.txt"),
            status: TransferStatus::Downloading { received_bytes: 50 },
            started_at: std::time::Instant::now(),
            is_outbound: true,
            sender_pubkey_hex: "senderpubkey".to_string(),
            chunk_data_hasher: None,
        };
        state.persist_state(&path);

        let (meta, chunks) = load_resume_state(&path).expect("load new format");
        assert_eq!(meta.sender_pubkey_hex, "senderpubkey");
        assert_eq!(meta.filename, "a.txt");
        assert_eq!(meta.save_dir, dir.to_string_lossy());
        assert_eq!(chunks, [0u32, 2].into_iter().collect());

        // 旧格式（纯 CSV）无 meta → load_resume_state 返回 None
        std::fs::write(&path, "0,2").unwrap();
        assert!(load_resume_state(&path).is_none());

        let _ = std::fs::remove_file(&path);
    }
}
