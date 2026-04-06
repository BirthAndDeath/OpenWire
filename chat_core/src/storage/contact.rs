use sqlx::{FromRow, Pool, Sqlite};

#[derive(Debug, Clone, FromRow)]
pub struct Contact {
    pub peer_id: String,
    pub name: Option<String>,
    pub added_at: i64,
}

// ========== 联系人管理 ==========

pub async fn upsert_contact(
    pool: &Pool<Sqlite>,
    peer_id: &str,
    name: Option<&str>,
) -> anyhow::Result<()> {
    sqlx::query(
        r#"INSERT INTO contacts (peer_id, name, added_at) 
          VALUES (?1, ?2, unixepoch())
          ON CONFLICT(peer_id) DO UPDATE 
          SET name = COALESCE(excluded.name, contacts.name)"#,
    )
    .bind(peer_id)
    .bind(name)
    .execute(pool)
    .await?;
    Ok(())
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
        r#"SELECT peer_id, name, added_at FROM contacts ORDER BY added_at DESC"#,
    )
    .fetch_all(pool)
    .await
    .map_err(Into::into)
}