use crate::CoreConfig;
use sqlx::{
    FromRow, Pool, Row, Sqlite,
    sqlite::{SqliteConnectOptions, SqlitePoolOptions},
};
use std::{path::Path, time::Duration};
mod migrations;
static DB_POOL: std::sync::OnceLock<Pool<Sqlite>> = std::sync::OnceLock::new();

// ========== 结构体 ==========

#[derive(Debug, Clone, FromRow)]
pub struct Contact {
    pub peer_id: String,
    pub name: Option<String>,
    pub added_at: i64,
}

#[derive(Debug, Clone, FromRow)]
pub struct Message {
    pub id: i64,
    pub peer_id: String,
    pub content: String,
    pub is_outgoing: i32,
    pub ts: i64,
    pub pending: i32,
}

// ========== 初始化 ==========

pub async fn init(cfg: &CoreConfig) -> anyhow::Result<()> {
    init_path(&cfg.database_path).await
}

pub async fn init_path(path: &Path) -> anyhow::Result<()> {
    if path.is_dir() {
        anyhow::bail!("Database path must be a file");
    }

    let is_new = !path.exists();
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p)?;
    }

    let pool = SqlitePoolOptions::new()
        .max_connections(7)
        .min_connections(1)
        .acquire_timeout(Duration::from_secs(7))
        .connect_with(
            SqliteConnectOptions::new()
                .filename(path)
                .create_if_missing(true)
                .foreign_keys(true)
                .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
                .synchronous(sqlx::sqlite::SqliteSynchronous::Normal)
                .pragma("cache_size", "-64000")
                .pragma("mmap_size", "268435456") // 内存映射，提升大查询性能
                .pragma("journal_size_limit", "67108864") // 限制 WAL 文件大小，避免无限膨胀
                .pragma("temp_store", "memory")
                .optimize_on_close(true, None)
                .busy_timeout(Duration::from_secs(5)),
        )
        .await?;

    sqlx::query("SELECT 1").execute(&pool).await?;

    if is_new {
        migrations::run(&pool).await?;
        crate::first_run();

        tracing::info!("New database initialized");
    }

    DB_POOL
        .set(pool)
        .map_err(|_| anyhow::anyhow!("Pool already initialized"))?;
    Ok(())
}

pub fn pool() -> Option<&'static Pool<Sqlite>> {
    DB_POOL.get()
}

async fn init_tables(pool: &Pool<Sqlite>) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await?;

    sqlx::query(
        r#"CREATE TABLE IF NOT EXISTS identity (
           peer_id TEXT PRIMARY KEY,
           key_enc BLOB
       )"#,
    )
    .execute(&mut *tx)
    .await?;

    sqlx::query(
        r#"CREATE TABLE IF NOT EXISTS contacts (
           peer_id TEXT PRIMARY KEY,
           name TEXT,
           added_at INTEGER DEFAULT (unixepoch())
       )"#,
    )
    .execute(&mut *tx)
    .await?;

    sqlx::query(
        r#"CREATE TABLE IF NOT EXISTS messages (
           id INTEGER PRIMARY KEY,
           peer_id TEXT NOT NULL REFERENCES contacts(peer_id) ON DELETE CASCADE,
           content TEXT NOT NULL,
           is_outgoing INTEGER NOT NULL CHECK(is_outgoing IN (0, 1)),
           pending INTEGER NOT NULL DEFAULT 0 CHECK(pending IN (0, 1, 2)),
           ts INTEGER NOT NULL DEFAULT (unixepoch())
       )"#,
    )
    .execute(&mut *tx)
    .await?;

    sqlx::query(
        r#"CREATE INDEX IF NOT EXISTS idx_messages_peer_ts 
          ON messages(peer_id, ts)"#,
    )
    .execute(&mut *tx)
    .await?;

    sqlx::query(
        r#"CREATE INDEX IF NOT EXISTS idx_pending 
          ON messages(pending) WHERE pending != 0"#,
    )
    .execute(&mut *tx)
    .await?;

    tx.commit().await
}

// ========== 身份管理 ==========

pub async fn set_identity(
    pool: &Pool<Sqlite>,
    peer_id: &str,
    key_enc: Option<&[u8]>,
) -> anyhow::Result<()> {
    sqlx::query(
        r#"INSERT INTO identity (peer_id, key_enc) VALUES (?1, ?2)
          ON CONFLICT(peer_id) DO UPDATE SET key_enc = excluded.key_enc"#,
    )
    .bind(peer_id)
    .bind(key_enc)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn get_identity(
    pool: &Pool<Sqlite>,
) -> anyhow::Result<Option<(String, Option<Vec<u8>>)>> {
    let row = sqlx::query("SELECT peer_id, key_enc FROM identity LIMIT 1")
        .fetch_optional(pool)
        .await?;
    Ok(row.map(|r| (r.get(0), r.get(1))))
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

// ========== 消息管理 ==========

pub async fn add_message(
    pool: &Pool<Sqlite>,
    peer_id: &str,
    content: &str,
    is_outgoing: bool,
    pending: bool,
) -> anyhow::Result<i64> {
    let pending_val = if pending { 1 } else { 0 };
    let row = sqlx::query(
        r#"INSERT INTO messages (peer_id, content, is_outgoing, pending, ts)
          VALUES (?1, ?2, ?3, ?4, unixepoch()) 
          RETURNING id"#,
    )
    .bind(peer_id)
    .bind(content)
    .bind(is_outgoing as i32)
    .bind(pending_val)
    .fetch_one(pool)
    .await?;
    Ok(row.get(0))
}

pub async fn get_message(pool: &Pool<Sqlite>, msg_id: i64) -> anyhow::Result<Option<Message>> {
    sqlx::query_as::<_, Message>(
        r#"SELECT id, peer_id, content, is_outgoing, ts, pending 
          FROM messages WHERE id = ?"#,
    )
    .bind(msg_id)
    .fetch_optional(pool)
    .await
    .map_err(Into::into)
}

pub async fn get_messages(
    pool: &Pool<Sqlite>,
    peer_id: &str,
    before: Option<i64>,
    limit: i64,
) -> anyhow::Result<Vec<Message>> {
    // 限制最大查询数量防止DoS
    let limit = limit.min(1000);

    sqlx::query_as::<_, Message>(
        r#"SELECT id, peer_id, content, is_outgoing, ts, pending FROM messages 
          WHERE peer_id = ?1 AND (?2 IS NULL OR ts < ?2) 
          ORDER BY ts DESC LIMIT ?3"#,
    )
    .bind(peer_id)
    .bind(before)
    .bind(limit)
    .fetch_all(pool)
    .await
    .map_err(Into::into)
}

/// 获取最后一条消息（修复：返回完整Message结构）
pub async fn get_last_message(
    pool: &Pool<Sqlite>,
    peer_id: &str,
) -> anyhow::Result<Option<Message>> {
    sqlx::query_as::<_, Message>(
        r#"SELECT id, peer_id, content, is_outgoing, ts, pending 
          FROM messages WHERE peer_id = ? ORDER BY ts DESC LIMIT 1"#,
    )
    .bind(peer_id)
    .fetch_optional(pool)
    .await
    .map_err(Into::into)
}

pub async fn delete_message(pool: &Pool<Sqlite>, msg_id: i64) -> anyhow::Result<u64> {
    Ok(sqlx::query("DELETE FROM messages WHERE id = ?")
        .bind(msg_id)
        .execute(pool)
        .await?
        .rows_affected())
}

// ========== 批量操作（新增） ==========

pub async fn add_messages_batch(
    pool: &Pool<Sqlite>,
    messages: &[(String, String, bool, bool)], // (peer_id, content, is_outgoing, pending)
) -> anyhow::Result<Vec<i64>> {
    let mut tx = pool.begin().await?;
    let mut ids = Vec::with_capacity(messages.len());

    for (peer_id, content, is_outgoing, pending) in messages {
        let pending_val = if *pending { 1 } else { 0 };
        let row = sqlx::query(
            r#"INSERT INTO messages (peer_id, content, is_outgoing, pending, ts)
              VALUES (?1, ?2, ?3, ?4, unixepoch()) RETURNING id"#,
        )
        .bind(peer_id)
        .bind(content)
        .bind(*is_outgoing as i32)
        .bind(pending_val)
        .fetch_one(&mut *tx)
        .await?;
        ids.push(row.get::<i64, _>(0));
    }

    tx.commit().await?;
    Ok(ids)
}

pub async fn mark_sent_batch(pool: &Pool<Sqlite>, ids: &[i64]) -> anyhow::Result<u64> {
    if ids.is_empty() {
        return Ok(0);
    }
    if ids.len() > 1000 {
        anyhow::bail!("Batch size too large");
    }

    // 使用参数化查询，无注入风险
    let placeholders: Vec<String> = (1..=ids.len()).map(|i| format!("?{}", i)).collect();
    let sql = format!(
        "UPDATE messages SET pending = 0 WHERE id IN ({})",
        placeholders.join(",")
    );

    let mut query = sqlx::query(&sql);
    for id in ids {
        query = query.bind(id);
    }

    Ok(query.execute(pool).await?.rows_affected())
}

pub async fn delete_messages_batch(pool: &Pool<Sqlite>, ids: &[i64]) -> anyhow::Result<u64> {
    if ids.is_empty() {
        return Ok(0);
    }
    if ids.len() > 1000 {
        anyhow::bail!("Batch size too large");
    }

    let placeholders: Vec<String> = (1..=ids.len()).map(|i| format!("?{}", i)).collect();
    let sql = format!(
        "DELETE FROM messages WHERE id IN ({})",
        placeholders.join(",")
    );

    let mut query = sqlx::query(&sql);
    for id in ids {
        query = query.bind(id);
    }

    Ok(query.execute(pool).await?.rows_affected())
}

// ========== 待发送消息管理 ==========

pub async fn list_pending(pool: &Pool<Sqlite>) -> anyhow::Result<Vec<Message>> {
    sqlx::query_as::<_, Message>(
        r#"SELECT id, peer_id, content, is_outgoing, ts, pending 
          FROM messages WHERE pending = 1 ORDER BY ts"#,
    )
    .fetch_all(pool)
    .await
    .map_err(Into::into)
}

pub async fn list_failed(pool: &Pool<Sqlite>) -> anyhow::Result<Vec<Message>> {
    sqlx::query_as::<_, Message>(
        r#"SELECT id, peer_id, content, is_outgoing, ts, pending 
          FROM messages WHERE pending = 2 ORDER BY ts"#,
    )
    .fetch_all(pool)
    .await
    .map_err(Into::into)
}

pub async fn mark_sent(pool: &Pool<Sqlite>, msg_id: i64) -> anyhow::Result<bool> {
    let rows = sqlx::query("UPDATE messages SET pending = 0 WHERE id = ?")
        .bind(msg_id)
        .execute(pool)
        .await?
        .rows_affected();
    Ok(rows > 0)
}

pub async fn mark_pending(pool: &Pool<Sqlite>, msg_id: i64) -> anyhow::Result<bool> {
    let rows = sqlx::query("UPDATE messages SET pending = 1 WHERE id = ?")
        .bind(msg_id)
        .execute(pool)
        .await?
        .rows_affected();
    Ok(rows > 0)
}

pub async fn mark_failed(pool: &Pool<Sqlite>, msg_id: i64) -> anyhow::Result<bool> {
    let rows = sqlx::query("UPDATE messages SET pending = 2 WHERE id = ?")
        .bind(msg_id)
        .execute(pool)
        .await?
        .rows_affected();
    Ok(rows > 0)
}

// ========== 统计查询==========

pub async fn get_message_count(pool: &Pool<Sqlite>, peer_id: &str) -> anyhow::Result<i64> {
    let row = sqlx::query("SELECT COUNT(*) FROM messages WHERE peer_id = ?")
        .bind(peer_id)
        .fetch_one(pool)
        .await?;
    Ok(row.get(0))
}

pub async fn get_unread_estimate(
    pool: &Pool<Sqlite>,
    peer_id: &str,
    last_read_ts: i64,
) -> anyhow::Result<i64> {
    let row = sqlx::query(
        r#"SELECT COUNT(*) FROM messages 
          WHERE peer_id = ? AND ts > ? AND is_outgoing = 0"#,
    )
    .bind(peer_id)
    .bind(last_read_ts)
    .fetch_one(pool)
    .await?;
    Ok(row.get(0))
}
