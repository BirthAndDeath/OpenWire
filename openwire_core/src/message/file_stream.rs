use serde::{Deserialize, Serialize};
use sha2::Sha256;
use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncSeekExt};

use crate::error::{FileTransferError, FileTransferResult};

/// 文件哈希信息（FileHash 消息的 data 载荷）
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FileHashInfo {
    /// 原始文件名
    pub filename: String,
    /// 文件总大小（字节）
    pub total_size: u64,
    /// 完整文件 SHA256
    pub file_hash: [u8; 32],
}

/// 文件流分片（FileStream 消息的 data 载荷）
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FileStreamChunk {
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

/// 加密后的文件流分片（`/chat/rr_file` 协议载荷）
///
/// 发送方向接收方传输的每个分片先按 [`FileStreamChunk`] 序列化，
/// 再经接收方 ML-KEM 公钥加密（`crypto::encrypt_message`）；
/// 明文不可见，防止传输窃听。发送方再用自己的 ML-DSA 私钥对
/// 密文签名，接收方验证签名还原出可信的发送者身份，防止伪造。
///
/// 分片约 256KB、postcard 序列化约 256KB、ML-KEM 加密后约 257KB、
/// 附签名（3309B）与公钥（1952B），合计仍远低于 4MB 上限。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EncryptedFileChunk {
    /// ML-KEM 加密后的 [`FileStreamChunk`] 序列化数据
    pub encrypted: Vec<u8>,
    /// 发送者对密文的 ML-DSA 签名（签名内容：encrypted）
    pub signature: Vec<u8>,
    /// 发送者 ML-DSA 公钥（原始格式，1952 字节）
    pub sender_public_key: Vec<u8>,
}

impl EncryptedFileChunk {
    /// 加密并签名一个明文分片
    ///
    /// # 参数
    /// - `chunk`: 明文分片（含压缩数据）
    /// - `recipient_mlkem_pubkey`: 接收方当前会话的 ML-KEM 公钥
    /// - `mldsa_private_key`: 发送方 ML-DSA 私钥（Zeroizing 内部）
    /// - `mldsa_public_key`: 发送方 ML-DSA 公钥
    pub fn seal(
        chunk: &FileStreamChunk,
        recipient_mlkem_pubkey: &[u8],
        mldsa_private_key: &[u8],
        mldsa_public_key: &[u8],
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let plain = postcard::to_allocvec(chunk)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, format!("序列化分片失败: {e}")))?;
        let encrypted = crate::crypto::encrypt_message(&plain, recipient_mlkem_pubkey)
            .map_err(|e| e.to_string())?;
        let signature = crate::signature::sign_data(mldsa_private_key, &encrypted)
            .map_err(|e| e.to_string())?;
        Ok(Self {
            encrypted,
            signature,
            sender_public_key: mldsa_public_key.to_vec(),
        })
    }

    /// 验证签名并解出明文分片
    ///
    /// 返回 `FileStreamChunk`，前提是：
    /// 1. 载荷携带的发送者公钥与 `expected_sender_public_key` 一致（恒定时间比较）
    /// 2. ML-DSA 签名对密文有效
    pub fn verify_and_open(
        &self,
        expected_sender_public_key: &[u8],
        decap_key: &aws_lc_rs::kem::DecapsulationKey,
    ) -> Result<FileStreamChunk, Box<dyn std::error::Error + Send + Sync>> {
        if !crate::crypto::constant_time_compare(&self.sender_public_key, expected_sender_public_key) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "分片发送者公钥与期望值不一致，拒绝接受",
            )
            .into());
        }
        match crate::signature::verify_signature(
            &self.sender_public_key,
            &self.encrypted,
            &self.signature,
        ) {
            Ok(true) => {}
            Ok(false) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "分片签名验证失败，拒绝接受",
                )
                .into());
            }
            Err(e) => return Err(e.into()),
        }
        let plain = crate::crypto::decrypt_message(&self.encrypted, decap_key)
            .map_err(|e| e.to_string())?;
        postcard::from_bytes(&plain)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, format!("反序列化分片失败: {e}")).into())
    }
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
    /// 拒绝原因（accepted=false 时返回）
    pub error_reason: Option<String>,
}

// ========== 文件哈希信息方法 ==========

impl FileHashInfo {
    /// 创建文件哈希信息
    pub fn new(filename: String, total_size: u64, file_hash: [u8; 32]) -> Self {
        Self {
            filename,
            total_size,
            file_hash,
        }
    }
}

/// 分片读取配置，用于 [`FileStreamChunk::from_file`] 的参数聚合
///
/// 将多个元数据参数打包为一个结构体，避免函数参数过多（clippy::too_many_arguments）
#[derive(Clone, Debug)]
pub struct ChunkReadConfig {
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
    /// - `config`: 分片读取配置（包含 file_hash, filename, total_size 等元数据）
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

    }
