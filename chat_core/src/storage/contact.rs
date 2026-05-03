use sqlx::{FromRow, Pool, Sqlite};

#[derive(Debug, Clone, FromRow)]
pub struct Contact {
    pub mldsa_pubkey_hex: String,          // 对方 ML-DSA 公钥 hex
    pub owner_identity_id: String,         // 所属身份（己方公钥 hex）
    pub name: Option<String>,              // 联系人名称
    pub mlkem_public_key: Option<Vec<u8>>, // ML-KEM 公钥（临时密钥交换，每次会话可更新）
    pub added_at: i64,                     // 添加时间
}

// ========== 联系人管理 ==========

/// 添加或更新联系人（使用 owner_identity_id + mldsa_pubkey_hex 复合主键）
/// ML-KEM 公钥是临时交换的，每次会话可能变化
pub async fn upsert_contact(
    pool: &Pool<Sqlite>,
    owner_identity_id: &str,
    mldsa_pubkey_hex: &str,
    name: Option<&str>,
    mlkem_public_key: Option<&[u8]>,
) -> anyhow::Result<()> {
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
) -> anyhow::Result<Option<Vec<u8>>> {
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
) -> anyhow::Result<bool> {
    let rows = sqlx::query(
        "UPDATE contacts SET mlkem_public_key = ? WHERE owner_identity_id = ?2 AND mldsa_pubkey_hex = ?3",
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
) -> anyhow::Result<Option<Contact>> {
    let mldsa_pubkey_hex = hex::encode(mldsa_pubkey);
    let result = sqlx::query_as::<_, Contact>(
        "SELECT mldsa_pubkey_hex, owner_identity_id, name, mlkem_public_key, added_at FROM contacts WHERE owner_identity_id = ?1 AND mldsa_pubkey_hex = ?2",
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
) -> anyhow::Result<bool> {
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
) -> anyhow::Result<u64> {
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
) -> anyhow::Result<Vec<Contact>> {
    sqlx::query_as::<_, Contact>(
        r#"SELECT mldsa_pubkey_hex, owner_identity_id, name, mlkem_public_key, added_at
          FROM contacts WHERE owner_identity_id = ? ORDER BY added_at DESC"#,
    )
    .bind(owner_identity_id)
    .fetch_all(pool)
    .await
    .map_err(Into::into)
}
