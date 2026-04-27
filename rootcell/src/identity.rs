//! 私钥管理模块
//!
//! 提供安全的私钥存储和访问，包括：
//! - mlock内存锁定（防止swap到磁盘）
//! - 自动zeroize清理
//! - 使用系统keyring存储
//! - 跨平台支持

use anyhow;
use tracing;
use zeroize::Zeroizing;

#[cfg(unix)]
use libc;

/// Keyring服务名称
const KEYRING_SERVICE: &str = "rootcell";

/// 私钥存储方式
#[derive(Debug, Clone, PartialEq)]
pub enum StorageBackend {
    /// 使用系统keyring
    Keyring,
}

/// 私钥Handle结构体
///
/// 提供安全的私钥访问，包括：
/// - mlock内存锁定（防止swap到磁盘）
/// - 自动zeroize清理
/// - 使用系统keyring存储
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
    /// - `identifier`: peer_id或identity_id
    ///
    /// # 返回
    /// 成功返回PrivateKeyHandle，失败返回错误
    pub fn load(identifier: &str) -> anyhow::Result<Self> {
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

        anyhow::bail!("Private key not found in keyring for {}", identifier)
    }

    /// 保存私钥并创建Handle
    ///
    /// # 参数
    /// - `identifier`: peer_id或identity_id
    /// - `private_key`: 私钥字节
    ///
    /// # 返回
    /// 成功返回PrivateKeyHandle和使用的存储后端
    pub fn save(identifier: &str, private_key: &[u8]) -> anyhow::Result<(Self, StorageBackend)> {
        tracing::debug!("Saving private key for identifier: {}", identifier);

        // 保存到keyring
        if Self::save_to_keyring(identifier, private_key)? {
            let mut handle = Self {
                private_key: Zeroizing::new(private_key.to_vec()),
                backend: StorageBackend::Keyring,
                identifier: identifier.to_string(),
                locked: false,
            };
            handle.lock_memory()?;
            Ok((handle, StorageBackend::Keyring))
        } else {
            anyhow::bail!("Failed to save private key to keyring for {}", identifier)
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
                anyhow::bail!("Failed to mlock private key memory: {}", err);
            }
        }

        self.locked = true;
        tracing::debug!("Successfully locked {} bytes of private key memory", len);
        Ok(())
    }

    /// 锁定内存（使用平台特定API防止swap到磁盘）
    #[cfg(windows)]
    fn lock_memory(&mut self) -> anyhow::Result<()> {
        if self.locked {
            return Ok(());
        }

        let ptr = self.private_key.as_ptr();
        let len = self.private_key.len();

        // 使用 Windows VirtualLock 锁定内存页
        // VirtualLock 锁定指定区域到物理内存，防止分页到磁盘
        let ret = unsafe {
            windows_sys::Win32::System::Memory::VirtualLock(ptr as *const std::ffi::c_void, len)
        };

        if ret == 0 {
            let err = std::io::Error::last_os_error();
            tracing::warn!("Failed to VirtualLock private key memory: {}", err);
            anyhow::bail!("Failed to VirtualLock private key memory: {}", err);
        }

        self.locked = true;
        tracing::debug!(
            "Successfully VirtualLocked {} bytes of private key memory",
            len
        );
        Ok(())
    }

    #[cfg(not(any(unix, windows)))]
    fn lock_memory(&mut self) -> anyhow::Result<()> {
        // 其他平台暂不支持内存锁定
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

    #[cfg(windows)]
    fn unlock_memory(&mut self) {
        if !self.locked {
            return;
        }

        let ptr = self.private_key.as_ptr();
        let len = self.private_key.len();

        let ret = unsafe {
            windows_sys::Win32::System::Memory::VirtualUnlock(ptr as *const std::ffi::c_void, len)
        };

        if ret == 0 {
            tracing::warn!("Failed to VirtualUnlock private key memory");
        }

        self.locked = false;
    }

    #[cfg(not(any(unix, windows)))]
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
    pub fn delete_from_keyring(identifier: &str) -> anyhow::Result<()> {
        let entry = keyring::Entry::new(KEYRING_SERVICE, identifier)?;
        entry.delete_credential()?;
        tracing::debug!("Deleted keyring entry for {}", identifier);
        Ok(())
    }

    /// 诊断存储状态
    pub fn diagnose_storage(identifier: &str) -> String {
        let mut report = format!("=== Private Key Storage for {} ===\n", identifier);

        // Keyring状态
        match Self::try_load_from_keyring(identifier) {
            Ok(Some(key)) => {
                report.push_str(&format!("✅ Keyring: {} bytes\n", key.len()));
            }
            Ok(None) => report.push_str("❌ Keyring: Not available\n"),
            Err(e) => report.push_str(&format!("⚠️ Keyring error: {}\n", e)),
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

        // private_key 的类型是 Zeroizing<Vec<u8>>，Drop 时已自动清零
        // 无需显式调用 zeroize()
    }
}
