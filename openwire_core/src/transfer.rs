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

/// 最大并发文件传输数
/// 超过此限制的新下载请求将被拒绝
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
}

impl FileTransferState {
    /// 计算已接收字节数（最后一个分片可能小于 chunk_size）
    pub fn received_bytes(&self) -> u64 {
        if self.total_chunks == 0 || self.chunk_size == 0 {
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

    /// 持久化已接收分片列表到状态文件（断点续传）
    pub fn persist_state(&self, state_path: &Path) {
        let csv: String = self
            .received_chunks
            .iter()
            .map(|c| c.to_string())
            .collect::<Vec<_>>()
            .join(",");
        let _ = std::fs::write(state_path, &csv);
    }
}
