//! 私钥管理模块
//!
//! 提供安全的私钥存储和访问，采用**双层密钥架构**：
//!
//! # 架构设计
//!
//! ```text
//! Keyring (存储主密钥)
//!   └── "rc_master" → AES-256 对称密钥 (32 bytes)
//!           │
//!           ▼  AES-256-GCM 加解密
//!   ┌───────────────────────────────┐
//!   │  加密文件 (data_dir/keys/)     │
//!   │  {identifier}.enc             │
//!   └───────────────────────────────┘
//! ```
//!
//! # 存储后端
//!
//! - [`StorageBackend::Keyring`]：主密钥存储在系统 Keyring 中（默认，推荐）
//! - [`StorageBackend::PasswordDerived`]：密钥从用户密码派生（降级方案）
//!
//! # 安全性
//!
//! - mlock 内存锁定（防止 swap 到磁盘）
//! - Zeroizing 自动清零
//! - AES-256-GCM 认证加密
//! - 加密文件加载后以只读方式管理

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow;
use tracing;
use zeroize::Zeroizing;

#[cfg(unix)]
use libc;

/// Keyring 服务名称
const KEYRING_SERVICE: &str = "rootcell";

/// 主密钥在 Keyring 中的标识符（短标识，避免平台长度限制）
const MASTER_KEY_IDENTIFIER: &str = "rc_master";

/// 全局标志：Keyring 默认存储是否已初始化
static KEYRING_INITIALIZED: AtomicBool = AtomicBool::new(false);

/// 确保 Keyring 的默认存储后端已初始化。
///
/// keyring v4 架构变更：`keyring-core` 不再自动检测存储后端，
/// 需要显式调用 `set_default_store()` 注册后端。
/// 此函数在首次调用时通过 `keyring::use_native_store()` 设置平台原生存储。
///
/// 线程安全：使用 AtomicBool 确保只初始化一次。
fn ensure_keyring_initialized() {
    if KEYRING_INITIALIZED.load(Ordering::Relaxed) {
        return;
    }
    // 使用 CAS 确保只有一个线程执行初始化
    if KEYRING_INITIALIZED
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return; // 其他线程已初始化
    }

    // keyring v4 架构变更：需要显式调用 use_native_store() 注册平台原生存储后端。
    //
    // Linux/BSD: 传 `true` 强制使用 Secret Service (dbus)，避免 keyutils（内核密钥环）
    // 不支持持久化存储的问题。
    // 其他平台: 传 `false` 使用平台默认后端（macOS Keychain, Windows Credential Manager）。
    let not_keyutils = cfg!(any(
        target_os = "linux",
        target_os = "freebsd",
        target_os = "openbsd"
    ));
    if let Err(e) = keyring::use_native_store(not_keyutils) {
        tracing::warn!(
            "Failed to initialize native keyring store: {}. Keyring will be unavailable.",
            e
        );
    }
}

/// 私钥存储方式
#[derive(Debug, Clone, PartialEq)]
pub enum StorageBackend {
    /// 使用系统 Keyring 存储主密钥
    Keyring,
    /// 使用用户密码派生密钥（降级方案）
    PasswordDerived,
}

/// 私钥 Handle 结构体
///
/// 提供安全的私钥访问，包括：
/// - mlock 内存锁定（防止 swap 到磁盘）
/// - 自动 zeroize 清理
/// - 使用系统 Keyring 存储主密钥 + 加密文件存储私钥
pub struct PrivateKeyHandle {
    /// 解密后的私钥（mlock 保护，自动 zeroize）
    private_key: Zeroizing<Vec<u8>>,
    /// 存储后端类型
    backend: StorageBackend,
    /// 标识符（如 identity_id_mlkem）
    identifier: String,
    /// 数据目录路径（用于定位加密文件）
    data_dir: String,
    /// 是否已锁定内存
    locked: bool,
}

impl PrivateKeyHandle {
    // ========================================================================
    // 主密钥管理
    // ========================================================================

    /// 生成并保存随机 AES-256 主密钥到 Keyring
    ///
    /// 主密钥仅 32 字节，identifier 仅 9 字符，不会触发平台长度限制。
    pub fn generate_and_save_master_key() -> anyhow::Result<[u8; 32]> {
        // 确保 Keyring 默认存储后端已初始化（keyring v4 需要显式设置）
        ensure_keyring_initialized();

        let key: [u8; 32] = rand::random();
        let key_hex = hex::encode(key);
        match keyring_core::Entry::new(KEYRING_SERVICE, MASTER_KEY_IDENTIFIER) {
            Ok(entry) => {
                entry.set_password(&key_hex)?;
                tracing::info!("✅ Generated and saved master key to Keyring");
                Ok(key)
            }
            Err(e) => {
                anyhow::bail!("Failed to create Keyring entry for master key: {}", e)
            }
        }
    }

    /// 从 Keyring 加载主密钥
    pub fn load_master_key() -> anyhow::Result<Option<[u8; 32]>> {
        // 确保 Keyring 默认存储后端已初始化（keyring v4 需要显式设置）
        ensure_keyring_initialized();

        match keyring_core::Entry::new(KEYRING_SERVICE, MASTER_KEY_IDENTIFIER) {
            Ok(entry) => match entry.get_password() {
                Ok(pwd) if !pwd.trim().is_empty() => {
                    let decoded = hex::decode(pwd.trim())?;
                    if decoded.len() == 32 {
                        let mut key = [0u8; 32];
                        key.copy_from_slice(&decoded);
                        tracing::debug!("✅ Loaded master key from Keyring");
                        Ok(Some(key))
                    } else {
                        tracing::warn!(
                            "Master key in Keyring has unexpected length: {}",
                            decoded.len()
                        );
                        Ok(None)
                    }
                }
                Ok(_) => {
                    tracing::debug!("Master key entry in Keyring is empty");
                    Ok(None)
                }
                Err(e) => {
                    tracing::debug!("Failed to get master key from Keyring: {}", e);
                    Ok(None)
                }
            },
            Err(e) => {
                tracing::debug!("Failed to create Keyring entry for master key: {}", e);
                Ok(None)
            }
        }
    }

    /// 检查 Keyring 中是否存在主密钥
    pub fn has_master_key() -> bool {
        Self::load_master_key().ok().flatten().is_some()
    }

    /// 检查 Keyring 是否真正可用。
    ///
    /// 直接检测默认存储后端是否已成功注册，无需创建测试条目。
    /// `ensure_keyring_initialized()` 调用 `use_native_store()` 注册后端，
    /// 注册成功后 `get_default_store()` 返回 `Some`。
    ///
    /// # 与 `load_master_key()` 的区别
    ///
    /// `load_master_key()` 返回 `Ok(None)` 有两种可能：
    /// 1. Keyring 可用但还没有主密钥（正常情况）
    /// 2. Keyring 完全不可用（后端未初始化、dbus 服务未运行等）
    /// - 无法区分，因此需要独立的可用性检测。
    ///
    /// # 返回
    /// - `true`: Keyring 可用
    /// - `false`: Keyring 不可用（后端未初始化或连接失败）
    pub fn check_keyring_available() -> bool {
        ensure_keyring_initialized();
        keyring_core::get_default_store().is_some()
    }

    /// 删除 Keyring 中的主密钥
    pub fn delete_master_key() -> anyhow::Result<()> {
        // 确保 Keyring 默认存储后端已初始化（keyring v4 需要显式设置）
        ensure_keyring_initialized();

        let entry = keyring_core::Entry::new(KEYRING_SERVICE, MASTER_KEY_IDENTIFIER)?;
        match entry.get_password() {
            Ok(_) => {
                entry.delete_credential()?;
                tracing::info!("Deleted master key from Keyring");
                Ok(())
            }
            Err(keyring_core::Error::NoEntry) => {
                tracing::debug!("Master key not found in Keyring, skipping deletion");
                Ok(())
            }
            Err(e) => {
                tracing::warn!("Failed to check master key in Keyring: {:?}", e);
                // 尝试强制删除
                entry.delete_credential()?;
                Ok(())
            }
        }
    }

    // ========================================================================
    // 加密文件存储
    // ========================================================================

    /// 将 identifier 哈希为短文件名（BLAKE3 前 32 字符 hex），
    /// 避免 ML-DSA 公钥 hex（2624 字符）超出文件系统路径长度限制。
    fn hash_identifier(identifier: &str) -> String {
        let hash = blake3::hash(identifier.as_bytes());
        hex::encode(&hash.as_bytes()[..16]) // 16 字节 → 32 字符 hex
    }

    /// 获取加密文件路径
    ///
    /// 文件名使用 identifier 的 BLAKE3 哈希（32 字符 hex），
    /// 避免 ML-DSA 公钥 hex（2624 字符）超出文件系统路径长度限制。
    fn encrypted_file_path(data_dir: &str, identifier: &str) -> std::path::PathBuf {
        let short_name = Self::hash_identifier(identifier);
        Path::new(data_dir)
            .join("keys")
            .join(format!("{}.enc", short_name))
    }

    /// 用主密钥加密私钥并写入文件
    ///
    /// 文件格式: `[nonce 12 bytes][ciphertext + AES-GCM tag]`
    /// 加载后文件以只读方式管理，防止意外损坏。
    pub fn save_encrypted_private_key(
        data_dir: &str,
        identifier: &str,
        private_key: &[u8],
        master_key: &[u8; 32],
    ) -> anyhow::Result<()> {
        use aes_gcm::aead::{Aead, AeadCore, KeyInit, OsRng};
        use aes_gcm::{Aes256Gcm, Key};

        let keys_dir = Path::new(data_dir).join("keys");
        std::fs::create_dir_all(&keys_dir)?;

        let key = Key::<Aes256Gcm>::from_slice(master_key);
        let cipher = Aes256Gcm::new(key);
        let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
        let ciphertext = cipher
            .encrypt(&nonce, private_key)
            .map_err(|e| anyhow::anyhow!("AES-256-GCM encryption failed: {:?}", e))?;

        // 写入文件: [nonce 12 bytes][ciphertext + tag]
        let path = Self::encrypted_file_path(data_dir, identifier);
        let mut file_content = nonce.to_vec();
        file_content.extend_from_slice(&ciphertext);
        std::fs::write(&path, file_content)?;

        // 设置文件权限为仅所有者可读写 (Unix)
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Ok(metadata) = std::fs::metadata(&path) {
                let mut perms = metadata.permissions();
                perms.set_mode(0o600);
                let _ = std::fs::set_permissions(&path, perms);
            }
        }

        tracing::debug!(
            "Saved encrypted private key for {} ({} bytes encrypted)",
            identifier,
            ciphertext.len()
        );
        Ok(())
    }

    /// 从加密文件加载私钥
    ///
    /// 加载后以只读方式打开文件，防止后续意外写入损坏。
    pub fn load_encrypted_private_key(
        data_dir: &str,
        identifier: &str,
        master_key: &[u8; 32],
    ) -> anyhow::Result<Option<Vec<u8>>> {
        use aes_gcm::aead::{Aead, KeyInit};
        use aes_gcm::{Aes256Gcm, Key, Nonce};

        let path = Self::encrypted_file_path(data_dir, identifier);
        if !path.exists() {
            return Ok(None);
        }

        // 以只读方式读取文件内容
        let file_content = std::fs::read(&path)?;
        if file_content.len() < 12 {
            tracing::warn!(
                "Encrypted key file for {} is too short ({} bytes), ignoring",
                identifier,
                file_content.len()
            );
            return Ok(None);
        }

        let (nonce_bytes, ciphertext) = file_content.split_at(12);
        let key = Key::<Aes256Gcm>::from_slice(master_key);
        let cipher = Aes256Gcm::new(key);
        let nonce = Nonce::from_slice(nonce_bytes);

        match cipher.decrypt(nonce, ciphertext) {
            Ok(plaintext) => {
                tracing::debug!(
                    "Loaded encrypted private key for {} ({} bytes decrypted)",
                    identifier,
                    plaintext.len()
                );
                Ok(Some(plaintext))
            }
            Err(_) => {
                tracing::warn!(
                    "Failed to decrypt private key for {} (wrong key or corrupted file)",
                    identifier
                );
                Ok(None)
            }
        }
    }

    /// 删除加密私钥文件
    pub fn delete_encrypted_private_key(data_dir: &str, identifier: &str) {
        let path = Self::encrypted_file_path(data_dir, identifier);
        if path.exists() {
            if let Err(e) = std::fs::remove_file(&path) {
                tracing::warn!(
                    "Failed to delete encrypted key file for {}: {}",
                    identifier,
                    e
                );
            } else {
                tracing::debug!("Deleted encrypted key file for {}", identifier);
            }
        }
    }

    // ========================================================================
    // 密码派生降级存储
    // ========================================================================

    /// 使用密码派生密钥保存私钥（降级方案）
    ///
    /// # 参数
    /// - `password_hex`: 前端用 Argon2id 处理后的 256 位密钥 hex 字符串
    ///
    /// # 安全性警告
    /// 此方案安全性低于 Keyring，仅作为 Keyring 不可用时的降级方案。
    pub fn save_with_password(
        data_dir: &str,
        identifier: &str,
        private_key: &[u8],
        password_hex: &str,
    ) -> anyhow::Result<()> {
        let key_bytes = hex::decode(password_hex)
            .map_err(|e| anyhow::anyhow!("Invalid password hex: {}", e))?;
        if key_bytes.len() != 32 {
            anyhow::bail!(
                "Password-derived key must be 32 bytes (256 bits), got {} bytes",
                key_bytes.len()
            );
        }
        let mut master_key = [0u8; 32];
        master_key.copy_from_slice(&key_bytes);
        Self::save_encrypted_private_key(data_dir, identifier, private_key, &master_key)
    }

    // ========================================================================
    // 公开 API
    // ========================================================================

    /// 保存私钥并创建 Handle
    ///
    /// 优先使用 Keyring 存储主密钥 + 加密文件存储私钥。
    /// 如果 Keyring 不可用，返回错误（调用方应使用 [`save_with_password`] 降级）。
    ///
    /// # 参数
    /// - `data_dir`: 数据目录路径（用于存放加密文件）
    /// - `identifier`: 标识符（如 `{identity_id}_mldsa`）
    /// - `private_key`: 私钥字节
    ///
    /// # 返回
    /// 成功返回 `(PrivateKeyHandle, StorageBackend)`
    pub fn save(
        data_dir: &str,
        identifier: &str,
        private_key: &[u8],
    ) -> anyhow::Result<(Self, StorageBackend)> {
        tracing::debug!("Saving private key for identifier: {}", identifier);

        // 1. 尝试加载或生成主密钥
        let master_key = match Self::load_master_key()? {
            Some(key) => key,
            None => {
                tracing::info!("No master key found in Keyring, generating new one");
                Self::generate_and_save_master_key()?
            }
        };

        // 2. 用主密钥加密私钥并写入文件
        Self::save_encrypted_private_key(data_dir, identifier, private_key, &master_key)?;

        // 3. 创建 Handle
        let mut handle = Self {
            private_key: Zeroizing::new(private_key.to_vec()),
            backend: StorageBackend::Keyring,
            identifier: identifier.to_string(),
            data_dir: data_dir.to_string(),
            locked: false,
        };
        handle.lock_memory()?;
        tracing::info!(
            "✅ Saved private key for {} (Keyring + encrypted file)",
            identifier
        );
        Ok((handle, StorageBackend::Keyring))
    }

    /// 从存储中加载私钥并创建 Handle
    ///
    /// # 参数
    /// - `data_dir`: 数据目录路径
    /// - `identifier`: 标识符
    /// - `password_hex`: 可选，密码派生密钥 hex（Keyring 不可用时使用）
    ///
    /// # 返回
    /// 成功返回 `PrivateKeyHandle`
    pub fn load(
        data_dir: &str,
        identifier: &str,
        password_hex: Option<&str>,
    ) -> anyhow::Result<Self> {
        tracing::debug!("Loading private key for identifier: {}", identifier);

        // 1. 尝试从 Keyring 加载主密钥
        if let Some(master_key) = Self::load_master_key()? {
            if let Some(private_key) =
                Self::load_encrypted_private_key(data_dir, identifier, &master_key)?
            {
                let mut handle = Self {
                    private_key: Zeroizing::new(private_key),
                    backend: StorageBackend::Keyring,
                    identifier: identifier.to_string(),
                    data_dir: data_dir.to_string(),
                    locked: false,
                };
                handle.lock_memory()?;
                tracing::info!(
                    "✅ Loaded private key for {} from encrypted file (Keyring)",
                    identifier
                );
                return Ok(handle);
            }
            // 主密钥存在但解密失败，可能是私钥文件损坏
            tracing::warn!(
                "Master key found but failed to decrypt private key for {}, file may be corrupted",
                identifier
            );
        }

        // 2. Keyring 不可用或解密失败，尝试密码派生
        if let Some(pwd) = password_hex {
            let key_bytes =
                hex::decode(pwd).map_err(|e| anyhow::anyhow!("Invalid password hex: {}", e))?;
            if key_bytes.len() == 32 {
                let mut master_key = [0u8; 32];
                master_key.copy_from_slice(&key_bytes);
                if let Some(private_key) =
                    Self::load_encrypted_private_key(data_dir, identifier, &master_key)?
                {
                    let mut handle = Self {
                        private_key: Zeroizing::new(private_key),
                        backend: StorageBackend::PasswordDerived,
                        identifier: identifier.to_string(),
                        data_dir: data_dir.to_string(),
                        locked: false,
                    };
                    handle.lock_memory()?;
                    tracing::info!(
                        "✅ Loaded private key for {} from encrypted file (PasswordDerived)",
                        identifier
                    );
                    return Ok(handle);
                }
            }
        }

        anyhow::bail!(
            "Private key not found for {}. Keyring unavailable and no valid password provided.",
            identifier
        )
    }

    /// 尝试将私钥从密码派生升级到 Keyring 主密钥加密。
    ///
    /// 如果当前 Handle 是 `PasswordDerived` 且 Keyring 可用，
    /// 则用 Keyring 主密钥重新加密私钥文件，并将 backend 标记更新为 `Keyring`。
    ///
    /// 这是为了支持以下场景：
    /// - 用户首次在无 Keyring 环境下使用密码启动
    /// - 后续 Keyring 服务变得可用（如安装了 gnome-keyring）
    /// - 自动迁移到更安全的 Keyring 存储，无需用户手动操作
    ///
    /// # 返回值
    /// - `Ok(true)`: 成功升级到 Keyring
    /// - `Ok(false)`: 无需升级（已经是 Keyring 或 Keyring 不可用）
    /// - `Err(e)`: 升级过程中发生错误
    pub fn try_upgrade_to_keyring(&mut self) -> anyhow::Result<bool> {
        // 已经是 Keyring 模式，无需升级
        if self.backend == StorageBackend::Keyring {
            return Ok(false);
        }

        // 检查 Keyring 是否可用
        let master_key = match Self::load_master_key()? {
            Some(key) => key,
            None => {
                // Keyring 不可用，尝试生成新的主密钥
                tracing::info!(
                    "Keyring available but no master key found, generating new one for upgrade"
                );
                match Self::generate_and_save_master_key() {
                    Ok(key) => key,
                    Err(e) => {
                        tracing::warn!(
                            "Cannot upgrade to Keyring: failed to generate master key: {}",
                            e
                        );
                        return Ok(false);
                    }
                }
            }
        };

        // 用 Keyring 主密钥重新加密私钥文件
        Self::save_encrypted_private_key(
            &self.data_dir,
            &self.identifier,
            &self.private_key,
            &master_key,
        )?;

        // 更新 backend 标记
        self.backend = StorageBackend::Keyring;
        tracing::info!(
            "✅ Upgraded private key for {} from PasswordDerived to Keyring storage",
            self.identifier
        );
        Ok(true)
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

    /// 获取数据目录
    pub fn data_dir(&self) -> &str {
        &self.data_dir
    }

    // ========================================================================
    // 内存锁定
    // ========================================================================

    /// 锁定内存（mlock）
    ///
    /// 防止私钥被 swap 到磁盘。
    /// 如果 mlock 失败，仅记录警告并继续运行（降级处理）。
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
                tracing::warn!(
                    "Failed to mlock private key memory ({}): {}. \
                     Private key may be swappable to disk. Continuing with degraded protection.",
                    len,
                    err
                );
                return Ok(());
            }
        }

        self.locked = true;
        tracing::debug!("Successfully locked {} bytes of private key memory", len);
        Ok(())
    }

    #[cfg(windows)]
    fn lock_memory(&mut self) -> anyhow::Result<()> {
        if self.locked {
            return Ok(());
        }

        let ptr = self.private_key.as_ptr();
        let len = self.private_key.len();

        let ret = unsafe {
            windows_sys::Win32::System::Memory::VirtualLock(ptr as *const std::ffi::c_void, len)
        };

        if ret == 0 {
            let err = std::io::Error::last_os_error();
            tracing::warn!(
                "Failed to VirtualLock private key memory ({}): {}. \
                 Private key may be swappable to disk. Continuing with degraded protection.",
                len,
                err
            );
            return Ok(());
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

    // ========================================================================
    // 诊断
    // ========================================================================

    /// 诊断存储状态
    pub fn diagnose_storage(identifier: &str) -> String {
        let mut report = format!("=== Private Key Storage for {} ===\n", identifier);

        // Keyring 主密钥状态
        match Self::load_master_key() {
            Ok(Some(_)) => report.push_str("✅ Keyring master key: Available\n"),
            Ok(None) => report.push_str("❌ Keyring master key: Not available\n"),
            Err(e) => report.push_str(&format!("⚠️ Keyring master key error: {}\n", e)),
        }

        report
    }

    // ========================================================================
    // 统一 KDF：密码 → 256 位密钥 hex
    // ========================================================================

    /// 使用 Argon2id 将用户密码派生为 256 位密钥，返回 64 字符 hex 字符串。
    ///
    /// 这是 CLI 和 Tauri 前端统一的 KDF 实现，保证参数完全一致。
    ///
    /// # 安全性设计
    /// 使用密码的 BLAKE3 哈希作为 Argon2id 的盐（确定性盐），确保：
    /// - 相同密码 → 相同密钥（可重现，无需额外存储盐）
    /// - 不同密码 → 不同盐 → 不同密钥（天然抗交叉密码攻击）
    /// - Argon2id 本身已提供强大的抗 GPU/ASIC 暴力破解能力
    ///
    /// # Argon2id 参数
    /// - Algorithm: Argon2id (RFC 9106)
    /// - Version: 0x13
    /// - Memory: 64 MiB (65536 KiB)
    /// - Iterations: 3
    /// - Parallelism: 4
    /// - Output: 32 bytes (256 bits)
    /// - Salt: BLAKE3(password) 的前 16 字节（确定性盐）
    ///
    /// # 返回
    /// 64 字符 hex 字符串（32 字节 AES-256 密钥），可直接用于 `save_with_password()` 或 `load()`。
    pub fn derive_key_from_password(password: &str) -> String {
        use argon2::Argon2;

        // 使用 BLAKE3(password) 的前 16 字节作为确定性盐
        let hash = blake3::hash(password.as_bytes());
        let salt: [u8; 16] = hash.as_bytes()[..16]
            .try_into()
            .expect("blake3 output is 32 bytes");

        let mut output_key_material = [0u8; 32];
        let argon2 = Argon2::default(); // Argon2id, 64 MiB, 3 iterations, 4 parallelism
        argon2
            .hash_password_into(password.as_bytes(), &salt, &mut output_key_material)
            .expect("Argon2id hashing should not fail with valid parameters");

        hex::encode(output_key_material)
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
    }
}
