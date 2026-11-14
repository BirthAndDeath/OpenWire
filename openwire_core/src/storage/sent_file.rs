#![allow(missing_docs)]

use crate::error::StorageResult;
use sqlx::{Pool, Row, Sqlite};

#[derive(Debug, Clone, sqlx::FromRow)]
/// 已发送文件记录
pub struct SentFile {
    /// 文件 SHA256 哈希
    pub file_hash: Vec<u8>,
    /// 文件本地路径
    pub file_path: String,
    /// 文件名
    pub filename: String,
    /// 文件总大小（字节）
    pub total_size: i64,
    /// 发送时间（Unix 时间戳）
    pub sent_at: i64,
    /// 授权下载的接收人 ML-DSA 公钥 hex（空=历史记录，已收回授权）
    pub recipient_pubkey_hex: String,
}

pub async fn add_sent_file(
    pool: &Pool<Sqlite>,
    file_hash: &[u8],
    file_path: &str,
    filename: &str,
    total_size: u64,
    recipient_pubkey_hex: &str,
) -> StorageResult<()> {
    sqlx::query(
        "INSERT OR REPLACE INTO sent_files (file_hash, file_path, filename, total_size, recipient_pubkey_hex) VALUES (?1, ?2, ?3, ?4, ?5)",
    )
    .bind(file_hash)
    .bind(file_path)
    .bind(filename)
    .bind(total_size as i64)
    .bind(recipient_pubkey_hex)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn get_sent_file(
    pool: &Pool<Sqlite>,
    file_hash: &[u8],
) -> StorageResult<Option<SentFile>> {
    sqlx::query_as::<_, SentFile>(
        "SELECT file_hash, file_path, filename, total_size, sent_at, recipient_pubkey_hex FROM sent_files WHERE file_hash = ?1",
    )
    .bind(file_hash)
    .fetch_optional(pool)
    .await
    .map_err(Into::into)
}

pub async fn list_all_sent_files(pool: &Pool<Sqlite>) -> StorageResult<Vec<SentFile>> {
    sqlx::query_as::<_, SentFile>(
        "SELECT file_hash, file_path, filename, total_size, sent_at, recipient_pubkey_hex FROM sent_files",
    )
    .fetch_all(pool)
    .await
    .map_err(Into::into)
}

pub async fn delete_sent_file(pool: &Pool<Sqlite>, file_hash: &[u8]) -> StorageResult<()> {
    let result = sqlx::query("DELETE FROM sent_files WHERE file_hash = ?1")
        .bind(file_hash)
        .execute(pool)
        .await?;
    if result.rows_affected() == 0 {
        tracing::debug!(
            "delete_sent_file: 未找到 hash={}.. 的记录",
            hex::encode(&file_hash[..8.min(file_hash.len())])
        );
    }
    Ok(())
}

/// 已发送文件记录的保留期（天）：超期记录在启动时清理，防止授权表无界增长
pub const SENT_FILE_RETENTION_DAYS: i64 = 90;

/// 清理超过保留期的已发送文件记录
pub async fn delete_sent_files_older_than(
    pool: &Pool<Sqlite>,
    keep_days: i64,
) -> StorageResult<u64> {
    let cutoff = now_unix() - keep_days * 86_400;
    let result = sqlx::query("DELETE FROM sent_files WHERE sent_at < ?1")
        .bind(cutoff)
        .execute(pool)
        .await?;
    Ok(result.rows_affected())
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

/// 查询某接收人的全部已发送文件记录
pub async fn list_sent_files_by_recipient(
    pool: &Pool<Sqlite>,
    recipient_pubkey_hex: &str,
) -> StorageResult<Vec<SentFile>> {
    sqlx::query_as::<_, SentFile>(
        "SELECT file_hash, file_path, filename, total_size, sent_at, recipient_pubkey_hex FROM sent_files WHERE recipient_pubkey_hex = ?1",
    )
    .bind(recipient_pubkey_hex)
    .fetch_all(pool)
    .await
    .map_err(Into::into)
}

/// 删除某接收人的全部已发送文件记录，返回对应本地路径（调用方负责删盘）
pub async fn delete_sent_files_by_recipient(
    pool: &Pool<Sqlite>,
    recipient_pubkey_hex: &str,
) -> StorageResult<Vec<String>> {
    let rows = list_sent_files_by_recipient(pool, recipient_pubkey_hex).await?;
    let paths: Vec<String> = rows.iter().map(|r| r.file_path.clone()).collect();
    sqlx::query("DELETE FROM sent_files WHERE recipient_pubkey_hex = ?1")
        .bind(recipient_pubkey_hex)
        .execute(pool)
        .await?;
    Ok(paths)
}

/// 某接收人的已发送文件占用总字节（存储管理用）
pub async fn sum_sent_bytes_by_recipient(
    pool: &Pool<Sqlite>,
    recipient_pubkey_hex: &str,
) -> StorageResult<i64> {
    let row = sqlx::query(
        "SELECT COALESCE(SUM(total_size), 0) FROM sent_files WHERE recipient_pubkey_hex = ?1",
    )
    .bind(recipient_pubkey_hex)
    .fetch_one(pool)
    .await?;
    Ok(row.get(0))
}

/// 大文件全量哈希的阈值：超过该大小的文件仅校验存在+大小，
/// 避免每次下载请求都 O(file) 重算哈希拖慢响应甚至触发 rr 超时。
const VERIFY_HASH_SIZE_LIMIT: u64 = 10 * 1024 * 1024;

pub async fn verify_sent_file(
    sent: &SentFile,
) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
    let path = std::path::Path::new(&sent.file_path);
    if !path.try_exists().unwrap_or(false) {
        return Ok(false);
    }
    let meta = tokio::fs::metadata(path).await?;
    if meta.len() as i64 != sent.total_size {
        return Ok(false);
    }
    if meta.len() <= VERIFY_HASH_SIZE_LIMIT {
        let current_hash = crate::transfer::compute_file_hash(path).await?;
        return Ok(current_hash.as_slice() == sent.file_hash.as_slice());
    }
    Ok(true)
}
