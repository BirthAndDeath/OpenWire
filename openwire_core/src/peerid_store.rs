use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use libp2p::identity;
use rootcell::store::EncryptedStore;
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

/// PeerID 持久化配置：存储 Ed25519 密钥与端口偏好，8h~24h 随机 TTL。
///
/// 每次启动时检查 TTL，TTL 过期或 10% 概率触发主动轮换时重新生成。
/// 文件通过 `rootcell::store::EncryptedStore` 加密存储（AES-256-GCM），
/// master key 由系统原生 keyring 保护。
const TTL_BASE_SECS: u64 = 28800;   // 8h
const TTL_JITTER_SECS: u64 = 57600; // +0~16h jitter，最终区间 8h~24h
const ROTATION_PROBABILITY: f64 = 0.1; // 每次启动 10% 概率主动轮换

/// EncryptedStore 中使用的 identifier
pub(crate) const STORE_IDENTIFIER: &str = "peerid";

/// Ed25519 密钥对 + 端口偏好，通过 `EncryptedStore` 加密持久化。
///
/// 实现设备级 PeerID 稳定：重启后复用存储的 Ed25519 密钥和端口偏好，
/// 端口被占用时静默回退到 OS 分配。TTL 到期后重新生成全部配置。
///
/// `was_rotated` 标记仅在 `load_or_create` 返回后有效，表示本次启动
/// 是否因为 TTL 到期或 10% 概率触发而轮换了 PeerID。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerIdConfig {
    ed25519_private_key: Zeroizing<Vec<u8>>,
    preferred_tcp_port: u16,
    preferred_quic_port: u16,
    preferred_ws_port: u16,
    created_at_unix: u64,
    ttl_secs: u64,
    #[serde(skip)]
    was_rotated: bool,
}

impl PeerIdConfig {
    fn create_new(prev: Option<&PeerIdConfig>) -> Self {
        let ed25519_private_key = Zeroizing::new(loop {
            let kp = identity::Keypair::generate_ed25519();
            match kp.to_protobuf_encoding() {
                Ok(encoded) => break encoded,
                Err(e) => tracing::warn!("Ed25519 protobuf encoding failed, retrying: {e}"),
            }
        });

        let ttl_secs = TTL_BASE_SECS + (rand::random::<u64>() % (TTL_JITTER_SECS + 1));
        let created_at_unix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        // 轮换时继承旧端口并自增，避免多个轮换周期竞争同一端口
        let (preferred_tcp_port, preferred_quic_port, preferred_ws_port) = match prev {
            Some(p) => (
                next_port(p.preferred_tcp_port),
                next_port(p.preferred_quic_port),
                next_port(p.preferred_ws_port),
            ),
            None => (0, 0, 0),
        };

        Self {
            ed25519_private_key,
            preferred_tcp_port,
            preferred_quic_port,
            preferred_ws_port,
            created_at_unix,
            ttl_secs,
            was_rotated: true,
        }
    }

    fn is_expired(&self) -> bool {
        let elapsed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
            .saturating_sub(self.created_at_unix);
        elapsed >= self.ttl_secs
    }

    /// 仅用于删除损坏的旧版明文文件（`peerid.json`）。
    /// 新加密存储使用 `EncryptedStore` 的哈希路径。
    pub(crate) fn path(data_dir: &Path) -> std::path::PathBuf {
        data_dir.join("peerid.json")
    }

    /// 从 `EncryptedStore` 加载持久化配置，不存在或超时时创建新的。
    ///
    /// 每次启动时检查 TTL，TTL 未到期时以 10% 概率主动轮换。
    /// 返回的配置中 `was_rotated()` 表示 PeerID 是否发生了变化。
    pub fn load_or_create(data_dir: &Path) -> Self {
        let data_dir_str = data_dir.to_string_lossy();

        // 尝试从 EncryptedStore 加载
        if let Ok(store) = EncryptedStore::init() {
            match store.load::<PeerIdConfig>(&data_dir_str, STORE_IDENTIFIER) {
                Ok(Some(config)) => {
                    return rotate_if_needed(
                        config,
                        &data_dir_str,
                        |cfg| {
                            if let Err(e) = store.save(&data_dir_str, STORE_IDENTIFIER, cfg) {
                                tracing::warn!("Failed to save new PeerIdConfig: {e}");
                            }
                        },
                        |cfg| tracing::info!(
                            "PeerIdConfig loaded from EncryptedStore, TTL remaining={}s",
                            cfg.ttl_secs.saturating_sub(
                                SystemTime::now()
                                    .duration_since(UNIX_EPOCH)
                                    .unwrap_or_default()
                                    .as_secs()
                                    .saturating_sub(cfg.created_at_unix)
                            )
                        ),
                    );
                }
                Ok(None) => {
                    // 加密存储无数据：若存在旧明文文件则迁移到加密存储，避免 split-brain
                    let plaintext_path = Self::path(data_dir);
                    if plaintext_path.exists() {
                        if let Ok(content) = std::fs::read_to_string(&plaintext_path)
                            && let Ok(mut pconfig) =
                                serde_json::from_str::<PeerIdConfig>(&content)
                        {
                            tracing::info!(
                                "Migrating existing plaintext peerid.json into EncryptedStore"
                            );
                            let _ = store.save(&data_dir_str, STORE_IDENTIFIER, &pconfig);
                            pconfig.was_rotated = false;
                            let _ = std::fs::remove_file(&plaintext_path);
                            return pconfig;
                        }
                        tracing::warn!(
                            "Ignoring unreadable plaintext peerid.json, creating new encrypted config"
                        );
                    }
                    let config = Self::create_new(None);
                    if let Err(e) = store.save(&data_dir_str, STORE_IDENTIFIER, &config) {
                        tracing::warn!("Failed to save new PeerIdConfig: {e}");
                    }
                    tracing::info!(
                        "Created new PeerIdConfig via EncryptedStore, TTL={}s (~{}h)",
                        config.ttl_secs,
                        config.ttl_secs / 3600
                    );
                    return config;
                }
                Err(e) => {
                    tracing::warn!("Failed to load PeerIdConfig from EncryptedStore: {e}");
                }
            }
        }

        // 回退到明文文件（无 keyring 环境）
        tracing::warn!("Keyring unavailable, falling back to plaintext peerid.json (Ed25519 private key unencrypted)");
        self::fallback_plaintext_load_or_create(data_dir)
    }

    fn save(&self, data_dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
        let data_dir_str = data_dir.to_string_lossy();
        if let Ok(store) = EncryptedStore::init() {
            store.save(&data_dir_str, STORE_IDENTIFIER, self)?;
            return Ok(());
        }
        tracing::warn!("Keyring unavailable, falling back to plaintext peerid.json (Ed25519 private key unencrypted)");
        // 回退到明文保存
        self.fallback_plaintext_save(data_dir)
    }

    /// 明文保存（无 keyring 环境回退）
    fn fallback_plaintext_save(&self, data_dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
        let content = serde_json::to_string_pretty(self)?;
        let path = Self::path(data_dir);
        #[cfg(windows)]
        {
            if path.exists() {
                let mut perms = std::fs::metadata(&path)?.permissions();
                perms.set_readonly(false);
                std::fs::set_permissions(&path, perms)?;
            }
        }
        std::fs::write(&path, content)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        }
        #[cfg(windows)]
        {
            let mut perms = std::fs::metadata(&path)?.permissions();
            perms.set_readonly(true);
            std::fs::set_permissions(&path, perms)?;
        }
        Ok(())
    }

    /// 返回此配置的 TTL 秒数
    pub fn ttl_secs(&self) -> u64 {
        self.ttl_secs
    }

    /// 本次启动是否轮换了 PeerID（TTL 到期或 10% 概率触发）
    pub fn was_rotated(&self) -> bool {
        self.was_rotated
    }

    /// 从存储的 Ed25519 私钥恢复 libp2p Keypair
    pub fn to_keypair(&self) -> Result<identity::Keypair, Box<dyn std::error::Error>> {
        Ok(identity::Keypair::from_protobuf_encoding(&self.ed25519_private_key)?)
    }

    /// 偏好的 TCP 监听端口（0 表示未记录）
    pub fn preferred_tcp_port(&self) -> u16 {
        self.preferred_tcp_port
    }

    /// 偏好的 QUIC 监听端口（0 表示未记录）
    pub fn preferred_quic_port(&self) -> u16 {
        self.preferred_quic_port
    }

    /// 偏好的 WebSocket 监听端口（0 表示未记录）
    pub fn preferred_ws_port(&self) -> u16 {
        self.preferred_ws_port
    }

    /// 更新端口偏好（首次启动后记录 OS 分配的端口）
    pub fn update_ports(&mut self, tcp: u16, quic: u16, ws: u16, data_dir: &Path) {
        if self.preferred_tcp_port == tcp
            && self.preferred_quic_port == quic
            && self.preferred_ws_port == ws
        {
            return;
        }
        self.preferred_tcp_port = tcp;
        self.preferred_quic_port = quic;
        self.preferred_ws_port = ws;
        if let Err(e) = self.save(data_dir) {
            tracing::warn!("Failed to update preferred ports: {e}");
        }
    }
}

/// 明文文件回退加载（无 keyring 环境）
fn fallback_plaintext_load_or_create(data_dir: &Path) -> PeerIdConfig {
    let path = PeerIdConfig::path(data_dir);
    if let Ok(content) = std::fs::read_to_string(&path)
        && let Ok(config) = serde_json::from_str::<PeerIdConfig>(&content) {
            return rotate_if_needed(
                config,
                &data_dir.to_string_lossy(),
                |cfg| {
                    if let Err(e) = cfg.fallback_plaintext_save(data_dir) {
                        tracing::warn!("Failed to save new PeerIdConfig: {e}");
                    }
                },
                |cfg| tracing::info!(
                    "PeerIdConfig loaded from {} (plaintext fallback), TTL remaining={}s",
                    path.display(),
                    cfg.ttl_secs.saturating_sub(
                        SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_secs()
                            .saturating_sub(cfg.created_at_unix)
                    )
                ),
            );
        }
    let config = PeerIdConfig::create_new(None);
    if let Err(e) = config.fallback_plaintext_save(data_dir) {
        tracing::warn!("Failed to save PeerIdConfig: {e}");
    }
    tracing::info!(
        "Created new PeerIdConfig (plaintext fallback), TTL={}s (~{}h)",
        config.ttl_secs,
        config.ttl_secs / 3600
    );
    config
}

/// 通用 TTL 检查和轮换逻辑：检查过期或 10% 主动轮换，通过 save_fn 持久化。
fn rotate_if_needed(
    mut config: PeerIdConfig,
    log_label: &str,
    save_fn: impl FnOnce(&PeerIdConfig),
    log_fn: impl FnOnce(&PeerIdConfig),
) -> PeerIdConfig {
    if !config.is_expired() {
        if rand::random::<f64>() < ROTATION_PROBABILITY {
            tracing::info!("PeerIdConfig 10% probability rotation triggered via {}", log_label);
            let new_config = PeerIdConfig::create_new(Some(&config));
            save_fn(&new_config);
            return new_config;
        }
        config.was_rotated = false;
        log_fn(&config);
        return config;
    }
    tracing::info!("PeerIdConfig expired, rotating via {}", log_label);
    let new_config = PeerIdConfig::create_new(Some(&config));
    save_fn(&new_config);
    new_config
}

/// 轮换时端口自增辅助：继承旧端口并 +1，避免多个轮换周期竞争同一端口。
/// 若端口未记录（0），保持 0 让 OS 分配；超出范围时回退到 1024。
fn next_port(port: u16) -> u16 {
    if port == 0 {
        return 0;
    }
    let next = port as u32 + 1;
    if next > 65535 {
        1024
    } else {
        next as u16
    }
}