use sqlx::{FromRow, Pool, Sqlite};

use crate::error::StorageResult;

#[derive(Debug, Clone, FromRow)]
/// 联系人记录
pub struct Contact {
    /// 对方 ML-DSA 公钥 hex
    pub mldsa_pubkey_hex: String,
    /// 所属身份（己方公钥 hex）
    pub owner_identity_id: String,
    /// 联系人名称（自定义昵称）
    pub name: Option<String>,
    /// ML-KEM 公钥（临时密钥交换，每次会话可更新）
    pub mlkem_public_key: Option<Vec<u8>>,
    /// 添加时间（Unix 时间戳）
    pub added_at: i64,
    /// 备注（自定义备注）
    pub notes: String,
    /// 分组名称（空 = 默认分组）
    pub group_name: String,
    /// 是否已屏蔽
    pub blocked: bool,
}

/// 空昵称时的默认显示名：公钥的 SHA256 哈希（DHT 键）前 12 位 hex。
/// 稳定、唯一性足够（48 bit），避免整串 3904 字符公钥直接当名字。
pub fn contact_display_name(pubkey_hex: &str) -> String {
    let digest = crate::actor::p2p::swarm_ops::dht_key(pubkey_hex);
    digest[..12.min(digest.len())].to_string()
}

/// 更新可自定义的联系人元数据（昵称/备注/分组/屏蔽）。
///
/// 参数语义：`None` = 保持该字段不变；`Some("")` = 清空；`Some(v)` = 设为 v。
/// 列名固定、值全部走绑定，无动态拼接注入面。
/// 返回是否命中该联系人并更新。
#[allow(clippy::too_many_arguments)]
pub async fn update_contact_meta(
    pool: &Pool<Sqlite>,
    owner_identity_id: &str,
    mldsa_pubkey_hex: &str,
    name: Option<&str>,
    notes: Option<&str>,
    group_name: Option<&str>,
    blocked: Option<bool>,
) -> StorageResult<bool> {
    let rows = sqlx::query(
        "UPDATE contacts SET
           name = COALESCE(?3, name),
           notes = COALESCE(?4, notes),
           group_name = COALESCE(?5, group_name),
           blocked = COALESCE(?6, blocked)
         WHERE owner_identity_id = ?1 AND mldsa_pubkey_hex = ?2",
    )
    .bind(owner_identity_id)
    .bind(mldsa_pubkey_hex)
    .bind(name)
    .bind(notes)
    .bind(group_name)
    .bind(blocked.map(|b| if b { 1 } else { 0 }))
    .execute(pool)
    .await?;
    Ok(rows.rows_affected() > 0)
}

/// 检查联系人是否已被屏蔽
pub async fn is_contact_blocked(
    pool: &Pool<Sqlite>,
    owner_identity_id: &str,
    mldsa_pubkey_hex: &str,
) -> StorageResult<bool> {
    let result = sqlx::query_scalar::<_, i64>(
        "SELECT blocked FROM contacts WHERE owner_identity_id = ?1 AND mldsa_pubkey_hex = ?2",
    )
    .bind(owner_identity_id)
    .bind(mldsa_pubkey_hex)
    .fetch_optional(pool)
    .await?;
    Ok(result.unwrap_or(0) != 0)
}

/// 添加或更新联系人（使用 owner_identity_id + mldsa_pubkey_hex 复合主键）
/// ML-KEM 公钥是临时交换的，每次会话可能变化
pub async fn upsert_contact(
    pool: &Pool<Sqlite>,
    owner_identity_id: &str,
    mldsa_pubkey_hex: &str,
    name: Option<&str>,
    mlkem_public_key: Option<&[u8]>,
) -> StorageResult<()> {
    sqlx::query(
        r#"INSERT INTO contacts (owner_identity_id, mldsa_pubkey_hex, name, mlkem_public_key, added_at)
          VALUES (?1, ?2, ?3, ?4, unixepoch())
          ON CONFLICT(owner_identity_id, mldsa_pubkey_hex) DO UPDATE
          SET name = COALESCE(excluded.name, contacts.name),
              mlkem_public_key = COALESCE(excluded.mlkem_public_key, contacts.mlkem_public_key)"#,
    )
    .bind(owner_identity_id)
    .bind(mldsa_pubkey_hex)
    .bind(name)
    .bind(mlkem_public_key)
    .execute(pool)
    .await?;
    Ok(())
}

/// 获取联系人的 ML-KEM 公钥（用于加密）
pub async fn get_contact_mlkem_pubkey(
    pool: &Pool<Sqlite>,
    owner_identity_id: &str,
    mldsa_pubkey_hex: &str,
) -> StorageResult<Option<Vec<u8>>> {
    let result = sqlx::query_scalar::<_, Vec<u8>>(
        "SELECT mlkem_public_key FROM contacts WHERE owner_identity_id = ?1 AND mldsa_pubkey_hex = ?2",
    )
    .bind(owner_identity_id)
    .bind(mldsa_pubkey_hex)
    .fetch_optional(pool)
    .await?;
    Ok(result)
}

/// 更新联系人的 ML-KEM 公钥（临时密钥交换，每次会话可更新）
pub async fn update_contact_mlkem_pubkey(
    pool: &Pool<Sqlite>,
    owner_identity_id: &str,
    mldsa_pubkey_hex: &str,
    mlkem_public_key: &[u8],
) -> StorageResult<bool> {
    let rows = sqlx::query(
        "UPDATE contacts SET mlkem_public_key = ?1 WHERE owner_identity_id = ?2 AND mldsa_pubkey_hex = ?3",
    )
    .bind(mlkem_public_key)
    .bind(owner_identity_id)
    .bind(mldsa_pubkey_hex)
    .execute(pool)
    .await?
    .rows_affected();
    Ok(rows > 0)
}

/// 通过 ML-DSA 公钥查找联系人（需要指定所属身份）
pub async fn get_contact_by_mldsa_pubkey(
    pool: &Pool<Sqlite>,
    owner_identity_id: &str,
    mldsa_pubkey: &[u8],
) -> StorageResult<Option<Contact>> {
    let mldsa_pubkey_hex = hex::encode(mldsa_pubkey);
    let result = sqlx::query_as::<_, Contact>(
        "SELECT mldsa_pubkey_hex, owner_identity_id, name, mlkem_public_key, added_at, notes, group_name, blocked FROM contacts WHERE owner_identity_id = ?1 AND mldsa_pubkey_hex = ?2",
    )
    .bind(owner_identity_id)
    .bind(&mldsa_pubkey_hex)
    .fetch_optional(pool)
    .await?;
    Ok(result)
}

/// 检查指定的 ML-DSA 公钥是否是已添加的联系人（好友）
pub async fn is_contact_exists(
    pool: &Pool<Sqlite>,
    owner_identity_id: &str,
    mldsa_pubkey_hex: &str,
) -> StorageResult<bool> {
    let result = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM contacts WHERE owner_identity_id = ?1 AND mldsa_pubkey_hex = ?2",
    )
    .bind(owner_identity_id)
    .bind(mldsa_pubkey_hex)
    .fetch_one(pool)
    .await?;
    Ok(result > 0)
}

/// 删除联系人
pub async fn delete_contact(
    pool: &Pool<Sqlite>,
    owner_identity_id: &str,
    mldsa_pubkey_hex: &str,
) -> StorageResult<u64> {
    Ok(
        sqlx::query("DELETE FROM contacts WHERE owner_identity_id = ?1 AND mldsa_pubkey_hex = ?2")
            .bind(owner_identity_id)
            .bind(mldsa_pubkey_hex)
            .execute(pool)
            .await?
            .rows_affected(),
    )
}

/// 列出指定身份下的所有联系人
pub async fn list_contacts(
    pool: &Pool<Sqlite>,
    owner_identity_id: &str,
) -> StorageResult<Vec<Contact>> {
    sqlx::query_as::<_, Contact>(
        r#"SELECT mldsa_pubkey_hex, owner_identity_id, name, mlkem_public_key, added_at, notes, group_name, blocked
          FROM contacts WHERE owner_identity_id = ? ORDER BY added_at DESC"#,
    )
    .bind(owner_identity_id)
    .fetch_all(pool)
    .await
    .map_err(Into::into)
}

/// 清理所有联系人的 ML-KEM 公钥（启动时调用）
///
/// 每次启动都会生成新的临时 ML-KEM 密钥对，旧的 ML-KEM 公钥已失效。
/// 如果不清理，lookup_mlkem_pubkey 在 DHT 本地数据库未命中时会回退到
/// contacts 表获取过时的公钥，导致加密消息后接收方解密失败。
///
/// 注意：此函数会清空所有联系人的 mlkem_public_key 字段，但保留联系人本身。
/// 新的 ML-KEM 公钥会在收到对方消息时通过 handle_incoming_request 自动更新，
/// 或通过 DHT 网络查询（GetRecord）获取。
pub async fn clear_all_mlkem_pubkeys(pool: &Pool<Sqlite>) -> StorageResult<u64> {
    let rows = sqlx::query("UPDATE contacts SET mlkem_public_key = NULL")
        .execute(pool)
        .await?
        .rows_affected();
    Ok(rows)
}
