//! 通用的加密文件存储层。
//!
//! 基于 OS keyring 中的 master key，对任意可序列化数据结构提供
//! AES-256-GCM 加密持久化。
//!
//! 字节级加密原语由 `identity` 模块的 `save_bytes`/`load_bytes`/`delete_file` 提供，
//! 本模块仅在其上包装 Serde 序列化/反序列化。
//!
//! 文件格式：
//! ```text
//! [version(1)][salt(16)][nonce(12)][AES-256-GCM(serde_json(plaintext))]
//! ```

use serde::{de::DeserializeOwned, Serialize};
use zeroize::Zeroizing;

use crate::identity;

/// 基于 OS keyring 的通用加密文件存储。
///
/// 持有 master key，通过 `save`/`load` 对任意 Serde 数据结构进行
/// AES-256-GCM 加密持久化。
pub struct EncryptedStore {
    master_key: Zeroizing<[u8; 32]>,
}

impl EncryptedStore {
    /// 打开存储。要求 master key 已在 Keyring 中存在（调用 `init` 或已有数据）。
    pub fn open() -> anyhow::Result<Self> {
        identity::ensure_keyring_init();
        let master_key = identity::PrivateKeyHandle::load_master_key()?.ok_or_else(|| {
            anyhow::anyhow!(
                "Keyring unavailable: no master key found. Call EncryptedStore::init() first."
            )
        })?;
        Ok(Self { master_key })
    }

    /// 初始化存储：master key 不存在时生成并保存到 Keyring。
    pub fn init() -> anyhow::Result<Self> {
        identity::ensure_keyring_init();
        let master_key = identity::PrivateKeyHandle::ensure_master_key()?;
        Ok(Self { master_key })
    }

    /// 加密并持久化任意可序列化值（每保存派生新密钥，消除 GCM nonce 重用风险）。
    pub fn save<T: Serialize>(
        &self,
        data_dir: &str,
        identifier: &str,
        value: &T,
    ) -> anyhow::Result<()> {
        // Zeroizing 确保序列化缓冲（可能含私钥明文）被释放时零化
        let serialized = Zeroizing::new(serde_json::to_vec(value)?);
        identity::save_bytes(data_dir, identifier, &serialized, &self.master_key)
    }

    /// 加载并解密任意可反序列化值。
    ///
    /// 返回 `None` 表示文件不存在；`Err` 表示读取或解密失败。
    pub fn load<T: DeserializeOwned>(
        &self,
        data_dir: &str,
        identifier: &str,
    ) -> anyhow::Result<Option<T>> {
        identity::load_bytes(data_dir, identifier, &self.master_key)?
            .map(|v| serde_json::from_slice(&v))
            .transpose()
            .map_err(Into::into)
    }

    /// 删除加密文件（不存在时静默成功）。
    pub fn delete(&self, data_dir: &str, identifier: &str) {
        identity::delete_file(data_dir, identifier);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
    struct TestConfig {
        field: String,
        count: u32,
    }

    fn temp_dir() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "rootcell_store_test_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn roundtrip_serializable_struct() {
        let dir = temp_dir();
        let master = [0x55u8; 32];
        let store = &EncryptedStore { master_key: Zeroizing::new(master) };

        let cfg = TestConfig { field: "hello".into(), count: 42 };
        store.save(dir.to_str().unwrap(), "cfg", &cfg).unwrap();

        let loaded: TestConfig = store
            .load(dir.to_str().unwrap(), "cfg")
            .unwrap()
            .expect("config should exist");
        assert_eq!(loaded, cfg);

        let wrong = EncryptedStore { master_key: Zeroizing::new([0x66u8; 32]) };
        assert!(wrong.load::<TestConfig>(dir.to_str().unwrap(), "cfg").is_err());

        assert!(store.load::<TestConfig>(dir.to_str().unwrap(), "nobody").unwrap().is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }
}