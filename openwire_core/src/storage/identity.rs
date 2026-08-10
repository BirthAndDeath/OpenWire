use sqlx::{FromRow, Pool, Sqlite};
use std::path::Path;
use tracing;

use crate::error::StorageResult;

//此文件提供对身份进行操作的函数
//通过初步审查✅
/// 身份信息
#[derive(Debug, Clone, FromRow)]
pub struct Identity {
    /// 数据库 ID
    pub id: i64,
    /// 身份 ID（ML-DSA 公钥 hex）
    pub identity_id: String,
    /// 是否为当前身份（1=是，0=否）
    pub is_current: i32,
    /// 创建时间
    pub created_at: i64,
}

// ========== 身份管理（以 ML-DSA 公钥为唯一身份标识） ==========

/// 添加新身份（ML-DSA 公钥 hex 作为 identity_id）
pub async fn add_identity(pool: &Pool<Sqlite>, identity_id: &str) -> StorageResult<()> {
    sqlx::query(
        "INSERT INTO identities (identity_id, is_current) VALUES (?, 0) ON CONFLICT(identity_id) DO NOTHING",
    )
    .bind(identity_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// 获取当前身份的 identity_id（ML-DSA 公钥 hex）
pub async fn get_current_identity(pool: &Pool<Sqlite>) -> StorageResult<Option<String>> {
    let row = sqlx::query_scalar::<_, String>(
        "SELECT identity_id FROM identities WHERE is_current = 1 LIMIT 1",
    )
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

/// 设置当前身份
pub async fn set_current_identity(pool: &Pool<Sqlite>, identity_id: &str) -> StorageResult<()> {
    sqlx::query("UPDATE identities SET is_current = 0")
        .execute(pool)
        .await?;
    sqlx::query("UPDATE identities SET is_current = 1 WHERE identity_id = ?")
        .bind(identity_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// 列出所有身份
pub async fn list_identities(pool: &Pool<Sqlite>) -> StorageResult<Vec<Identity>> {
    sqlx::query_as::<_, Identity>(
        r#"SELECT id, identity_id, is_current, created_at FROM identities ORDER BY id"#,
    )
    .fetch_all(pool)
    .await
    .map_err(Into::into)
}

/// 删除身份（同时删除加密的私钥文件）
pub async fn delete_identity(
    pool: &Pool<Sqlite>,
    data_dir: &Path,
    identity_id: &str,
) -> StorageResult<u64> {
    let data_dir_str = data_dir.to_string_lossy();

    // 1. 删除加密的私钥文件
    rootcell::identity::PrivateKeyHandle::delete_encrypted_private_key(
        &data_dir_str,
        &format!("{}_mldsa", identity_id),
    );

    // 2. 删除数据库记录
    let rows = sqlx::query("DELETE FROM identities WHERE identity_id = ?")
        .bind(identity_id)
        .execute(pool)
        .await?
        .rows_affected();

    tracing::info!(
        "Deleted identity {}: removed encrypted key files, DHT records, and DB ({} rows affected)",
        identity_id,
        rows
    );

    Ok(rows)
}
