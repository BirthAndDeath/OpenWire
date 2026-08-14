use std::path::Path;

use libp2p::identity;
use rootcell::store::EncryptedStore;
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

/// PeerID 持久化配置：存储 Ed25519 密钥与端口偏好，永久有效。
///
/// 文件通过 `rootcell::store::EncryptedStore` 加密存储（AES-256-GCM），
/// master key 由系统原生 keyring 保护。
/// EncryptedStore 中使用的 identifier
pub(crate) const STORE_IDENTIFIER: &str = "peerid";

/// 删除加密存储中损坏的 PeerID 条目。keyring 不可用时静默跳过。
pub(crate) fn delete_corrupted_entry(data_dir: &Path) {
    let data_dir_str = data_dir.to_string_lossy();
    if let Ok(store) = EncryptedStore::open() {
        store.delete(&data_dir_str, STORE_IDENTIFIER);
    }
}

/// Ed25519 密钥对 + 端口偏好，通过 `EncryptedStore` 加密持久化。
///
/// 实现设备级 PeerID 稳定：重启后复用存储的 Ed25519 密钥和端口偏好，
/// 端口被占用时静默回退到 OS 分配。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerIdConfig {
    ed25519_private_key: Zeroizing<Vec<u8>>,
    preferred_tcp_port: u16,
    preferred_quic_port: u16,
    preferred_ws_port: u16,
}

impl PeerIdConfig {
    fn create_new() -> Self {
        let ed25519_private_key = Zeroizing::new(loop {
            let kp = identity::Keypair::generate_ed25519();
            match kp.to_protobuf_encoding() {
                Ok(encoded) => break encoded,
                Err(e) => tracing::warn!("Ed25519 protobuf encoding failed, retrying: {e}"),
            }
        });

        Self {
            ed25519_private_key,
            preferred_tcp_port: 0,
            preferred_quic_port: 0,
            preferred_ws_port: 0,
        }
    }

    /// 从 `EncryptedStore` 加载持久化配置，不存在时创建新的。
    /// keyring 不可用时返回错误。加密数据损坏时自动删除并重建。
    pub fn load_or_create(data_dir: &Path) -> Result<Self, String> {
        let data_dir_str = data_dir.to_string_lossy();

        // 读路径：先尝试打开已有 master key。
        // 不存在时 init 创建新 master key（首次启动）。
        let store = match EncryptedStore::open() {
            Ok(store) => store,
            Err(_) => EncryptedStore::init()
                .map_err(|e| format!("Keyring 不可用，无法加密存储 PeerID: {e}"))?,
        };

        match store.load::<PeerIdConfig>(&data_dir_str, STORE_IDENTIFIER) {
            Ok(Some(config)) => {
                tracing::info!("PeerIdConfig loaded from EncryptedStore");
                Ok(config)
            }
            Ok(None) => {
                let config = Self::create_new();
                store.save(&data_dir_str, STORE_IDENTIFIER, &config)
                    .map_err(|e| format!("保存加密 PeerID 失败: {e}"))?;
                tracing::info!("Created new PeerIdConfig via EncryptedStore");
                Ok(config)
            }
            Err(e) => {
                // 加密数据损坏/无法解密→删除并重建（静默身份轮换）
                tracing::warn!("PeerID 加密存储损坏，删除并重建: {e}");
                delete_corrupted_entry(data_dir);
                let config = Self::create_new();
                store.save(&data_dir_str, STORE_IDENTIFIER, &config)
                    .map_err(|e| format!("保存加密 PeerID 失败（重建后）: {e}"))?;
                tracing::info!("Recreated PeerIdConfig after corruption");
                Ok(config)
            }
        }
    }

    fn save(&self, data_dir: &Path) -> Result<(), String> {
        let data_dir_str = data_dir.to_string_lossy();
        let store = EncryptedStore::open()
            .map_err(|e| format!("Keyring 不可用，无法加密保存 PeerID: {e}"))?;
        store.save(&data_dir_str, STORE_IDENTIFIER, self)
            .map_err(|e| format!("保存加密 PeerID 失败: {e}"))
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