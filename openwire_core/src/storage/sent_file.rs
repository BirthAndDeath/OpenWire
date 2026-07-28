#![allow(missing_docs)]

use crate::error::StorageResult;
use sqlx::{Pool, Sqlite};

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
}

#[allow(dead_code)]
fn feature_err<T>() -> StorageResult<T> {
    Err(crate::error::StorageError::FeatureNotEnabled(
        "sqlite_history_storage",
    ))
}

#[cfg(feature = "sqlite_history_storage")]
pub async fn add_sent_file(
    pool: &Pool<Sqlite>,
    file_hash: &[u8],
    file_path: &str,
    filename: &str,
    total_size: u64,
) -> StorageResult<()> {
    sqlx::query(
        "INSERT OR REPLACE INTO sent_files (file_hash, file_path, filename, total_size) VALUES (?1, ?2, ?3, ?4)",
    )
    .bind(file_hash)
    .bind(file_path)
    .bind(filename)
    .bind(total_size as i64)
    .execute(pool)
    .await?;
    Ok(())
}
#[cfg(not(feature = "sqlite_history_storage"))]
pub async fn add_sent_file(
    _pool: &Pool<Sqlite>,
    _file_hash: &[u8],
    _file_path: &str,
    _filename: &str,
    _total_size: u64,
) -> StorageResult<()> {
    feature_err()
}

#[cfg(feature = "sqlite_history_storage")]
pub async fn get_sent_file(
    pool: &Pool<Sqlite>,
    file_hash: &[u8],
) -> StorageResult<Option<SentFile>> {
    sqlx::query_as::<_, SentFile>(
        "SELECT file_hash, file_path, filename, total_size, sent_at FROM sent_files WHERE file_hash = ?1",
    )
    .bind(file_hash)
    .fetch_optional(pool)
    .await
    .map_err(Into::into)
}
#[cfg(not(feature = "sqlite_history_storage"))]
pub async fn get_sent_file(
    _pool: &Pool<Sqlite>,
    _file_hash: &[u8],
) -> StorageResult<Option<SentFile>> {
    feature_err()
}

#[cfg(feature = "sqlite_history_storage")]
pub async fn list_all_sent_files(pool: &Pool<Sqlite>) -> StorageResult<Vec<SentFile>> {
    sqlx::query_as::<_, SentFile>(
        "SELECT file_hash, file_path, filename, total_size, sent_at FROM sent_files",
    )
    .fetch_all(pool)
    .await
    .map_err(Into::into)
}
#[cfg(not(feature = "sqlite_history_storage"))]
pub async fn list_all_sent_files(_pool: &Pool<Sqlite>) -> StorageResult<Vec<SentFile>> {
    feature_err()
}

#[cfg(feature = "sqlite_history_storage")]
pub async fn delete_sent_file(pool: &Pool<Sqlite>, file_hash: &[u8]) -> StorageResult<()> {
    let result = sqlx::query("DELETE FROM sent_files WHERE file_hash = ?1")
        .bind(file_hash)
        .execute(pool)
        .await?;
    if result.rows_affected() == 0 {
        tracing::debug!(
            "delete_sent_file: 未找到 hash={}.. 的记录",
            hex::encode(&file_hash[..8])
        );
    }
    Ok(())
}
#[cfg(not(feature = "sqlite_history_storage"))]
pub async fn delete_sent_file(_pool: &Pool<Sqlite>, _file_hash: &[u8]) -> StorageResult<()> {
    feature_err()
}

const VERIFY_HASH_SIZE_LIMIT: u64 = 10 * 1024 * 1024;

#[cfg(feature = "sqlite_history_storage")]
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
        Ok(current_hash.as_slice() == sent.file_hash.as_slice())
    } else {
        Ok(true)
    }
}
#[cfg(not(feature = "sqlite_history_storage"))]
pub async fn verify_sent_file(
    _sent: &SentFile,
) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
    Err("sqlite_history_storage feature is not enabled".into())
}
