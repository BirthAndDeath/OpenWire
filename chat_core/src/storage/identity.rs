use crate::CoreConfig;
use sqlx::{
    FromRow, Pool, Sqlite,
    sqlite::{SqliteConnectOptions, SqlitePoolOptions},
};
use std::{path::Path, time::Duration};
use tracing;

static DB_POOL: std::sync::OnceLock<Pool<Sqlite>> = std::sync::OnceLock::new();

#[derive(Debug, Clone, FromRow)]
pub struct Identity {
    pub id: i64,
    pub identity_id: String, // 身份ID (Hex encoded ML-DSA PubKey)
    pub is_current: i32,
    pub created_at: i64,
}

// ========== 初始化 ==========

pub async fn init(cfg: &CoreConfig) -> anyhow::Result<()> {
    let db_path = cfg.data_dir.join("database.sqlite");
    init_path(&db_path).await
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
                .pragma("mmap_size", "268435456")
                .pragma("journal_size_limit", "67108864")
                .pragma("temp_store", "memory")
                .optimize_on_close(true, None)
                .busy_timeout(Duration::from_secs(5)),
        )
        .await?;

    sqlx::query("SELECT 1").execute(&pool).await?;

    if is_new {
        super::migrations::run(&pool).await?;
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

// ========== 身份管理（以 ML-DSA 公钥为唯一身份标识） ==========

/// 添加新身份（ML-DSA 公钥 hex 作为 identity_id）
pub async fn add_identity(pool: &Pool<Sqlite>, identity_id: &str) -> anyhow::Result<()> {
    sqlx::query(
        "INSERT INTO identities (identity_id, is_current) VALUES (?, 0) ON CONFLICT(identity_id) DO NOTHING",
    )
    .bind(identity_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// 获取当前身份的 identity_id（ML-DSA 公钥 hex）
pub async fn get_current_identity(pool: &Pool<Sqlite>) -> anyhow::Result<Option<String>> {
    let row = sqlx::query_scalar::<_, String>(
        "SELECT identity_id FROM identities WHERE is_current = 1 LIMIT 1",
    )
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

pub async fn set_current_identity(pool: &Pool<Sqlite>, identity_id: &str) -> anyhow::Result<()> {
    sqlx::query("UPDATE identities SET is_current = 0")
        .execute(pool)
        .await?;
    sqlx::query("UPDATE identities SET is_current = 1 WHERE identity_id = ?")
        .bind(identity_id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn list_identities(pool: &Pool<Sqlite>) -> anyhow::Result<Vec<Identity>> {
    sqlx::query_as::<_, Identity>(
        r#"SELECT id, identity_id, is_current, created_at FROM identities ORDER BY id"#,
    )
    .fetch_all(pool)
    .await
    .map_err(Into::into)
}

pub async fn delete_identity(
    pool: &Pool<Sqlite>,
    data_dir: &Path,
    identity_id: &str,
) -> anyhow::Result<u64> {
    // 1. 删除 Keyring 中的私钥
    rootcell::identity::PrivateKeyHandle::delete_from_keyring(&format!("{}_mldsa", identity_id))?;
    rootcell::identity::PrivateKeyHandle::delete_from_keyring(&format!("{}_mlkem", identity_id))?;

    // 2. 清理 DHT 中的记录（PUBKEY_PEERID_TABLE 和 PUBKEY_MLKEM_TABLE）
    let dht_path = data_dir.join("dht.redb");
    if let Ok(db) = redb::Database::create(&dht_path) {
        use std::sync::Arc;
        let store = crate::p2p::dht::RedbRecordStore::new(Arc::new(db));
        let _ = store.remove_pubkey_peerid(identity_id);
        let _ = store.remove_mlkem_pubkey(identity_id);
    }

    // 3. 删除数据库记录
    let rows = sqlx::query("DELETE FROM identities WHERE identity_id = ?")
        .bind(identity_id)
        .execute(pool)
        .await?
        .rows_affected();

    tracing::info!(
        "Deleted identity {}: removed keys from Keyring, DHT records, and DB ({} rows affected)",
        identity_id,
        rows
    );

    Ok(rows)
}
