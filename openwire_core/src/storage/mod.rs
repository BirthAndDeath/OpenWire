/// 联系人管理模块
mod contact;
/// 身份管理模块
mod identity;
/// 消息管理模块
mod message;
/// 数据库迁移模块
mod migrations;
/// 扫码暂存模块（好友摘要化流程）
mod pending_contact;
/// 已发送文件历史模块
mod sent_file;
/// 统计查询模块
mod stats;
//✅
use crate::CoreConfig;
pub use contact::{
    Contact, clear_all_mlkem_pubkeys, contact_display_name, delete_contact,
    get_contact_by_mldsa_pubkey, get_contact_mlkem_pubkey, is_contact_blocked,
    is_contact_exists, list_contacts, update_contact_meta, update_contact_mlkem_pubkey,
    upsert_contact,
};
pub use identity::{
    Identity, add_identity, current_identity_id, delete_identity, get_current_identity,
    list_identities, set_current_identity,
};
pub use message::{
    ContactMessagesStat, Message, MessageSearchHit, MAX_MESSAGE_PAGE_SIZE,
    add_message_with_hash, contact_messages_stat, delete_message, delete_messages_batch,
    delete_messages_by_peer, get_last_message, get_message, get_message_by_hash, get_messages,
    get_messages_around, get_messages_range, list_failed, list_pending, list_pending_by_peer,
    mark_failed, mark_pending,
    mark_sent, mark_sent_batch, mark_sent_by_hash, search_messages_by_content,
    update_message_hash,
};
pub use pending_contact::{
    PendingContact, add_pending_contact, delete_pending_contact, get_pending_contact_by_digest,
    list_pending_contacts,
};
pub use sent_file::{
    SentFile, SENT_FILE_RETENTION_DAYS, add_sent_file, delete_sent_file,
    delete_sent_files_by_recipient, delete_sent_files_older_than, get_sent_file,
    list_all_sent_files, list_sent_files_by_recipient, sum_sent_bytes_by_recipient,
    verify_sent_file,
};
use sqlx::{
    Pool, Sqlite,
    sqlite::{SqliteConnectOptions, SqlitePoolOptions},
};
use std::{path::Path, path::PathBuf, time::Duration};
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

    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p)?;
    }

    let mut is_new = !path.exists();
    let mut pool = connect_db(path).await?;

    if !is_new {
        // 旧数据库与当前迁移基线不兼容（迁移文件已合并/修改，校验和不匹配）→ 视为不存在，直接删除重建
        if let Err(e) = crate::storage::migrations::run(&pool).await {
            tracing::warn!("旧数据库与当前迁移基线不兼容，删除并重建: {e}");
            pool.close().await;
            drop(pool);
            remove_db_files(path)?;
            pool = connect_db(path).await?;
            is_new = true;
        }
    }

    crate::storage::migrations::run(&pool).await?;

    if is_new {
        tracing::info!("New database initialized");
    } else {
        tracing::info!("Existing database migrated");
    }

    SQLITE_DB_POOL
        .set(pool)
        .map_err(|_| StorageError::PoolAlreadyInitialized)?;
    Ok(())
}

/// 建立数据库连接池并验证连接可用
async fn connect_db(path: &Path) -> StorageResult<Pool<Sqlite>> {
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
    Ok(pool)
}

/// 删除旧数据库文件（连同 WAL/SHM）
fn remove_db_files(path: &Path) -> StorageResult<()> {
    for suffix in ["", "-wal", "-shm"] {
        let mut os = path.as_os_str().to_owned();
        os.push(suffix);
        let f = PathBuf::from(os);
        if f.exists() {
            std::fs::remove_file(&f)?;
        }
    }
    Ok(())
}

/// 获取数据库连接池
pub fn pool() -> Option<&'static Pool<Sqlite>> {
    SQLITE_DB_POOL.get()
}
