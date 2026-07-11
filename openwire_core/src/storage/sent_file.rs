use sqlx::{Pool, Sqlite};

use crate::error::StorageResult;
//✅
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

/// 添加或更新已发送文件记录
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

/// 通过文件哈希查询已发送文件记录
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
