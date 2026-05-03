use sqlx::{Pool, Sqlite};

/// 使用 sqlx::migrate! 宏嵌入迁移文件
/// 迁移文件位于 CARGO_MANIFEST_DIR/migrations/ 目录
static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

pub async fn run(pool: &Pool<Sqlite>) -> anyhow::Result<()> {
    MIGRATOR.run(pool).await?;
    Ok(())
}
