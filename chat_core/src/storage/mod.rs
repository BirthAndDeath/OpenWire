use sqlx::{
    Pool, Sqlite,
    sqlite::{SqliteConnectOptions, SqlitePoolOptions},
};
use std::time::Duration;

use crate::CoreConfig;
static DB_POOL: std::sync::OnceLock<Pool<Sqlite>> = std::sync::OnceLock::new();

pub async fn init(cfg: &CoreConfig) -> anyhow::Result<()> {
    let db_path = cfg.database_path.clone();
    if db_path.is_dir() {
        anyhow::bail!("Database path must be a file, not directory");
    }

    // 首次创建标记
    let is_new = !db_path.exists();
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let pool = SqlitePoolOptions::new()
        .max_connections(10)
        .acquire_timeout(Duration::from_secs(30))
        .connect_with(
            SqliteConnectOptions::new()
                .filename(db_path)
                .create_if_missing(true)
                .foreign_keys(true)
                .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
                .synchronous(sqlx::sqlite::SqliteSynchronous::Normal)
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

/// 初始化数据库表结构
///
/// 创建三个核心数据表：identities（身份信息表）、contacts（联系人表）和messages（消息表）
/// 并建立相应的索引以优化查询性能
///
/// # 参数
/// * `pool` - SQLite数据库连接池引用
///
/// # 返回值
/// * `Result<(), sqlx::Error>` - 成功时返回空元组，失败时返回SQLx错误
async fn init_tables(pool: &Pool<Sqlite>) -> Result<(), sqlx::Error> {
    // 创建身份信息表：存储节点的身份信息，包括公钥、私钥等
    sqlx::query(
        r#"CREATE TABLE IF NOT EXISTS identities (
            peer_id TEXT PRIMARY KEY,
            public_key TEXT UNIQUE NOT NULL,
            private_key_enc TEXT NOT NULL DEFAULT '',
            created_at DATETIME DEFAULT CURRENT_TIMESTAMP
        )"#,
    )
    .execute(pool)
    .await?;

    // 创建联系人表：存储联系人的基本信息
    sqlx::query(
        r#"CREATE TABLE IF NOT EXISTS contacts (
            peer_id TEXT PRIMARY KEY,
            public_key TEXT NOT NULL,
            name TEXT,
            added_by TEXT NOT NULL,
            created_at DATETIME DEFAULT CURRENT_TIMESTAMP
        )"#,
    )
    .execute(pool)
    .await?;

    // 创建消息表并建立索引：存储消息记录并按联系人和时间排序
    sqlx::query(
        r#"CREATE TABLE IF NOT EXISTS messages (
            id INTEGER PRIMARY KEY,
            peer_id TEXT NOT NULL REFERENCES contacts(peer_id),
            content TEXT NOT NULL,
            is_outgoing BOOLEAN NOT NULL,
            timestamp INTEGER NOT NULL,
            status TEXT DEFAULT 'pending'
        );
        CREATE INDEX IF NOT EXISTS idx_messages_peer_time ON messages(peer_id, timestamp)"#,
    )
    .execute(pool)
    .await?;

    Ok(())
}
