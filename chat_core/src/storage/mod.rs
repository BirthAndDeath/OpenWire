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
    // 1. 用户表（身份层）
    // 注意：这里存储的是"用户身份"而非设备，实际私钥在硬件/WebAuthn中
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS users (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            -- 用户可读的名称（可修改）
            display_name TEXT NOT NULL,
            -- 用户全局唯一标识（UUID v4，用于跨设备识别同一用户）
            user_uuid TEXT UNIQUE NOT NULL,
            -- 用户元数据（JSON：头像、状态等）
            metadata TEXT NOT NULL DEFAULT '{}',
            created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
            updated_at DATETIME DEFAULT CURRENT_TIMESTAMP
        )
        "#,
    )
    .execute(pool)
    .await?;

    // 2. 设备表（硬件层）
    // 每个设备 = 一个 WebAuthn 凭证 + 一个 X25519 密钥对 + 一个 libp2p PeerId
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS devices (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            -- 关联到用户（多设备支持）
            user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
            
            -- libp2p 身份（网络层）
            peer_id TEXT UNIQUE NOT NULL,
            
            -- X25519 公钥（32字节，BASE64编码，用于密钥交换）
            -- 注意：私钥每次启动重新生成，不存储
            x25519_public_key TEXT UNIQUE NOT NULL,
            
            -- WebAuthn 凭证 ID（硬件绑定，用于恢复身份）
            webauthn_credential_id BLOB UNIQUE NOT NULL,
            
            -- 设备类型（用于UI显示）
            device_type TEXT CHECK(device_type IN ('desktop', 'mobile', 'tablet', 'hardware_key')),
            
            -- 设备标签（用户可识别，如"MacBook Pro"）
            device_label TEXT NOT NULL,
            
            -- 是否当前活跃设备
            is_active BOOLEAN DEFAULT 1,
            
            -- 最后在线时间（用于设备管理）
            last_seen_at DATETIME,
            
            created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
            
            -- 索引：快速查找用户的所有设备
            INDEX idx_devices_user (user_id),
            -- 索引：通过 PeerId 查找设备
            INDEX idx_devices_peer (peer_id)
        )
        "#,
    )
    .execute(pool)
    .await?;

    // 3. WebAuthn 凭证表（安全层）
    // 分离存储，支持一个设备多个凭证（迁移/备份场景）
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS webauthn_credentials (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            -- 关联设备
            device_id INTEGER NOT NULL REFERENCES devices(id) ON DELETE CASCADE,
            
            -- 凭证元数据（JSON格式存储完整 PersistentCredential）
            credential_data BLOB NOT NULL,
            
            -- 凭证类型（usb_hid, platform, hybrid, soft_token）
            auth_type TEXT NOT NULL,
            
            -- 创建时间
            created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
            
            -- 是否有效（吊销用）
            is_valid BOOLEAN DEFAULT 1,
            
            INDEX idx_credentials_device (device_id)
        )
        "#,
    )
    .execute(pool)
    .await?;

    // 4. 联系人表（社交层）
    // 存储好友信息，一个联系人可以有多个设备
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS contacts (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            
            -- 本地用户ID（谁的好友）
            owner_user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
            
            -- 对方用户UUID（全局标识）
            contact_user_uuid TEXT NOT NULL,
            
            -- 本地设置的备注名
            local_name TEXT,
            
            -- 对方公钥（用于验证身份，从对方设备获取）
            -- 格式：{"devices": [{"peer_id": "...", "x25519_pubkey": "...", "webauthn_cred": "..."}]}
            identity_proof TEXT NOT NULL,
            
            -- 信任锚（首次验证方式：qr_code, manual, mutual_contact）
            trust_anchor TEXT NOT NULL,
            
            -- 验证状态（pending, verified, blocked）
            verification_status TEXT DEFAULT 'pending',
            
            created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
            updated_at DATETIME DEFAULT CURRENT_TIMESTAMP,
            
            -- 唯一约束：一个用户不能重复添加同一联系人
            UNIQUE(owner_user_id, contact_user_uuid),
            INDEX idx_contacts_owner (owner_user_id)
        )
        "#,
    )
    .execute(pool)
    .await?;

    // 5. 联系人设备表（动态更新）
    // 联系人的设备可能变化（增减设备），单独存储便于同步
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS contact_devices (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            -- 关联联系人
            contact_id INTEGER NOT NULL REFERENCES contacts(id) ON DELETE CASCADE,
            
            -- 对方设备PeerId
            peer_id TEXT NOT NULL,
            
            -- 对方X25519公钥（用于加密）
            x25519_public_key TEXT NOT NULL,
            
            -- 对方WebAuthn凭证ID（用于验证签名）
            webauthn_credential_id BLOB,
            
            -- 网络地址（Multiaddr，可能多个，JSON数组）
            multiaddrs TEXT NOT NULL DEFAULT '[]',
            
            -- 是否在线（心跳更新）
            is_online BOOLEAN DEFAULT 0,
            
            -- 最后确认时间
            last_seen_at DATETIME,
            
            created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
            
            UNIQUE(contact_id, peer_id),
            INDEX idx_contact_devices_contact (contact_id)
        )
        "#,
    )
    .execute(pool)
    .await?;

    // 6. 会话表（加密通道）
    // 存储与每个联系人设备的双棘轮会话状态
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS sessions (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            -- 本地设备（谁发起的会话）
            local_device_id INTEGER NOT NULL REFERENCES devices(id) ON DELETE CASCADE,
            -- 对方设备
            remote_device_id INTEGER NOT NULL REFERENCES contact_devices(id) ON DELETE CASCADE,
            
            -- 会话状态（JSON：链密钥、消息计数器等敏感数据）
            -- 注意：此字段应额外加密存储（使用设备主密钥）
            session_state BLOB NOT NULL,
            
            -- 会话建立时间
            established_at DATETIME DEFAULT CURRENT_TIMESTAMP,
            
            -- 最后使用时间
            last_used_at DATETIME,
            
            UNIQUE(local_device_id, remote_device_id),
            INDEX idx_sessions_local (local_device_id)
        )
        "#,
    )
    .execute(pool)
    .await?;

    // 7. 消息表（文件存的是hash索引链接，前端点击链接处理点对点下载）
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS messages (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            -- 所属会话
            session_id INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
            
            -- 消息唯一标识（用于去重和同步）
            message_uuid TEXT UNIQUE NOT NULL,
            
            -- 发送方设备（null表示自己发送）
            sender_device_id INTEGER REFERENCES devices(id),
            
            -- 密文（二进制，包含nonce和tag）
            ciphertext BLOB NOT NULL,
            
            -- 序列号（双棘轮算法使用）
            sequence_number INTEGER NOT NULL,
            
            -- 发送时间（对方声称的，需验证）
            sent_at DATETIME,
            -- 接收时间（本地记录）
            received_at DATETIME DEFAULT CURRENT_TIMESTAMP,
            
            -- 消息状态（sending, sent, delivered, read, failed）
            status TEXT DEFAULT 'sending',
            
            INDEX idx_messages_session (session_id, sequence_number)
        )
        "#,
    )
    .execute(pool)
    .await?;

    // 检查是否首次创建（通过sqlite_master查询）
    let is_fresh: bool = sqlx::query_scalar(
        "SELECT COUNT(*) = 0 FROM sqlite_master WHERE type='table' AND name='users'",
    )
    .fetch_one(pool)
    .await?;

    if is_fresh {
        tracing::info!("Database initialized successfully (fresh install)");
    } else {
        tracing::debug!("Database schema verified");
    }

    Ok(())
}
