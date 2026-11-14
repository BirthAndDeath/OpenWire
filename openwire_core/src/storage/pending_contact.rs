use crate::command::ScanContactInfo;
use sqlx::{FromRow, Pool, Sqlite};

use crate::error::StorageResult;

/// 扫码暂存记录（好友摘要化流程）
#[derive(Debug, Clone, FromRow)]
pub struct PendingContact {
    /// 自增 ID
    pub id: i64,
    /// 所属身份（己方公钥 hex）
    pub owner_identity_id: String,
    /// SHA256(ML-DSA 公钥) hex，64 字符
    pub digest_hex: String,
    /// 联系人名称（可选）
    pub name: Option<String>,
    /// 二维码推荐的 PeerID（Base58）
    pub peer_id: Option<String>,
    /// 二维码推荐的 Multiaddr 列表（JSON 数组）
    pub addrs: String,
    /// 二维码推荐的公共 relay 节点（JSON 数组）
    pub relay_addrs: String,
    /// 来源二维码的版本字段
    pub version: i64,
    /// 创建时间（Unix 时间戳）
    pub created_at: i64,
}

/// 新增扫码暂存记录；同 (owner, digest) 已存在时更新推荐信息
pub async fn add_pending_contact(
    pool: &Pool<Sqlite>,
    owner_identity_id: &str,
    info: &ScanContactInfo,
) -> StorageResult<()> {
    let addrs_json = serde_json::to_string(&info.addrs).unwrap_or_else(|_| "[]".to_string());
    let relay_json =
        serde_json::to_string(&info.relay_addrs).unwrap_or_else(|_| "[]".to_string());
    sqlx::query(
        r#"INSERT INTO pending_contacts
             (owner_identity_id, digest_hex, name, peer_id, addrs, relay_addrs, version, created_at)
           VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, unixepoch())
           ON CONFLICT(owner_identity_id, digest_hex) DO UPDATE
           SET name = COALESCE(excluded.name, pending_contacts.name),
               peer_id = COALESCE(excluded.peer_id, pending_contacts.peer_id),
               addrs = excluded.addrs,
               relay_addrs = excluded.relay_addrs,
               version = excluded.version"#,
    )
    .bind(owner_identity_id)
    .bind(&info.digest_hex)
    .bind(info.name.as_deref())
    .bind(info.peer_id.as_deref())
    .bind(&addrs_json)
    .bind(&relay_json)
    .bind(info.version)
    .execute(pool)
    .await?;
    Ok(())
}

/// 查询指定身份的暂存记录列表
pub async fn list_pending_contacts(
    pool: &Pool<Sqlite>,
    owner_identity_id: &str,
) -> StorageResult<Vec<PendingContact>> {
    sqlx::query_as::<_, PendingContact>(
        r#"SELECT id, owner_identity_id, digest_hex, name, peer_id, addrs, relay_addrs, version, created_at
           FROM pending_contacts WHERE owner_identity_id = ?1 ORDER BY created_at DESC"#,
    )
    .bind(owner_identity_id)
    .fetch_all(pool)
    .await
    .map_err(Into::into)
}

/// 按 digest 查询暂存记录
pub async fn get_pending_contact_by_digest(
    pool: &Pool<Sqlite>,
    owner_identity_id: &str,
    digest_hex: &str,
) -> StorageResult<Option<PendingContact>> {
    sqlx::query_as::<_, PendingContact>(
        r#"SELECT id, owner_identity_id, digest_hex, name, peer_id, addrs, relay_addrs, version, created_at
           FROM pending_contacts WHERE owner_identity_id = ?1 AND digest_hex = ?2"#,
    )
    .bind(owner_identity_id)
    .bind(digest_hex)
    .fetch_optional(pool)
    .await
    .map_err(Into::into)
}

/// 匹配成功（或放弃）后删除暂存记录
pub async fn delete_pending_contact(
    pool: &Pool<Sqlite>,
    owner_identity_id: &str,
    digest_hex: &str,
) -> StorageResult<()> {
    sqlx::query("DELETE FROM pending_contacts WHERE owner_identity_id = ?1 AND digest_hex = ?2")
        .bind(owner_identity_id)
        .bind(digest_hex)
        .execute(pool)
        .await?;
    Ok(())
}
