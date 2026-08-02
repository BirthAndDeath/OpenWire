use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use libp2p::identity;
use serde::{Deserialize, Serialize};

/// PeerID 持久化配置：存储 Ed25519 密钥与端口偏好，8h~24h 随机 TTL。
///
/// 仅在启动时检查 TTL，运行期间不轮换。TTL 过期后重新生成全部配置。
const TTL_BASE_SECS: u64 = 28800;  // 8h
const TTL_JITTER_SECS: u64 = 57600; // +0~16h jitter，最终区间 8h~24h

/// Ed25519 密钥对 + 端口偏好，持久化到 `peerid.json`。
///
/// 实现设备级 PeerID 稳定：重启后复用存储的 Ed25519 密钥和端口偏好，
/// 端口被占用时静默回退到 OS 分配。TTL 到期后重新生成全部配置。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerIdConfig {
    ed25519_private_key: Vec<u8>,
    preferred_tcp_port: u16,
    preferred_quic_port: u16,
    preferred_ws_port: u16,
    created_at_unix: u64,
    ttl_secs: u64,
}

impl PeerIdConfig {
    fn create_new() -> Self {
        let ed25519_private_key = loop {
            let kp = identity::Keypair::generate_ed25519();
            match kp.to_protobuf_encoding() {
                Ok(encoded) => break encoded,
                Err(e) => tracing::warn!("Ed25519 protobuf encoding failed, retrying: {e}"),
            }
        };

        let ttl_secs = TTL_BASE_SECS + (rand::random::<u64>() % (TTL_JITTER_SECS + 1));
        let created_at_unix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        Self {
            ed25519_private_key,
            preferred_tcp_port: 0,
            preferred_quic_port: 0,
            preferred_ws_port: 0,
            created_at_unix,
            ttl_secs,
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

    pub(crate) fn path(data_dir: &Path) -> std::path::PathBuf {
        data_dir.join("peerid.json")
    }

    /// 加载持久化配置，不存在或超时时创建新的。
    /// 仅在启动时调用，运行期间不重新检查。
    pub fn load_or_create(data_dir: &Path) -> Self {
        let path = Self::path(data_dir);
        if let Ok(content) = std::fs::read_to_string(&path)
            && let Ok(config) = serde_json::from_str::<PeerIdConfig>(&content) {
                if !config.is_expired() {
                    tracing::info!(
                        "PeerIdConfig loaded from {}, TTL remaining={}s",
                        path.display(),
                        config.ttl_secs.saturating_sub(
                            SystemTime::now()
                                .duration_since(UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_secs()
                                .saturating_sub(config.created_at_unix)
                        )
                    );
                    return config;
                }
                tracing::info!("PeerIdConfig at {} expired, rotating", path.display());
            }
        let config = Self::create_new();
        if let Err(e) = config.save(data_dir) {
            tracing::warn!("Failed to save PeerIdConfig: {e}");
        }
        tracing::info!("Created new PeerIdConfig, TTL={}s (~{}h)", config.ttl_secs, config.ttl_secs / 3600);
        config
    }

    fn save(&self, data_dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
        let content = serde_json::to_string_pretty(self)?;
        let path = Self::path(data_dir);
        // Windows 上先移除 readonly，否则 std::fs::write 无法覆写文件
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