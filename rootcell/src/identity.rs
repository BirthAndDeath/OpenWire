//! 私钥管理模块
//!
//! 提供安全的私钥存储和访问，包括：
//! - mlock内存锁定（防止swap到磁盘）
//! - 自动zeroize清理
//! - keyring优先，失败时使用临时对称密钥加密文件
//! - 跨平台支持

use aes_gcm::{AeadInPlace, Aes256Gcm, KeyInit, Nonce};
use anyhow;
use rand::Rng;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use tracing;
use zeroize::{Zeroize, Zeroizing};

#[cfg(unix)]
use libc;

/// 临时对称密钥的长度（AES-256）
const TEMP_KEY_LENGTH: usize = 32;
/// AES-GCM nonce长度
const NONCE_LENGTH: usize = 12;
/// Keyring服务名称
const KEYRING_SERVICE: &str = "rootcell";

/// 私钥存储方式
#[derive(Debug, Clone, PartialEq)]
pub enum StorageBackend {
    /// 使用系统keyring
    Keyring,
    /// 使用加密文件（keyring不可用时）
    EncryptedFile,
}

/// 私钥Handle结构体
///
/// 提供安全的私钥访问，包括：
/// - mlock内存锁定（防止swap到磁盘）
/// - 自动zeroize清理
/// - keyring失败时使用临时对称密钥加密
pub struct PrivateKeyHandle {
    /// 解密后的私钥（mlock保护，自动zeroize）
    private_key: Zeroizing<Vec<u8>>,
    /// 存储后端类型
    backend: StorageBackend,
    /// peer_id或identity_id
    identifier: String,
    /// 是否已锁定内存
    locked: bool,
}

impl PrivateKeyHandle {
    /// 从存储中加载私钥并创建Handle
    ///
    /// # 参数
    /// - `data_dir`: 数据目录路径
    /// - `identifier`: peer_id或identity_id
    ///
    /// # 返回
    /// 成功返回PrivateKeyHandle，失败返回错误
    pub fn load(data_dir: &Path, identifier: &str) -> anyhow::Result<Self> {
        tracing::debug!("Loading private key for identifier: {}", identifier);

        // 尝试从keyring获取
        if let Some(private_key) = Self::try_load_from_keyring(identifier)? {
            tracing::info!("✅ Loaded private key from keyring for {}", identifier);
            let mut handle = Self {
                private_key: Zeroizing::new(private_key),
                backend: StorageBackend::Keyring,
                identifier: identifier.to_string(),
                locked: false,
            };
            handle.lock_memory()?;
            return Ok(handle);
        }

        // 尝试从加密文件获取
        tracing::debug!("Keyring miss, trying encrypted file");
        if let Some(private_key) = Self::try_load_from_encrypted_file(data_dir, identifier)? {
            tracing::info!(
                "✅ Loaded private key from encrypted file for {}",
                identifier
            );
            let mut handle = Self {
                private_key: Zeroizing::new(private_key),
                backend: StorageBackend::EncryptedFile,
                identifier: identifier.to_string(),
                locked: false,
            };
            handle.lock_memory()?;
            return Ok(handle);
        }

        anyhow::bail!(
            "Private key not found in keyring or encrypted file for {}",
            identifier
        )
    }

    /// 保存私钥并创建Handle
    ///
    /// # 参数
    /// - `data_dir`: 数据目录路径
    /// - `identifier`: peer_id或identity_id
    /// - `private_key`: 私钥字节
    ///
    /// # 返回
    /// 成功返回PrivateKeyHandle和使用的存储后端
    pub fn save(
        data_dir: &Path,
        identifier: &str,
        private_key: &[u8],
    ) -> anyhow::Result<(Self, StorageBackend)> {
        tracing::debug!("Saving private key for identifier: {}", identifier);

        // 先尝试保存到keyring
        if Self::save_to_keyring(identifier, private_key)? {
            // 使用了keyring
            let mut handle = Self {
                private_key: Zeroizing::new(private_key.to_vec()),
                backend: StorageBackend::Keyring,
                identifier: identifier.to_string(),
                locked: false,
            };
            handle.lock_memory()?;
            Ok((handle, StorageBackend::Keyring))
        } else {
            // keyring失败，使用加密文件
            tracing::warn!(
                "Keyring unavailable, falling back to encrypted file storage for {}",
                identifier
            );
            let temp_key = Self::generate_temp_key(identifier);
            Self::save_to_encrypted_file(data_dir, identifier, private_key, &temp_key)?;

            let mut handle = Self {
                private_key: Zeroizing::new(private_key.to_vec()),
                backend: StorageBackend::EncryptedFile,
                identifier: identifier.to_string(),
                locked: false,
            };
            handle.lock_memory()?;
            Ok((handle, StorageBackend::EncryptedFile))
        }
    }

    /// 获取私钥的引用
    pub fn get_private_key(&self) -> &[u8] {
        &self.private_key
    }

    /// 获取存储后端类型
    pub fn backend(&self) -> &StorageBackend {
        &self.backend
    }

    /// 获取标识符
    pub fn identifier(&self) -> &str {
        &self.identifier
    }

    /// 锁定内存（mlock）
    ///
    /// 防止私钥被swap到磁盘
    #[cfg(unix)]
    fn lock_memory(&mut self) -> anyhow::Result<()> {
        if self.locked {
            return Ok(());
        }

        let ptr = self.private_key.as_ptr();
        let len = self.private_key.len();

        unsafe {
            let ret = libc::mlock(ptr as *const libc::c_void, len);
            if ret != 0 {
                let err = std::io::Error::last_os_error();
                tracing::warn!("Failed to mlock private key memory: {}", err);
                // mlock失败不致命，继续运行但记录警告
                return Ok(());
            }
        }

        self.locked = true;
        tracing::debug!("Successfully locked {} bytes of private key memory", len);
        Ok(())
    }

    #[cfg(not(unix))]
    fn lock_memory(&mut self) -> anyhow::Result<()> {
        // Windows和其他平台暂不支持mlock
        tracing::debug!("mlock not supported on this platform");
        self.locked = false;
        Ok(())
    }

    /// 解锁内存（munlock）
    #[cfg(unix)]
    fn unlock_memory(&mut self) {
        if !self.locked {
            return;
        }

        let ptr = self.private_key.as_ptr();
        let len = self.private_key.len();

        unsafe {
            let ret = libc::munlock(ptr as *const libc::c_void, len);
            if ret != 0 {
                tracing::warn!("Failed to munlock private key memory");
            }
        }

        self.locked = false;
    }

    #[cfg(not(unix))]
    fn unlock_memory(&mut self) {
        self.locked = false;
    }

    /// 尝试从keyring加载私钥
    fn try_load_from_keyring(identifier: &str) -> anyhow::Result<Option<Vec<u8>>> {
        match keyring::Entry::new(KEYRING_SERVICE, identifier) {
            Ok(entry) => match entry.get_password() {
                Ok(pwd) if !pwd.trim().is_empty() => {
                    let decoded = hex::decode(pwd.trim())?;
                    Ok(Some(decoded))
                }
                Ok(_) => {
                    tracing::debug!("Keyring returned empty for {}", identifier);
                    Ok(None)
                }
                Err(e) => {
                    tracing::debug!("Keyring get failed for {}: {}", identifier, e);
                    Ok(None)
                }
            },
            Err(e) => {
                tracing::debug!("Keyring entry creation failed for {}: {}", identifier, e);
                Ok(None)
            }
        }
    }

    /// 保存私钥到keyring
    fn save_to_keyring(identifier: &str, private_key: &[u8]) -> anyhow::Result<bool> {
        let private_key_hex = hex::encode(private_key);

        match keyring::Entry::new(KEYRING_SERVICE, identifier) {
            Ok(entry) => match entry.set_password(&private_key_hex) {
                Ok(()) => {
                    tracing::info!("✅ Saved private key to keyring for {}", identifier);
                    Ok(true)
                }
                Err(e) => {
                    tracing::warn!("Keyring save failed for {}: {}", identifier, e);
                    Ok(false)
                }
            },
            Err(e) => {
                tracing::warn!("Keyring entry creation failed for {}: {}", identifier, e);
                Ok(false)
            }
        }
    }

    /// 从keyring删除私钥
    pub fn delete_from_keyring(identifier: &str) {
        if let Ok(entry) = keyring::Entry::new(KEYRING_SERVICE, identifier) {
            if let Err(e) = entry.delete_credential() {
                tracing::warn!("Failed to delete keyring entry for {}: {}", identifier, e);
            }
        }
    }

    /// 尝试从加密文件加载私钥
    fn try_load_from_encrypted_file(
        data_dir: &Path,
        identifier: &str,
    ) -> anyhow::Result<Option<Vec<u8>>> {
        let path = Self::encrypted_file_path(data_dir, identifier);

        if !path.exists() {
            return Ok(None);
        }

        let stored = std::fs::read(&path)?;
        if stored.is_empty() {
            return Ok(None);
        }

        // 生成临时密钥用于解密
        let temp_key = Self::generate_temp_key(identifier);

        // 解密
        match Self::decrypt_private_key(&stored, &temp_key) {
            Ok(decrypted) => Ok(Some(decrypted)),
            Err(e) => {
                tracing::warn!("Failed to decrypt private key file: {}", e);
                Ok(None)
            }
        }
    }

    /// 保存私钥到加密文件
    fn save_to_encrypted_file(
        data_dir: &Path,
        identifier: &str,
        private_key: &[u8],
        temp_key: &[u8],
    ) -> anyhow::Result<()> {
        let path = Self::encrypted_file_path(data_dir, identifier);

        // 加密
        let encrypted = Self::encrypt_private_key(private_key, temp_key)?;

        // 安全写入
        Self::write_encrypted_file(&path, &encrypted)?;

        tracing::info!(
            "✅ Saved encrypted private key to file for {} at {:?}",
            identifier,
            path
        );

        // 验证：立即解密检查
        let verify_encrypted = std::fs::read(&path)?;
        let verify_decrypted = Self::decrypt_private_key(&verify_encrypted, temp_key)?;

        if verify_decrypted != private_key {
            // 验证失败，清理
            let _ = std::fs::remove_file(&path);
            anyhow::bail!("File verification failed after write");
        }

        Ok(())
    }

    /// 删除加密文件
    pub fn delete_encrypted_file(data_dir: &Path, identifier: &str) -> anyhow::Result<()> {
        let path = Self::encrypted_file_path(data_dir, identifier);
        if path.exists() {
            std::fs::remove_file(&path)?;
        }
        Ok(())
    }

    /// 生成临时对称密钥
    ///
    /// 从identifier派生确定性密钥，用于加密文件
    fn generate_temp_key(identifier: &str) -> Vec<u8> {
        let mut hasher = Sha256::new();
        hasher.update(identifier.as_bytes());
        hasher.update(b"rootcell_temp_key_v1");
        hasher.finalize().to_vec()
    }

    /// 加密私钥
    fn encrypt_private_key(private_key: &[u8], key: &[u8]) -> anyhow::Result<Vec<u8>> {
        let cipher = Aes256Gcm::new_from_slice(key)
            .map_err(|e| anyhow::anyhow!("Invalid key length: {}", e))?;

        // 使用 CSPRNG 生成安全的随机 nonce
        let mut nonce_bytes = [0u8; NONCE_LENGTH];
        rand::rng().fill_bytes(&mut nonce_bytes);

        let nonce = Nonce::from_slice(&nonce_bytes);

        let mut buffer = private_key.to_vec();
        let tag = cipher
            .encrypt_in_place_detached(nonce, &[], &mut buffer)
            .map_err(|e| anyhow::anyhow!("Encryption failed: {}", e))?;

        // 格式：nonce (12 bytes) + ciphertext + tag (16 bytes)
        let mut result = nonce.to_vec();
        result.extend_from_slice(&buffer);
        result.extend_from_slice(tag.as_slice());

        Ok(result)
    }

    /// 解密私钥
    fn decrypt_private_key(encrypted: &[u8], key: &[u8]) -> anyhow::Result<Vec<u8>> {
        if encrypted.len() < NONCE_LENGTH + 16 {
            // nonce + tag最小长度
            anyhow::bail!("Encrypted data too short");
        }

        let (nonce_bytes, rest) = encrypted.split_at(NONCE_LENGTH);
        let nonce = Nonce::from_slice(nonce_bytes);

        // 分离ciphertext和tag
        let ciphertext_len = rest.len() - 16;
        let (ciphertext, tag_bytes) = rest.split_at(ciphertext_len);

        use aes_gcm::aead::AeadInPlace;
        let cipher = Aes256Gcm::new_from_slice(key)
            .map_err(|e| anyhow::anyhow!("Invalid key length: {}", e))?;

        let mut buffer = ciphertext.to_vec();
        let tag = aes_gcm::Tag::from_slice(tag_bytes);

        cipher
            .decrypt_in_place_detached(nonce, &[], &mut buffer, tag)
            .map_err(|e| anyhow::anyhow!("Decryption failed: {}", e))?;

        Ok(buffer)
    }

    /// 获取加密文件路径
    fn encrypted_file_path(data_dir: &Path, identifier: &str) -> PathBuf {
        let mut hasher = Sha256::new();
        hasher.update(identifier.as_bytes());
        let hash = hex::encode(hasher.finalize());
        data_dir
            .join("private_keys")
            .join(format!("pk_{}.enc", &hash[..16]))
    }

    /// 安全写入加密文件
    fn write_encrypted_file(path: &Path, content: &[u8]) -> anyhow::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        // 先写入临时文件，成功后再重命名（原子性）
        let temp_path = path.with_extension("tmp");
        std::fs::write(&temp_path, content)?;

        // 设置权限（Unix: 0600）
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&temp_path)?.permissions();
            perms.set_mode(0o600);
            std::fs::set_permissions(&temp_path, perms)?;
        }

        // 原子重命名
        std::fs::rename(&temp_path, path)?;

        Ok(())
    }

    /// 诊断存储状态
    pub fn diagnose_storage(data_dir: &Path, identifier: &str) -> String {
        let mut report = format!("=== Private Key Storage for {} ===\n", identifier);

        // Keyring状态
        match Self::try_load_from_keyring(identifier) {
            Ok(Some(key)) => {
                report.push_str(&format!("✅ Keyring: {} bytes\n", key.len()));
            }
            Ok(None) => report.push_str("❌ Keyring: Not available\n"),
            Err(e) => report.push_str(&format!("⚠️ Keyring error: {}\n", e)),
        }

        // 文件状态
        let path = Self::encrypted_file_path(data_dir, identifier);
        report.push_str(&format!("\n📁 Encrypted File: {:?}\n", path));

        if path.exists() {
            match std::fs::metadata(&path) {
                Ok(meta) => {
                    report.push_str(&format!("✅ Available: {} bytes\n", meta.len()));
                }
                Err(e) => report.push_str(&format!("❌ Read error: {}\n", e)),
            }
        } else {
            report.push_str("❌ Not found\n");
        }

        report
    }
}

impl Drop for PrivateKeyHandle {
    fn drop(&mut self) {
        tracing::debug!(
            "Dropping PrivateKeyHandle for {}, unlocking memory",
            self.identifier
        );

        // 先解锁内存
        self.unlock_memory();

        // Zeroizing会自动清零，这里显式调用确保
        self.private_key.zeroize();
    }
}

// 不允许Clone，防止意外复制私钥
impl Clone for PrivateKeyHandle {
    fn clone(&self) -> Self {
        panic!("PrivateKeyHandle cannot be cloned for security reasons");
    }
}
