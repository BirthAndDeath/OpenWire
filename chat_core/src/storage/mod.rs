use sqlx::{
    Pool, Sqlite,
    sqlite::{SqliteConnectOptions, SqlitePoolOptions},
};
use std::time::Duration;

use crate::CoreConfig;

// 全局连接池实例
static DB_POOL: std::sync::OnceLock<Pool<Sqlite>> = std::sync::OnceLock::new();

pub async fn init(cfg: &CoreConfig) -> anyhow::Result<()> {
    let pool_options = SqlitePoolOptions::new()
        .max_connections(10) // 连接池最大连接数 (默认取决于特性)
        .min_connections(0) // 连接池最小（保持）连接数 (默认 0)
        .max_lifetime(Some(Duration::from_secs(30 * 60))) // 连接最大存活时间（默认 30 分钟）
        .idle_timeout(Some(Duration::from_secs(10 * 60))) // 空闲连接超时时间（默认 10 分钟）
        .acquire_timeout(Duration::from_secs(30)); // 获取连接的超时时间（默认 30 秒）

    let connect_options = SqliteConnectOptions::new()
        .filename(cfg.database_path.clone())
        .create_if_missing(true) // 如果数据库文件不存在，则创建（默认 false）
        .read_only(false) // 是否以只读模式打开（默认 false）
        .foreign_keys(true) // 启用或禁用外键约束（默认由 SQLite 编译期决定）
        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal) // 设置日志模式为 WAL
        .synchronous(sqlx::sqlite::SqliteSynchronous::Normal) // 设置同步模式
        .busy_timeout(Duration::from_secs(5)) // 设置繁忙超时时间
        .pragma("temp_store", "memory") // 设置 PRAGMA 参数
        .pragma("cache_size", "-10000"); // 设置缓存大小（约 10MB）

    // 异步建立连接并等待连接池准备就绪
    let pool = pool_options
        .connect_with(connect_options)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to initialize database connection pool: {}", e))?;

    // 测试连接是否正常工作
    sqlx::query("SELECT 1")
        .execute(&pool)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to test database connection: {}", e))?;

    // 存储连接池到全局静态变量
    DB_POOL
        .set(pool)
        .map_err(|_| anyhow::anyhow!("Database pool already initialized"))?;

    tracing::info!("Database connection pool initialized successfully");
    //create_table_once(get_pool().unwrap()).await;
    Ok(())
}

/// 获取数据库连接池
pub fn get_pool() -> Option<&'static Pool<Sqlite>> {
    DB_POOL.get()
}

async fn create_table_once(pool: &Pool<Sqlite>) -> Result<(), sqlx::Error> {
    // 程序启动时执行一次建表语句
    // 创建 users 表
    // 用户表 代表用户自己管理的身份/账号
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS users(
            accountid SERIAL PRIMARY KEY,
            metadata TEXT NOT NULL,
            created_at TIMESTAMP DEFAULT NOW()
        )
        "#,
    )
    .execute(pool)
    .await?;
    //设备表
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS devices(
            accountid SERIAL PRIMARY KEY,
            peerid SERIAL PRIMARY KEY,
            created_at TIMESTAMP DEFAULT NOW()
        )
        "#,
    )
    .execute(pool)
    .await?;
    //联系人表
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS contacts(
            peerid         SERIAL PRIMARY KEY,
            multiaddr       TEXT NOT NULL,
            metadata       TEXT NOT NULL,
            created_at TIMESTAMP DEFAULT NOW()
        )
        "#,
    )
    .execute(pool)
    .await?;

    tracing::info!("Table created successfully(first time)");
    Ok(())
}
