//! # 压缩/解压缩模块
//!
//! 统一压缩接口，明确消息类型与压缩策略的对应关系：
//!
//! | 消息类型 | 压缩策略 |
//! |----------|---------|
//! | `Text` | 不压缩 |
//! | `FileHash` | 不压缩 |
//! | `FileDownloadRequest` | 不压缩 |
//! | `FileStream` | zstd 压缩（level=3，分片数据在 [`FileStreamChunk::from_file`] 中独立压缩） |
//!
//! ## 设计原则
//!
//! - **按消息类型选择**：根据 [`ChatMessageType`] 决定是否压缩，而非数据大小
//! - **内存压缩**：`compress()` / `decompress()` 用于小数据（<1MB），一次性加载到内存
//! - **文件流压缩**：`compress_file()` / `decompress_file()` 用于大文件，流式处理避免内存溢出

use async_compression::futures::bufread::{ZstdDecoder, ZstdEncoder};
use futures::io::{AsyncReadExt, AsyncWriteExt, BufReader};
use std::path::Path;
use tokio::fs::File;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

/// 解压缩最大数据大小（1MB）
const DECOMPRESS_MAX_SIZE: usize = 1024 * 1024;

/// 根据消息类型选择 zstd 压缩等级
///
/// - `FileStream`: 使用 zstd level=3（快速压缩，适合网络传输）
/// - 其他类型（`Text` / `FileHash` / `FileDownloadRequest`）: 不压缩 (level=0)
pub fn compression_level(msgtype: crate::message::ChatMessageType) -> i32 {
    match msgtype {
        crate::message::ChatMessageType::FileStream => 3,
        _ => 0,
    }
}

/// 异步压缩字节数组（内存压缩，适用于小数据）
///
/// # 参数
/// - `data`: 原始数据
/// - `level`: zstd 压缩等级（0=不压缩, 1-22）
///
/// # 注意
/// 此方法会将整个数据加载到内存，仅适用于小数据（建议 <1MB）。
/// 对于大文件，请使用 [`compress_file`] 进行流式处理。
pub async fn compress(data: &[u8], level: i32) -> crate::error::CompressionResult<Vec<u8>> {
    use futures::io::Cursor;

    let reader = Cursor::new(data);
    let buf_reader = BufReader::new(reader);
    let mut encoder =
        ZstdEncoder::with_quality(buf_reader, async_compression::Level::Precise(level));

    let mut output = Vec::new();
    futures::io::copy(&mut encoder, &mut output)
        .await
        .map_err(crate::error::CompressionError::CompressFailed)?;

    Ok(output)
}

/// 异步解压缩字节数组（内存解压缩，适用于小数据）
///
/// # 参数
/// - `data`: 已压缩的数据
///
/// # 错误
/// - 输入数据为空
/// - 输入数据超过 [`DECOMPRESS_MAX_SIZE`] 的两倍（防止 DoS）
/// - 解压后数据超过 [`DECOMPRESS_MAX_SIZE`]
///
/// # 注意
/// 此方法会将整个数据加载到内存，仅适用于小数据（建议 <1MB）。
/// 对于大文件，请使用 [`decompress_file`] 进行流式处理。
pub async fn decompress(data: &[u8]) -> crate::error::CompressionResult<Vec<u8>> {
    if data.is_empty() {
        return Err(crate::error::CompressionError::CompressedDataEmpty);
    }

    // 限制输入数据大小，防止处理过大的非有效负载或潜在的 DoS
    if data.len() > DECOMPRESS_MAX_SIZE * 2 {
        return Err(crate::error::CompressionError::CompressedDataTooLarge(
            data.len(),
        ));
    }

    use futures::io::{AsyncReadExt, Cursor};

    let reader = Cursor::new(data);
    let mut decoder = ZstdDecoder::new(reader);

    let mut output = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        let n = decoder
            .read(&mut buf)
            .await
            .map_err(crate::error::CompressionError::DecompressFailed)?;
        if n == 0 {
            break;
        }
        if output.len().saturating_add(n) > DECOMPRESS_MAX_SIZE {
            return Err(crate::error::CompressionError::DecompressedDataTooLarge(
                output.len().saturating_add(n),
            ));
        }
        output.extend_from_slice(&buf[..n]);
    }

    Ok(output)
}

/// 异步流式压缩文件
///
/// 从输入文件读取数据，压缩后写入输出文件。
/// 适用于大文件的流式处理，避免一次性加载到内存。
///
/// # 参数
/// - `input_path`: 源文件路径
/// - `output_path`: 目标文件路径（压缩后）
/// - `level`: zstd 压缩等级
pub async fn compress_file(
    input_path: &Path,
    output_path: &Path,
    level: i32,
) -> crate::error::CompressionResult<()> {
    let input_file = File::open(input_path)
        .await
        .map_err(crate::error::CompressionError::FileIoError)?;
    let output_file = File::create(output_path)
        .await
        .map_err(crate::error::CompressionError::FileIoError)?;

    // 将 tokio AsyncRead 转换为 futures AsyncRead
    let reader = input_file.compat();
    let buf_reader = BufReader::new(reader);

    // 创建 zstd 编码器（使用 bufread API）
    let mut encoder =
        ZstdEncoder::with_quality(buf_reader, async_compression::Level::Precise(level));

    // 将 tokio AsyncWrite 转换为 futures AsyncWrite
    let mut writer = output_file.compat_write();

    // 使用 futures::io::copy 进行流式复制（自动压缩）
    futures::io::copy(&mut encoder, &mut writer)
        .await
        .map_err(crate::error::CompressionError::CompressFailed)?;

    Ok(())
}

/// 异步流式解压缩文件
///
/// 从输入文件读取压缩数据，解压后写入输出文件。
/// 适用于大文件的流式处理，避免一次性加载到内存。
///
/// # 参数
/// - `input_path`: 已压缩的源文件路径
/// - `output_path`: 目标文件路径（解压后）
pub async fn decompress_file(
    input_path: &Path,
    output_path: &Path,
) -> crate::error::CompressionResult<()> {
    let input_file = File::open(input_path)
        .await
        .map_err(crate::error::CompressionError::FileIoError)?;
    let output_file = File::create(output_path)
        .await
        .map_err(crate::error::CompressionError::FileIoError)?;

    // 将 tokio AsyncRead 转换为 futures AsyncRead
    let reader = input_file.compat();
    let buf_reader = BufReader::new(reader);

    // 创建 zstd 解码器（使用 bufread API）
    let mut decoder = ZstdDecoder::new(buf_reader);

    // 将 tokio AsyncWrite 转换为 futures AsyncWrite
    let mut writer = output_file.compat_write();

    // 流式解压并限制输出大小：`DECOMPRESS_MAX_SIZE` 仍适用于磁盘输出，
    // 防止恶意 zstd 炸弹填满磁盘。超限时删除已写入的部分。
    let mut total: usize = 0;
    let mut buf = [0u8; 8192];
    loop {
        let n = decoder
            .read(&mut buf)
            .await
            .map_err(crate::error::CompressionError::DecompressFailed)?;
        if n == 0 {
            break;
        }
        total = total.saturating_add(n);
        if total > DECOMPRESS_MAX_SIZE {
            // 先关闭输出句柄再删除，否则 Windows 上删除打开中的文件会静默失败
            drop(writer);
            drop(decoder);
            let _ = std::fs::remove_file(output_path);
            return Err(crate::error::CompressionError::DecompressedDataTooLarge(total));
        }
        writer
            .write_all(&buf[..n])
            .await
            .map_err(crate::error::CompressionError::DecompressFailed)?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::ChatMessageType;

    #[tokio::test]
    async fn test_compress_decompress_roundtrip() {
        let data = b"Hello, world! This is a test message for compression.";
        let compressed = compress(data, 3).await.unwrap();
        let decompressed = decompress(&compressed).await.unwrap();
        assert_eq!(data.to_vec(), decompressed);
    }

    #[tokio::test]
    async fn test_compress_level_zero() {
        let data = b"Small data";
        // level=0 表示不压缩（存储模式）
        let compressed = compress(data, 0).await.unwrap();
        let decompressed = decompress(&compressed).await.unwrap();
        assert_eq!(data.to_vec(), decompressed);
    }

    #[tokio::test]
    async fn test_decompress_empty_data() {
        let result = decompress(&[]).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("空"));
    }

    #[test]
    fn test_compression_level_by_msgtype() {
        // FileStream 类型应压缩 (level=3)
        assert_eq!(compression_level(ChatMessageType::FileStream), 3);
        // Text 类型不压缩
        assert_eq!(compression_level(ChatMessageType::Text), 0);
        // FileHash 类型不压缩
        assert_eq!(compression_level(ChatMessageType::FileHash), 0);
        // FileDownloadRequest 类型不压缩
        assert_eq!(compression_level(ChatMessageType::FileDownloadRequest), 0);
    }
}
