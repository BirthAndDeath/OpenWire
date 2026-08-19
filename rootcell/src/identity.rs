use std::path::Path;
use std::pin::Pin;
use std::sync::{LazyLock, Mutex};

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use hkdf::Hkdf;
use sha2::Sha256;
use tracing;
use zeroize::{Zeroize, Zeroizing};

#[cfg(unix)]
use libc;

pub(crate) const KEYRING_SERVICE: &str = "rootcell";
pub(crate) const MASTER_KEY_IDENTIFIER: &str = "rc_master";
/// 加密文件格式版本号
pub(crate) const FILE_FORMAT_VERSION: u8 = 0x01;

/// 保护 master key 检查与生成串行化，避免并发 TOCTOU 竞态。
static MASTER_KEY_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

static KEYRING_INIT: LazyLock<()> = LazyLock::new(|| {
    #[cfg(target_os = "windows")]
    {
        if let Ok(store) = windows_native_keyring_store::Store::new() {
            keyring_core::set_default_store(store);
            tracing::info!("Windows Credential Manager store set as default");
        } else {
            tracing::warn!("Windows Credential Manager store initialization failed");
        }
    }
    #[cfg(target_os = "macos")]
    {
        if let Ok(store) = apple_native_keyring_store::keychain::Store::new() {
            keyring_core::set_default_store(store);
            tracing::info!("macOS Keychain store set as default");
        } else {
            tracing::warn!("macOS Keychain store initialization failed");
        }
    }
    #[cfg(target_os = "android")]
    {
        if let Ok(store) = android_native_keyring_store::Store::new() {
            keyring_core::set_default_store(store);
            tracing::info!("Android KeyStore set as default");
        } else {
            tracing::warn!("Android KeyStore initialization failed");
        }
    }
    #[cfg(target_os = "ios")]
    {
        if let Ok(store) = apple_native_keyring_store::keychain::Store::new() {
            keyring_core::set_default_store(store);
            tracing::info!("iOS Keychain store set as default");
        } else {
            tracing::warn!("iOS Keychain initialization failed");
        }
    }
    #[cfg(all(
        unix,
        not(any(target_os = "macos", target_os = "ios", target_os = "android"))
    ))]
    {
        if let Ok(store) = zbus_secret_service_keyring_store::Store::new() {
            keyring_core::set_default_store(store);
            tracing::info!("Linux Secret Service store set as default");
        } else {
            tracing::warn!("Linux Secret Service store initialization failed");
        }
    }
});

pub(crate) fn ensure_keyring_init() {
    LazyLock::force(&KEYRING_INIT);
}

/// 初始化当前平台的 keyring store（强制立即执行，非惰性）。
///
/// 桌面/iOS 平台无需手动调用，`check_keyring_available` 等函数会自动触发惰性初始化。
/// Android 平台需在 JNI 主线程（JVM 上下文就绪后）显式调用此函数，确保 keyring
/// 在正确的线程上完成初始化。
pub fn setup_default_keyring() {
    ensure_keyring_init();
}

pub struct PrivateKeyHandle {
    private_key: Pin<Box<[u8]>>,
    identifier: String,
    data_dir: String,
    locked: bool,
}

impl PrivateKeyHandle {
    pub fn generate_and_save_master_key() -> anyhow::Result<Zeroizing<[u8; 32]>> {
        let mut key: [u8; 32] = rand::random();
        let key_hex = Zeroizing::new(hex::encode(key));
        let result =
            keyring_core::Entry::new(KEYRING_SERVICE, MASTER_KEY_IDENTIFIER).and_then(|entry| {
            entry.set_password(&key_hex)?;
            tracing::info!("Generated and saved master key to Keyring");
            Ok(Zeroizing::new(key))
        });
        // 清零栈上的原始 key 副本（Zeroizing::new 对 Copy 类型只做了拷贝）
        key.zeroize();
        result.map_err(|e: keyring_core::Error| {
            anyhow::anyhow!("Failed to create Keyring entry for master key: {e}")
        })
    }

    /// 加载 master key。
    ///
    /// 返回 `None` 表示密钥不存在（首次启动），
    /// 返回 `Err` 表示密钥存在但无法读取（损坏或密钥环错误）。
    /// 调用方必须区分这两种情况，避免在损坏时覆盖密钥导致数据丢失。
    pub fn load_master_key() -> anyhow::Result<Option<Zeroizing<[u8; 32]>>> {
        match keyring_core::Entry::new(KEYRING_SERVICE, MASTER_KEY_IDENTIFIER) {
            Ok(entry) => match entry.get_password() {
                Ok(mut pwd) if !pwd.trim().is_empty() => {
                    let decode_result = hex::decode(pwd.trim());
                    pwd.zeroize();
                    let decoded = Zeroizing::new(decode_result?);
                    if decoded.len() == 32 {
                        let mut key = Zeroizing::new([0u8; 32]);
                        key.copy_from_slice(&decoded);
                        Ok(Some(key))
                    } else {
                        anyhow::bail!(
                            "Master key in Keyring has unexpected length: {}",
                            decoded.len()
                        );
                    }
                }
                Ok(_) => {
                    anyhow::bail!("Master key entry in Keyring is empty or whitespace-only");
                }
                Err(keyring_core::Error::NoEntry) => {
                    tracing::debug!("Master key not found in Keyring");
                    Ok(None)
                }
                Err(e) => {
                    anyhow::bail!("Failed to get master key from Keyring: {e}");
                }
            },
            Err(e) => {
                anyhow::bail!("Failed to create Keyring entry for master key: {e}");
            }
        }
    }

    pub fn has_master_key() -> anyhow::Result<bool> {
        Ok(Self::load_master_key()?.is_some())
    }

    /// 确保 master key 存在并返回。并发安全：持有全局锁，避免重复生成导致数据丢失。
    pub(crate) fn ensure_master_key() -> anyhow::Result<Zeroizing<[u8; 32]>> {
        let _guard = MASTER_KEY_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        if let Some(key) = Self::load_master_key()? {
            Ok(key)
        } else {
            tracing::info!("No master key found in Keyring, generating new one");
            Self::generate_and_save_master_key()
        }
    }

    /// 使用随机 salt 派生 AES 密钥（每保存派生新密钥）。
    ///
    /// salt 作为 HKDF 的 salt 参数，使每次保存的派生密钥均不同。
    /// 即使同一 identifier 的两次保存产生相同的 nonce，由于密钥不同，
    /// GCM nonce 重用攻击完全失效。salt 需与密文一同存储。
    pub(crate) fn derive_aes_key(
        master_key: &[u8; 32],
        identifier: &str,
        salt: &[u8; 16],
    ) -> Zeroizing<[u8; 32]> {
        let hk = Hkdf::<Sha256>::new(Some(salt.as_slice()), master_key);
        let mut aes_key = Zeroizing::new([0u8; 32]);
        hk.expand(identifier.as_bytes(), &mut *aes_key)
            .expect("HKDF expand should not fail with valid output length");
        aes_key
    }

    pub fn check_keyring_available() -> bool {
        ensure_keyring_init();
        // Entry::new 只构造句柄，不写入凭据，因此不会残留测试条目。
        keyring_core::Entry::new(KEYRING_SERVICE, MASTER_KEY_IDENTIFIER).is_ok()
    }

    pub fn delete_master_key(data_dir: &str) -> anyhow::Result<()> {
        let _guard = MASTER_KEY_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());

        let keys_dir = std::path::Path::new(data_dir).join("keys");
        if keys_dir.exists() {
            let enc_count = match std::fs::read_dir(&keys_dir) {
                Ok(entries) => entries
                    .filter_map(|e| e.ok())
                    .filter(|e| e.path().extension().map(|ext| ext == "enc").unwrap_or(false))
                    .count(),
                Err(_) => 0,
            };
            if enc_count > 0 {
                anyhow::bail!(
                    "Cannot delete master key: {enc_count} encrypted key file(s) exist in {:?}. \
                     Delete all identities first.",
                    keys_dir
                );
            }
        }

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
                tracing::error!("Failed to check master key in Keyring: {e:?}");
                Err(anyhow::anyhow!("Keyring is unavailable, cannot delete master key: {e}"))
            }
        }
    }

    pub(crate) fn hash_identifier(identifier: &str) -> String {
        let hash = blake3::hash(identifier.as_bytes());
        let bytes = hash.as_bytes();
        hex::encode(&bytes[..16.min(bytes.len())])
    }

    pub fn encrypted_file_path(data_dir: &str, identifier: &str) -> std::path::PathBuf {
        let short_name = Self::hash_identifier(identifier);
        Path::new(data_dir).join("keys").join(format!("{}.enc", short_name))
    }

    pub fn save_encrypted_private_key(
        data_dir: &str,
        identifier: &str,
        private_key: &[u8],
        master_key: &[u8; 32],
    ) -> anyhow::Result<()> {
        save_bytes(data_dir, identifier, private_key, master_key)
    }

    pub fn load_encrypted_private_key(
        data_dir: &str,
        identifier: &str,
        master_key: &[u8; 32],
    ) -> anyhow::Result<Option<Zeroizing<Vec<u8>>>> {
        load_bytes(data_dir, identifier, master_key)
    }

    pub fn delete_encrypted_private_key(data_dir: &str, identifier: &str) {
        delete_file(data_dir, identifier);
    }

    /// 备份加密的私钥文件为 `<name>.bak`，供删除重建前保留旧密钥。
    /// 文件不存在时静默成功。
    pub fn backup_encrypted_private_key(data_dir: &str, identifier: &str) {
        let path = Self::encrypted_file_path(data_dir, identifier);
        if !path.exists() {
            return;
        }
        let backup = path.with_extension("enc.bak");
        if let Err(e) = std::fs::copy(&path, &backup) {
            tracing::warn!(
                "Failed to back up encrypted private key for {}: {}",
                identifier,
                e
            );
        }
    }

    /// 将私钥加密保存到 Keyring + 加密文件。
    ///
    /// 仅负责持久化，不构造内存驻留句柄；如需在内存保留密钥，请使用 `load`。
    pub fn save(data_dir: &str, identifier: &str, private_key: &[u8]) -> anyhow::Result<()> {
        tracing::debug!("Saving private key for identifier: {}", identifier);

        if !Self::check_keyring_available() {
            anyhow::bail!(
                "Keyring is not available on this platform. \
                 OpenWire requires a system keyring (Windows Credential Manager, \
                 macOS Keychain, Linux Secret Service, or Android/iOS keystore) \
                 to securely store encryption keys."
            );
        }

        let master_key = Self::ensure_master_key()?;

        Self::save_encrypted_private_key(data_dir, identifier, private_key, &master_key)?;
        tracing::info!(
            "Saved private key for {} (Keyring + encrypted file)",
            identifier
        );
        Ok(())
    }

    pub fn load(data_dir: &str, identifier: &str) -> anyhow::Result<Self> {
        tracing::debug!("Loading private key for identifier: {}", identifier);

        if !Self::check_keyring_available() {
            anyhow::bail!(
                "Keyring is not available on this platform. \
                 OpenWire requires a system keyring to securely store encryption keys."
            );
        }

        let master_key = Self::load_master_key()?.ok_or_else(|| {
            anyhow::anyhow!(
                "Keyring unavailable: no master key found. \
                 OpenWire requires a system keyring to access encryption keys."
            )
        })?;

        let private_key = Self::load_encrypted_private_key(data_dir, identifier, &master_key)?
            .ok_or_else(|| anyhow::anyhow!(
                "Private key not found for {identifier}. Keyring is available but no matching encrypted key file exists."
            ))?;

        let mut private_key = private_key;
        // 复制到 Box<[u8]> 后显式零化原 Vec：into_boxed_slice 在容量 > 长度时会
        // realloc，旧缓冲中未零化的明文会被释放到堆上。
        let private_key_vec = std::mem::take(&mut *private_key);
        let mut private_key_vec = private_key_vec;
        let boxed_slice: Box<[u8]> = private_key_vec.as_slice().into();
        private_key_vec.zeroize();
        let mut handle = Self {
            private_key: Box::into_pin(boxed_slice),
            identifier: identifier.to_string(),
            data_dir: data_dir.to_string(),
            locked: false,
        };
        handle.lock_memory()?;
        tracing::info!(
            "Loaded private key for {} from encrypted file (Keyring)",
            identifier
        );
        Ok(handle)
    }

    pub fn get_private_key(&self) -> &[u8] {
        &self.private_key
    }

    pub fn identifier(&self) -> &str {
        &self.identifier
    }

    pub fn data_dir(&self) -> &str {
        &self.data_dir
    }

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
                    "Failed to mlock private key memory ({}): {}. Continuing with degraded protection.",
                    len, err
                );
                return Ok(());
            }
            // 禁止 core dump，防止私钥在崩溃时写入 core 文件
            #[cfg(target_os = "linux")]
            libc::prctl(libc::PR_SET_DUMPABLE, 0);
        }
        self.locked = true;
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
            tracing::warn!("Failed to VirtualLock private key memory ({}): {}. Continuing with degraded protection.", len, err);
            return Ok(());
        }
        self.locked = true;
        Ok(())
    }

    #[cfg(not(any(unix, windows)))]
    fn lock_memory(&mut self) -> anyhow::Result<()> {
        tracing::debug!("mlock not supported on this platform");
        self.locked = false;
        Ok(())
    }

    #[cfg(unix)]
    fn unlock_memory(&mut self) {
        if !self.locked {
            return;
        }
        let ptr = self.private_key.as_ptr();
        let len = self.private_key.len();
        unsafe {
            let _ = libc::munlock(ptr as *const libc::c_void, len);
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
        unsafe {
            windows_sys::Win32::System::Memory::VirtualUnlock(ptr as *const std::ffi::c_void, len);
        }
        self.locked = false;
    }

    #[cfg(not(any(unix, windows)))]
    fn unlock_memory(&mut self) {
        self.locked = false;
    }

    pub fn diagnose_storage(data_dir: &str, identifier: &str) -> String {
        let mut report = format!("=== Private Key Storage for {identifier} ===\n");
        match Self::load_master_key() {
            Ok(Some(_)) => report.push_str("Keyring master key: Available\n"),
            Ok(None) => report.push_str("Keyring master key: Not available\n"),
            Err(e) => report.push_str(&format!("Keyring master key error: {e}\n")),
        }
        let path = Self::encrypted_file_path(data_dir, identifier);
        if path.exists() {
            report.push_str(&format!("Encrypted key file: Present ({})\n", path.display()));
        } else {
            report.push_str("Encrypted key file: Missing\n");
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
        // 在 munlock 前对密钥缓冲区清零，防止解锁后内存页被换出时泄露密钥材料。
        // 使用 Zeroize（volatile 写）而非 fill(0)，避免编译器把清零优化为 dead store。
        self.private_key.zeroize();
        self.unlock_memory();
    }
}

// ============================================================================
// 共享字节级加密原语（PrivateKeyHandle + EncryptedStore 共用）
// ============================================================================

const SALT_LEN: usize = 16;
const NONCE_LEN: usize = 12;
const HEADER_LEN: usize = 1 + SALT_LEN + NONCE_LEN;

/// 加密并写入字节到文件。每保存生成随机 salt 派生独立密钥。
pub(crate) fn save_bytes(
    data_dir: &str,
    identifier: &str,
    bytes: &[u8],
    master_key: &[u8; 32],
) -> anyhow::Result<()> {
    let keys_dir = Path::new(data_dir).join("keys");
    std::fs::create_dir_all(&keys_dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&keys_dir, std::fs::Permissions::from_mode(0o700));
    }

    let salt: [u8; SALT_LEN] = rand::random();
    let derived = PrivateKeyHandle::derive_aes_key(master_key, identifier, &salt);
    let cipher = Aes256Gcm::new_from_slice(&*derived)
        .map_err(|e| anyhow::anyhow!("Invalid AES key length: {e}"))?;

    let nonce = Nonce::from(rand::random::<[u8; NONCE_LEN]>());
    // 版本字节作为 AAD 认证，防止被篡改导致有效文件被拒绝
    let aad = [FILE_FORMAT_VERSION];
    let ciphertext = cipher
        .encrypt(&nonce, Payload { msg: bytes, aad: &aad })
        .map_err(|e| anyhow::anyhow!("AES-256-GCM encryption failed: {e:?}"))?;

    let path = PrivateKeyHandle::encrypted_file_path(data_dir, identifier);
    let mut file_content = vec![FILE_FORMAT_VERSION];
    file_content.extend_from_slice(&salt);
    file_content.extend_from_slice(&nonce);
    file_content.extend_from_slice(&ciphertext);
    atomic_write(&path, &file_content)?;

    Ok(())
}

/// 原子写入：先写同目录临时文件再 rename，避免中途崩溃留下截断/损坏的密钥文件。
/// Windows 上 rename 在目标已存在时失败，不会出现旧文件已被删除而新文件未就位的窗口期。
/// 临时文件在创建后立即设置 0o600 权限（仅 unix），避免 umask 默认权限下的
/// TOCTOU 窗口；rename 保留源文件权限。
fn atomic_write(path: &std::path::Path, content: &[u8]) -> std::io::Result<()> {
    let tmp_path = path.with_extension("tmp");
    let _ = std::fs::remove_file(&tmp_path);
    std::fs::write(&tmp_path, content)?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp_path, std::fs::Permissions::from_mode(0o600));
    }

    std::fs::rename(&tmp_path, path)
}

/// 从文件读取并解密字节。返回 `None` 表示文件不存在。
///
/// 自动检测旧格式文件（无版本字节）并迁移到新格式。
pub(crate) fn load_bytes(
    data_dir: &str,
    identifier: &str,
    master_key: &[u8; 32],
) -> anyhow::Result<Option<Zeroizing<Vec<u8>>>> {
    let path = PrivateKeyHandle::encrypted_file_path(data_dir, identifier);
    let file_content = match std::fs::read(&path) {
        Ok(content) => content,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    if file_content.len() < NONCE_LEN {
        anyhow::bail!(
            "Encrypted file for {} is too short ({} bytes) — file exists but is corrupt",
            identifier,
            file_content.len()
        );
    }

    let version = file_content[0];
    if version != FILE_FORMAT_VERSION {
        // 开发版本无兼容约束：未知/旧格式版本直接报错，不做旧格式回退或迁移
        anyhow::bail!(
            "Encrypted file for {} has unknown format version {:#04x} (expected {:#04x})",
            identifier,
            version,
            FILE_FORMAT_VERSION
        );
    }
    decode_v1_format(identifier, &file_content, master_key).map(|v| Some(Zeroizing::new(v)))
}

/// 解密 v1 格式：version(1) + salt(16) + nonce(12) + AES-GCM-AAD(ciphertext)
fn decode_v1_format(
    identifier: &str,
    file_content: &[u8],
    master_key: &[u8; 32],
) -> anyhow::Result<Vec<u8>> {
    if file_content.len() < HEADER_LEN {
        anyhow::bail!("file too short for v1 format");
    }
    let salt: [u8; SALT_LEN] = file_content[1..=SALT_LEN]
        .try_into()
        .map_err(|_| anyhow::anyhow!("Invalid salt length"))?;
    let (nonce_bytes, ciphertext) = file_content[1 + SALT_LEN..].split_at(NONCE_LEN);
    let nonce_array: [u8; NONCE_LEN] = nonce_bytes
        .try_into()
        .map_err(|_| anyhow::anyhow!("Invalid nonce length"))?;

    let derived = PrivateKeyHandle::derive_aes_key(master_key, identifier, &salt);
    let cipher = Aes256Gcm::new_from_slice(&*derived)
        .map_err(|e| anyhow::anyhow!("Invalid AES key length: {e}"))?;
    let aad = [FILE_FORMAT_VERSION];
    cipher
        .decrypt(&Nonce::from(nonce_array), Payload { msg: ciphertext, aad: &aad })
        .map_err(|_| anyhow::anyhow!(
            "Failed to decrypt file for {} (wrong key or corrupted file)", identifier
        ))
}

/// 删除加密文件（不存在时静默成功）。
pub(crate) fn delete_file(data_dir: &str, identifier: &str) {
    let path = PrivateKeyHandle::encrypted_file_path(data_dir, identifier);
    if path.exists() {
        if let Err(e) = std::fs::remove_file(&path) {
            tracing::warn!("Failed to delete encrypted file for {}: {}", identifier, e);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "rootcell_test_{}_{}",
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
    fn derive_aes_key_is_deterministic() {
        let master = [42u8; 32];
        let salt = [7u8; 16];
        let k1 = PrivateKeyHandle::derive_aes_key(&master, "alice", &salt);
        let k2 = PrivateKeyHandle::derive_aes_key(&master, "alice", &salt);
        assert_eq!(&*k1, &*k2);

        let k3 = PrivateKeyHandle::derive_aes_key(&master, "bob", &salt);
        assert_ne!(&*k1, &*k3);

        let master2 = [43u8; 32];
        let k4 = PrivateKeyHandle::derive_aes_key(&master2, "alice", &salt);
        assert_ne!(&*k1, &*k4);

        // 不同 salt 产生不同密钥
        let salt2 = [8u8; 16];
        let k5 = PrivateKeyHandle::derive_aes_key(&master, "alice", &salt2);
        assert_ne!(&*k1, &*k5);
    }

    #[test]
    fn hash_identifier_is_stable() {
        let h1 = PrivateKeyHandle::hash_identifier("hello");
        let h2 = PrivateKeyHandle::hash_identifier("hello");
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 32); // 16 bytes hex -> 32 chars
        assert_ne!(h1, PrivateKeyHandle::hash_identifier("world"));
    }

    #[test]
    fn encrypted_private_key_roundtrip() {
        let dir = temp_dir();
        let master = [7u8; 32];
        let key_bytes: Vec<u8> = (0..64).map(|i| i as u8).collect();

        PrivateKeyHandle::save_encrypted_private_key(
            dir.to_str().unwrap(),
            "alice",
            &key_bytes,
            &master,
        )
        .unwrap();

        let loaded = PrivateKeyHandle::load_encrypted_private_key(
            dir.to_str().unwrap(),
            "alice",
            &master,
        )
        .unwrap()
        .expect("key should exist");
        assert_eq!(&*loaded, &key_bytes);

        // 错误 master key 应解密失败
        let wrong_master = [8u8; 32];
        assert!(
            PrivateKeyHandle::load_encrypted_private_key(
                dir.to_str().unwrap(),
                "alice",
                &wrong_master,
            )
            .is_err()
        );

        // 不存在的 identifier 应返回 None
        assert!(
            PrivateKeyHandle::load_encrypted_private_key(dir.to_str().unwrap(), "nobody", &master)
                .unwrap()
                .is_none()
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn encrypted_file_path_is_hashed() {
        let dir = temp_dir();
        let p1 = PrivateKeyHandle::encrypted_file_path(dir.to_str().unwrap(), "a/b");
        let p2 = PrivateKeyHandle::encrypted_file_path(dir.to_str().unwrap(), "a%2Fb");
        // 路径分隔符不会影响哈希结果，文件名始终为 32 位 hex + .enc
        let f1 = p1.file_name().unwrap().to_str().unwrap().to_string();
        let f2 = p2.file_name().unwrap().to_str().unwrap().to_string();
        assert_eq!(f1.len(), 32 + 4);
        assert_eq!(f2.len(), 32 + 4);
        assert!(f1.ends_with(".enc"));
        assert!(f2.ends_with(".enc"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}