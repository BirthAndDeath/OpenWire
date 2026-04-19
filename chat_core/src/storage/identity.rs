use crate::CoreConfig;
use hex;
use keyring;
use sqlx::{
    FromRow, Pool, Row, Sqlite,
    sqlite::{SqliteConnectOptions, SqlitePoolOptions},
};
use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};
use tracing;

static DB_POOL: std::sync::OnceLock<Pool<Sqlite>> = std::sync::OnceLock::new();

#[derive(Debug, Clone, FromRow)]
pub struct Identity {
    pub id: i64,
    pub peer_id: String,
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

// ========== 身份管理 ==========

pub async fn add_identity(
    pool: &Pool<Sqlite>,
    peer_id: &str,
    public_key: &[u8],
) -> anyhow::Result<i64> {
    let id = sqlx::query_scalar(
        r#"INSERT INTO identity (peer_id, public_key) VALUES (?, ?) RETURNING id"#,
    )
    .bind(peer_id)
    .bind(public_key)
    .fetch_one(pool)
    .await?;
    Ok(id)
}

pub async fn get_current_identity(
    pool: &Pool<Sqlite>,
) -> anyhow::Result<Option<(String, Vec<u8>)>> {
    let row = sqlx::query("SELECT peer_id, public_key FROM identity WHERE is_current = 1 LIMIT 1")
        .fetch_optional(pool)
        .await?;
    Ok(row.map(|r| (r.get(0), r.get(1))))
}

pub async fn set_current_identity(pool: &Pool<Sqlite>, peer_id: &str) -> anyhow::Result<()> {
    // 先清除所有current
    sqlx::query("UPDATE identity SET is_current = 0")
        .execute(pool)
        .await?;
    // 设置新的current
    sqlx::query("UPDATE identity SET is_current = 1 WHERE peer_id = ?")
        .bind(peer_id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn list_identities(pool: &Pool<Sqlite>) -> anyhow::Result<Vec<Identity>> {
    sqlx::query_as::<_, Identity>(r#"SELECT id, peer_id, is_current FROM identity ORDER BY id"#)
        .fetch_all(pool)
        .await
        .map_err(Into::into)
}

const KEYRING_SERVICE: &str = "rootcell";

fn private_key_file_path(data_dir: &Path, peer_id: &str) -> PathBuf {
    data_dir.join("private_keys").join(peer_id)
}

pub async fn delete_identity(
    pool: &Pool<Sqlite>,
    data_dir: &Path,
    peer_id: &str,
) -> anyhow::Result<u64> {
    let current_identity = get_current_identity(pool).await?;
    let rows = sqlx::query("DELETE FROM identity WHERE peer_id = ?")
        .bind(peer_id)
        .execute(pool)
        .await?
        .rows_affected();
    if rows > 0 {
        if let Err(e) = remove_private_key(data_dir, peer_id) {
            tracing::warn!("Failed to remove private key for {}: {e}", peer_id);
        }
        if let Some((existing_peer_id, _)) = current_identity {
            if existing_peer_id == peer_id {
                if let Some(new_peer_id) = sqlx::query_scalar::<_, String>(
                    "SELECT peer_id FROM identity ORDER BY id LIMIT 1",
                )
                .fetch_optional(pool)
                .await?
                {
                    set_current_identity(pool, &new_peer_id).await?;
                }
            }
        }
    }
    Ok(rows)
}

pub fn set_private_key(data_dir: &Path, peer_id: &str, private_key: &[u8]) -> anyhow::Result<bool> {
    let private_key_hex = hex::encode(private_key);
    tracing::info!("Attempting to save private key for {} ({} bytes hex-encoded)", peer_id, private_key_hex.len());

    // 尝试使用 keyring
    match keyring::Entry::new(KEYRING_SERVICE, peer_id) {
        Ok(entry) => {
            match entry.set_password(&private_key_hex) {
                Ok(_) => {
                    tracing::info!("✅ Successfully saved private key to keyring for {}", peer_id);
                    return Ok(false);
                }
                Err(e) => {
                    tracing::warn!(
                        "⚠️  Failed to save private key to keyring: {}, falling back to local file storage",
                        e
                    );
                }
            }
        }
        Err(e) => {
            tracing::warn!(
                "⚠️  Failed to create keyring entry: {}, falling back to local file storage",
                e
            );
        }
    }
    
    // 降级到本地文件存储
    tracing::info!("📁 Using local file storage for private key");
    let path = private_key_file_path(data_dir, peer_id);
    tracing::debug!("Saving private key to local file: {:?}", path);
    
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
        tracing::debug!("Created directory: {:?}", parent);
    }
    
    fs::write(&path, &private_key_hex)?;
    tracing::info!("✅ Successfully saved private key to local file for {} at {:?}", peer_id, path);
    
    // 验证写入
    if path.exists() {
        let verify_content = fs::read_to_string(&path)?;
        if verify_content.trim() == private_key_hex {
            tracing::debug!("Verified private key file content matches");
        } else {
            tracing::error!("Private key file verification failed!");
        }
    }
    
    Ok(true)
}

pub fn get_private_key(data_dir: &Path, peer_id: &str) -> anyhow::Result<Vec<u8>> {
    tracing::debug!("Attempting to load private key for {}", peer_id);
    
    // 尝试从 keyring 加载
    match keyring::Entry::new(KEYRING_SERVICE, peer_id) {
        Ok(entry) => {
            match entry.get_password() {
                Ok(password) if !password.trim().is_empty() => {
                    tracing::info!("✅ Successfully loaded private key from keyring for {}", peer_id);
                    let decoded = hex::decode(password.trim())?;
                    tracing::debug!("Decoded private key: {} bytes", decoded.len());
                    return Ok(decoded);
                }
                Ok(_) => {
                    tracing::warn!(
                        "⚠️  Keyring returned empty password for {}, this may indicate a corrupted keyring entry. Trying local file",
                        peer_id
                    );
                }
                Err(e) => {
                    tracing::debug!(
                        "Failed to read from keyring for {}: {}, trying local file",
                        peer_id,
                        e
                    );
                }
            }
        }
        Err(e) => {
            tracing::debug!(
                "⚠️  Failed to create keyring entry for {}: {}, trying local file",
                peer_id,
                e
            );
        }
    }
    
    // 降级到本地文件
    tracing::debug!("📁 Attempting to load from local file storage");
    let path = private_key_file_path(data_dir, peer_id);
    tracing::debug!("Looking for private key file at: {:?}", path);
    
    if path.exists() {
        let stored = fs::read_to_string(&path)?;
        tracing::debug!("Found private key file with {} bytes", stored.len());
        if stored.trim().is_empty() {
            anyhow::bail!("Stored private key file is empty at {:?}", path);
        }
        tracing::info!("✅ Successfully loaded private key from local file for {}", peer_id);
        let decoded = hex::decode(stored.trim())?;
        tracing::debug!("Decoded private key: {} bytes", decoded.len());
        Ok(decoded)
    } else {
        tracing::error!("❌ Local private key file does not exist for {} at {:?}", peer_id, path);
        Err(anyhow::anyhow!(
            "Private key not found in keyring or local storage for {}",
            peer_id
        ))
    }
}

pub fn remove_private_key(data_dir: &Path, peer_id: &str) -> anyhow::Result<()> {
    let entry = keyring::Entry::new(KEYRING_SERVICE, peer_id)?;
    // 尝试彻底删除 keyring 中的凭证，而不是设置为空字符串
    if let Err(e) = entry.delete_credential() {
        tracing::warn!("Failed to delete private key from keyring: {e}");
    }
    let path = private_key_file_path(data_dir, peer_id);
    if path.exists() {
        fs::remove_file(path)?;
    }
    Ok(())
}

/// 诊断私钥存储状态
pub fn diagnose_private_key_storage(data_dir: &Path, peer_id: &str) -> String {
    let mut report = format!("=== Private Key Storage Diagnosis for {} ===\n", peer_id);
    
    // 检查 keyring
    let entry = match keyring::Entry::new(KEYRING_SERVICE, peer_id) {
        Ok(e) => e,
        Err(e) => {
            report.push_str(&format!("❌ Failed to create keyring entry: {}\n", e));
            report.push_str(&format!("📁 Checking local file storage...\n"));
            return report;
        }
    };
    
    match entry.get_password() {
        Ok(password) if !password.trim().is_empty() => {
            report.push_str(&format!("✅ Keyring: Available ({} bytes hex-encoded)\n", password.len()));
            match hex::decode(password.trim()) {
                Ok(decoded) => report.push_str(&format!("   Decoded size: {} bytes\n", decoded.len())),
                Err(e) => report.push_str(&format!("   ⚠️  Failed to decode: {}\n", e)),
            }
        }
        Ok(_) => {
            report.push_str("⚠️  Keyring: Entry exists but empty (corrupted?)\n");
        }
        Err(e) => {
            report.push_str(&format!("❌ Keyring: Not available ({})\n", e));
        }
    }
    
    // 检查本地文件
    let path = private_key_file_path(data_dir, peer_id);
    report.push_str(&format!("\n📁 Local file check:\n"));
    report.push_str(&format!("   Path: {:?}\n", path));
    
    if path.exists() {
        match fs::read_to_string(&path) {
            Ok(content) if !content.trim().is_empty() => {
                report.push_str(&format!("✅ File: Available ({} bytes hex-encoded)\n", content.len()));
                match hex::decode(content.trim()) {
                    Ok(decoded) => report.push_str(&format!("   Decoded size: {} bytes\n", decoded.len())),
                    Err(e) => report.push_str(&format!("   ⚠️  Failed to decode: {}\n", e)),
                }
            }
            Ok(_) => {
                report.push_str("⚠️  File: Exists but empty\n");
            }
            Err(e) => {
                report.push_str(&format!("❌ File: Cannot read ({})\n", e));
            }
        }
    } else {
        report.push_str("❌ File: Does not exist\n");
    }
    
    report.push_str("\n💡 Recommendation: ");
    if path.exists() {
        report.push_str("Local file backup is available. If keyring fails, the app will use the file.\n");
    } else {
        report.push_str("Consider regenerating identity if both storage methods fail.\n");
    }
    
    report
}
