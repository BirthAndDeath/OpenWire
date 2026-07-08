use crate::crypto::constant_time_compare;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

use crate::error::{FileTransferError, FileTransferResult};

/// 文件哈希信息（FileHash 消息的 data 载荷）
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FileHashInfo {
    /// 文件唯一标识（SHA256）
    pub file_id: [u8; 32],
    /// 原始文件名
    pub filename: String,
    /// 文件总大小（字节）
    pub total_size: u64,
    /// 完整文件 SHA256
    pub file_hash: [u8; 32],
}

/// 文件流元数据（FileStream 消息的 data 载荷）
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FileStreamMeta {
    /// 文件唯一标识（SHA256）
    pub file_id: [u8; 32],
    /// 原始文件名
    pub filename: String,
    /// 文件总大小（字节）
    pub total_size: u64,
    /// 总分片数
    pub total_chunks: u32,
    /// 分片大小（字节）
    pub chunk_size: u32,
    /// 完整文件 SHA256
    pub file_hash: [u8; 32],
}

/// 文件流分片（FileStream 消息的 data 载荷）
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FileStreamChunk {
    /// 文件唯一标识（SHA256）
    pub file_id: [u8; 32],
    /// 原始文件名
    pub filename: String,
    /// 文件总大小（字节）
    pub total_size: u64,
    /// 总分片数
    pub total_chunks: u32,
    /// 分片大小（字节）
    pub chunk_size: u32,
    /// 分片序号
    pub chunk_index: u32,
    /// 文件读取偏移量
    pub offset: u64,
    /// 压缩后的分片数据（使用 zstd 压缩）
    pub chunk_data: Vec<u8>,
    /// 原始分片数据的 SHA256 哈希（用于完整性校验）
    pub chunk_hash: [u8; 32],
    /// 完整文件 SHA256
    pub file_hash: [u8; 32],
    /// 是否为最后一个分片
    pub is_last: bool,
}

/// 分片接收确认（文件下载请求的响应）
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChunkResponse {
    /// 文件唯一标识（SHA256）
    pub file_id: [u8; 32],
    /// 已接收的分片序号列表（用于断点续传）
    pub received_chunks: Vec<u32>,
}

/// 文件下载请求（接收方 → 发送方）
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DownloadRequest {
    /// 要下载的文件的 SHA256 哈希
    pub file_hash: [u8; 32],
}

/// 文件下载响应（发送方 → 接收方）
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DownloadResponse {
    /// 文件的 SHA256 哈希
    pub file_hash: [u8; 32],
    /// 发送方是否接受下载请求
    pub accepted: bool,
    /// 原始文件名（接受时返回）
    pub filename: Option<String>,
    /// 文件总大小（接受时返回）
    pub total_size: Option<u64>,
    /// 总分片数（接受时返回）
    pub total_chunks: Option<u32>,
    /// 分片大小（接受时返回）
    pub chunk_size: Option<u32>,
}

// ========== 文件哈希信息方法 ==========

impl FileHashInfo {
    /// 创建文件哈希信息
    pub fn new(filename: String, total_size: u64, file_hash: [u8; 32], file_id: [u8; 32]) -> Self {
        Self {
            file_id,
            filename,
            total_size,
            file_hash,
        }
    }
}

// ========== 文件流传输方法 ==========

impl FileStreamMeta {
    /// 创建文件流元数据
    pub fn new(
        filename: String,
        total_size: u64,
        chunk_size: u32,
        file_hash: [u8; 32],
        file_id: [u8; 32],
    ) -> Self {
        let total_chunks = if total_size == 0 {
            1
        } else {
            total_size.div_ceil(chunk_size as u64) as u32
        };

        Self {
            file_id,
            filename,
            total_size,
            total_chunks,
            chunk_size,
            file_hash,
        }
    }
}

/// 分片读取配置，用于 [`FileStreamChunk::from_file`] 的参数聚合
///
/// 将多个元数据参数打包为一个结构体，避免函数参数过多（clippy::too_many_arguments）
#[derive(Clone, Debug)]
pub struct ChunkReadConfig {
    /// 文件唯一标识
    pub file_id: [u8; 32],
    /// 原始文件名
    pub filename: String,
    /// 文件总大小
    pub total_size: u64,
    /// 总分片数
    pub total_chunks: u32,
    /// 分片大小
    pub chunk_size: u32,
    /// 分片序号
    pub chunk_index: u32,
    /// 文件读取偏移量
    pub offset: u64,
    /// 完整文件 SHA256
    pub file_hash: [u8; 32],
}

impl FileStreamChunk {
    /// 从文件中读取一个分片，压缩后返回 FileStreamChunk
    ///
    /// # 参数
    /// - `file`: 已打开的 tokio 文件句柄
    /// - `config`: 分片读取配置（包含 file_id, filename, total_size 等元数据）
    /// - `compression_level`: zstd 压缩等级
    ///
    /// # 返回
    /// (FileStreamChunk, 实际读取的字节数)
    pub async fn from_file(
        file: &mut File,
        config: &ChunkReadConfig,
        compression_level: i32,
    ) -> FileTransferResult<(Self, usize)> {
        use sha2::Digest;

        // 定位到指定偏移量
        file.seek(std::io::SeekFrom::Start(config.offset)).await?;

        // 读取分片数据
        let mut buf = vec![0u8; config.chunk_size as usize];
        let n = file.read(&mut buf).await?;
        buf.truncate(n);

        if n == 0 {
            return Err(FileTransferError::NoDataRead(config.offset));
        }

        // 计算原始数据哈希
        let chunk_hash = {
            let mut hasher = Sha256::new();
            hasher.update(&buf);
            hasher.finalize().into()
        };

        // 压缩分片数据
        let compressed = crate::compression::compress(&buf, compression_level).await?;

        let is_last = n < config.chunk_size as usize;

        Ok((
            Self {
                file_id: config.file_id,
                filename: config.filename.clone(),
                total_size: config.total_size,
                total_chunks: config.total_chunks,
                chunk_size: config.chunk_size,
                chunk_index: config.chunk_index,
                offset: config.offset,
                chunk_data: compressed,
                chunk_hash,
                file_hash: config.file_hash,
                is_last,
            },
            n,
        ))
    }

    /// 解压缩分片数据并写入文件
    ///
    /// # 参数
    /// - `file`: 已打开的 tokio 文件句柄（可写）
    /// - `expected_file_id`: 期望的 file_id，用于校验
    ///
    /// # 返回
    /// 写入的字节数
    pub async fn decompress_to_file(
        &self,
        file: &mut File,
        expected_file_id: &[u8; 32],
    ) -> FileTransferResult<usize> {
        // 校验 file_id
        if &self.file_id != expected_file_id {
            return Err(FileTransferError::FileIdMismatch {
                expected: *expected_file_id,
                got: self.file_id,
            });
        }

        // 解压缩
        let decompressed = crate::compression::decompress(&self.chunk_data).await?;

        // 校验分片哈希
        let computed_hash = {
            let mut hasher = Sha256::new();
            hasher.update(&decompressed);
            hasher.finalize()
        };
        if !constant_time_compare(&computed_hash, &self.chunk_hash) {
            return Err(FileTransferError::ChunkHashMismatch(self.chunk_index));
        }

        // 定位到指定偏移量并写入
        file.seek(std::io::SeekFrom::Start(self.offset)).await?;
        file.write_all(&decompressed).await?;

        Ok(decompressed.len())
    }
}
