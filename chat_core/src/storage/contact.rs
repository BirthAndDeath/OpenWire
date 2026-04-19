use sqlx::{FromRow, Pool, Sqlite};

#[derive(Debug, Clone, FromRow)]
pub struct Contact {
    pub peer_id: String,
    pub name: Option<String>,
    pub public_key: Option<Vec<u8>>,
    pub added_at: i64,
}

// ========== 联系人管理 ==========

pub async fn upsert_contact(
    pool: &Pool<Sqlite>,
    peer_id: &str,
    name: Option<&str>,
    public_key: Option<&[u8]>,
) -> anyhow::Result<()> {
    sqlx::query(
        r#"INSERT INTO contacts (peer_id, name, public_key, added_at) 
          VALUES (?1, ?2, ?3, unixepoch())
          ON CONFLICT(peer_id) DO UPDATE 
          SET name = COALESCE(excluded.name, contacts.name),
              public_key = COALESCE(excluded.public_key, contacts.public_key)"#,
    )
    .bind(peer_id)
    .bind(name)
    .bind(public_key)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn get_contact_public_key(
    pool: &Pool<Sqlite>,
    peer_id: &str,
) -> anyhow::Result<Option<Vec<u8>>> {
    let result = sqlx::query_scalar::<_, Vec<u8>>(
        "SELECT public_key FROM contacts WHERE peer_id = ?",
    )
    .bind(peer_id)
    .fetch_optional(pool)
    .await?;
    Ok(result)
}

/// 检查指定的 PeerID 是否是已添加的联系人（好友）
pub async fn is_contact_exists(pool: &Pool<Sqlite>, peer_id: &str) -> anyhow::Result<bool> {
    let result = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM contacts WHERE peer_id = ?",
    )
    .bind(peer_id)
    .fetch_one(pool)
    .await?;
    Ok(result > 0)
}

pub async fn delete_contact(pool: &Pool<Sqlite>, peer_id: &str) -> anyhow::Result<u64> {
    Ok(sqlx::query("DELETE FROM contacts WHERE peer_id = ?")
        .bind(peer_id)
        .execute(pool)
        .await?
        .rows_affected())
}

pub async fn list_contacts(pool: &Pool<Sqlite>) -> anyhow::Result<Vec<Contact>> {
    sqlx::query_as::<_, Contact>(
        r#"SELECT peer_id, name, public_key, added_at FROM contacts ORDER BY added_at DESC"#,
    )
    .fetch_all(pool)
    .await
    .map_err(Into::into)
}