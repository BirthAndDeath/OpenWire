use crate::CoreConfig;
use sqlx::{
    FromRow, Pool, Row, Sqlite,
    sqlite::{SqliteConnectOptions, SqlitePoolOptions},
};
use std::{path::Path, time::Duration};
use tracing;

static DB_POOL: std::sync::OnceLock<Pool<Sqlite>> = std::sync::OnceLock::new();

#[derive(Debug, Clone, FromRow)]
pub struct MlKemIdentity {
    pub id: i64,
    pub identity_id: String, // ML-KEM公钥的hex编码
    pub public_key: Vec<u8>, // 原始公钥字节
    pub is_current: i32,
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

// ========== 身份管理（数据库层） ==========

pub async fn add_mlkem_identity(
    pool: &Pool<Sqlite>,
    identity_id: &str,
    public_key: &[u8],
) -> anyhow::Result<()> {
    sqlx::query(
        "INSERT INTO mlkem_identity (identity_id, public_key, is_current) VALUES (?, ?, 0)",
    )
    .bind(identity_id)
    .bind(public_key)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn get_current_mlkem_public_key(
    pool: &Pool<Sqlite>,
) -> anyhow::Result<Option<(String, Vec<u8>)>> {
    let row = sqlx::query(
        "SELECT identity_id, public_key FROM mlkem_identity WHERE is_current = 1 LIMIT 1",
    )
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|r| {
        let identity_id: String = r.get("identity_id");
        let public_key: Vec<u8> = r.get("public_key");
        (identity_id, public_key)
    }))
}

/// 获取当前 ML-KEM 身份信息
pub async fn get_current_mlkem_identity(
    pool: &Pool<Sqlite>,
) -> anyhow::Result<Option<(String, Vec<u8>)>> {
    get_current_mlkem_public_key(pool).await
}

pub async fn set_current_mlkem_identity(
    pool: &Pool<Sqlite>,
    identity_id: &str,
) -> anyhow::Result<()> {
    sqlx::query("UPDATE mlkem_identity SET is_current = 0")
        .execute(pool)
        .await?;
    sqlx::query("UPDATE mlkem_identity SET is_current = 1 WHERE identity_id = ?")
        .bind(identity_id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn list_mlkem_identities(pool: &Pool<Sqlite>) -> anyhow::Result<Vec<MlKemIdentity>> {
    sqlx::query_as::<_, MlKemIdentity>(
        r#"SELECT id, identity_id, public_key, is_current FROM mlkem_identity ORDER BY id"#,
    )
    .fetch_all(pool)
    .await
    .map_err(Into::into)
}

pub async fn delete_mlkem_identity(
    pool: &Pool<Sqlite>,
    data_dir: &Path,
    identity_id: &str,
) -> anyhow::Result<u64> {
    // 先删除私钥（使用rootcell的PrivateKeyHandle）
    rootcell::identity::PrivateKeyHandle::delete_from_keyring(identity_id);
    let _ = rootcell::identity::PrivateKeyHandle::delete_encrypted_file(data_dir, identity_id);

    // 然后删除数据库记录
    let rows = sqlx::query("DELETE FROM mlkem_identity WHERE identity_id = ?")
        .bind(identity_id)
        .execute(pool)
        .await?
        .rows_affected();
    Ok(rows)
}
