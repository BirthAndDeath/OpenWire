/// 联系人管理模块
mod contact;
/// 身份管理模块
mod identity;
/// 消息管理模块
mod message;
/// 数据库迁移模块
mod migrations;
/// 已发送文件历史模块
mod sent_file;
/// 统计查询模块
mod stats;
//✅
use crate::CoreConfig;
pub use contact::{
    Contact, clear_all_mlkem_pubkeys, delete_contact, get_contact_by_mldsa_pubkey,
    get_contact_mlkem_pubkey, is_contact_exists, list_contacts, update_contact_mlkem_pubkey,
    upsert_contact,
};
pub use identity::{
    Identity, add_identity, delete_identity, get_current_identity, list_identities,
    set_current_identity,
};
pub use message::{
    Message, add_message, add_message_with_hash, add_messages_batch, delete_message,
    delete_messages_batch, delete_messages_by_peer, get_last_message, get_message,
    get_message_by_hash, get_messages, get_messages_range, list_failed, list_pending,
    list_pending_by_peer, mark_failed, mark_pending, mark_sent, mark_sent_batch, mark_sent_by_hash,
    update_message_hash,
};
pub use sent_file::{add_sent_file, get_sent_file};
use sqlx::{
    FromRow, Pool, Sqlite,
    sqlite::{SqliteConnectOptions, SqlitePoolOptions},
};
use std::{path::Path, time::Duration};
use tracing;
//✅
use crate::error::{StorageError, StorageResult};
/// 从 CoreConfig 初始化数据库连接池
pub async fn init(cfg: &CoreConfig) -> StorageResult<()> {
    let db_path = cfg.data_dir.join("database.sqlite");
    init_path(&db_path).await
}
static SQLITE_DB_POOL: std::sync::OnceLock<Pool<Sqlite>> = std::sync::OnceLock::new();
/// 从指定路径初始化数据库连接池
pub async fn init_path(path: &Path) -> StorageResult<()> {
    if path.is_dir() {
        return Err(StorageError::InvalidPath(
            "Database path must be a file".to_string(),
        ));
    }

    if SQLITE_DB_POOL.get().is_some() {
        tracing::info!("数据库连接池已存在，跳过重复初始化");
        return Ok(());
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

    sqlx::query("SELECT 1").execute(&pool).await?; //测试数据库连接

    if is_new {
        crate::storage::migrations::run(&pool).await?;
        tracing::info!("New database initialized");
    }

    SQLITE_DB_POOL
        .set(pool)
        .map_err(|_| StorageError::PoolAlreadyInitialized)?;
    Ok(())
}

/// 获取数据库连接池
pub fn pool() -> Option<&'static Pool<Sqlite>> {
    SQLITE_DB_POOL.get()
}
