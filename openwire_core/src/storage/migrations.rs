use sqlx::{Pool, Sqlite};

use crate::error::{StorageError, StorageResult};
//✅
/// 使用 sqlx::migrate! 宏嵌入迁移文件
/// 迁移文件位于 CARGO_MANIFEST_DIR/migrations/ 目录
static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

/// 运行数据库迁移
pub async fn run(pool: &Pool<Sqlite>) -> StorageResult<()> {
    MIGRATOR
        .run(pool)
        .await
        .map_err(StorageError::MigrationFailed)?;
    Ok(())
}
