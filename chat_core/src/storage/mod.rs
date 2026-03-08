use crate::CoreConfig;
use sqlx::{
    FromRow, Pool, Row, Sqlite,
    sqlite::{SqliteConnectOptions, SqlitePoolOptions},
};
use std::{path::Path, time::Duration};

static DB_POOL: std::sync::OnceLock<Pool<Sqlite>> = std::sync::OnceLock::new();

// ========== 结构体 ==========

#[derive(Debug, FromRow)]
pub struct Contact {
    pub peer_id: String,
    pub name: Option<String>,
    pub added_at: i64,
}

#[derive(Debug, FromRow)]
pub struct Message {
    pub id: i64,
    pub content: String,
    pub is_outgoing: i32,
    pub ts: i64,
}

// ========== 初始化 ==========

pub async fn init(cfg: &CoreConfig) -> anyhow::Result<()> {
    init_path(&cfg.database_path).await
}

pub async fn init_path(db_path: &Path) -> anyhow::Result<()> {
    if db_path.is_dir() {
        anyhow::bail!("Database path must be a file");
    }

    let is_new = !db_path.exists();
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let pool = SqlitePoolOptions::new()
        .acquire_timeout(Duration::from_secs(30))
        .connect_with(
            SqliteConnectOptions::new()
                .filename(db_path)
                .create_if_missing(true)
                .foreign_keys(true)
                .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
                .pragma("cache_size", "-64000")
                .busy_timeout(Duration::from_secs(5)),
        )
        .await
        .map_err(|e| anyhow::anyhow!("Database connection failed: {}", e))?;

    sqlx::query("SELECT 1").execute(&pool).await?;

    if is_new {
        init_tables(&pool).await?;
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
    sqlx::query(
        r#"CREATE TABLE IF NOT EXISTS identity (
        peer_id TEXT PRIMARY KEY,
        key_enc BLOB
    )"#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"CREATE TABLE IF NOT EXISTS contacts (
        peer_id TEXT PRIMARY KEY,
        name TEXT,
        added_at INTEGER DEFAULT (unixepoch())
    )"#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"CREATE TABLE IF NOT EXISTS messages (
        id INTEGER PRIMARY KEY,
        peer_id TEXT NOT NULL REFERENCES contacts(peer_id) ON DELETE CASCADE,
        content TEXT NOT NULL,
        is_outgoing INTEGER NOT NULL CHECK(is_outgoing IN (0, 1)),
        ts INTEGER NOT NULL DEFAULT (unixepoch())
    )"#,
    )
    .execute(pool)
    .await?;

    // 添加索引以提高查询性能
    sqlx::query(r#"CREATE INDEX IF NOT EXISTS idx_messages_peer_ts ON messages(peer_id, ts)"#)
        .execute(pool)
        .await?;
    //待做：增加离线未发送消息，当拨号成功自动发送

    Ok(())
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
       ON CONFLICT(peer_id) DO UPDATE SET
           name = COALESCE(excluded.name, contacts.name)"#,
    )
    .bind(peer_id)
    .bind(name)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn delete_contact(pool: &Pool<Sqlite>, peer_id: &str) -> anyhow::Result<u64> {
    let res = sqlx::query("DELETE FROM contacts WHERE peer_id = ?")
        .bind(peer_id)
        .execute(pool)
        .await?;
    Ok(res.rows_affected())
}

pub async fn list_contacts(pool: &Pool<Sqlite>) -> anyhow::Result<Vec<Contact>> {
    let rows = sqlx::query_as::<_, Contact>(
        r#"SELECT peer_id, name, added_at
           FROM contacts ORDER BY added_at DESC"#,
    )
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

// ========== 消息管理 ==========

pub async fn add_message(
    pool: &Pool<Sqlite>,
    peer_id: &str,
    content: &str,
    is_outgoing: bool,
) -> anyhow::Result<i64> {
    let is_out = is_outgoing as i32;
    let row = sqlx::query(
        r#"INSERT INTO messages (peer_id, content, is_outgoing, ts)
       VALUES (?1, ?2, ?3, unixepoch())
       RETURNING id"#,
    )
    .bind(peer_id)
    .bind(content)
    .bind(is_out)
    .fetch_one(pool)
    .await?;
    let id: i64 = row.get(0);
    Ok(id)
}

pub async fn get_messages(
    pool: &Pool<Sqlite>,
    peer_id: &str,
    before: Option<i64>,
    limit: i64,
) -> anyhow::Result<Vec<Message>> {
    let msgs = if let Some(ts) = before {
        sqlx::query_as::<_, Message>(
            r#"SELECT id, content, is_outgoing, ts
           FROM messages WHERE peer_id = ? AND ts < ?
           ORDER BY ts DESC LIMIT ?"#,
        )
        .bind(peer_id)
        .bind(ts)
        .bind(limit)
        .fetch_all(pool)
        .await?
    } else {
        sqlx::query_as::<_, Message>(
            r#"SELECT id, content, is_outgoing, ts
           FROM messages WHERE peer_id = ?
           ORDER BY ts DESC LIMIT ?"#,
        )
        .bind(peer_id)
        .bind(limit)
        .fetch_all(pool)
        .await?
    };
    Ok(msgs)
}

pub async fn get_last_message(
    pool: &Pool<Sqlite>,
    peer_id: &str,
) -> anyhow::Result<Option<(String, i64)>> {
    let row = sqlx::query(
        r#"SELECT content, ts FROM messages 
       WHERE peer_id = ? ORDER BY ts DESC LIMIT 1"#,
    )
    .bind(peer_id)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|r| (r.get(0), r.get(1))))
}

pub async fn delete_message(pool: &Pool<Sqlite>, msg_id: i64) -> anyhow::Result<()> {
    sqlx::query("DELETE FROM messages WHERE id = ?")
        .bind(msg_id)
        .execute(pool)
        .await?;
    Ok(())
}
