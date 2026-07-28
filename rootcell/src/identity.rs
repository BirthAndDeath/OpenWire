use std::path::Path;
use std::sync::LazyLock;

use anyhow;
use tracing;
use zeroize::Zeroizing;
use hkdf::Hkdf;
use sha2::Sha256;

#[cfg(unix)]
use libc;

const KEYRING_SERVICE: &str = "rootcell";
const MASTER_KEY_IDENTIFIER: &str = "rc_master";

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
            tracing::warn!("iOS Keychain store initialization failed");
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

fn ensure_keyring_init() {
    LazyLock::force(&KEYRING_INIT);
}

/// 初始化当前平台的 keyring store（强制立即执行，非惰性）。
///
/// 桌面/iOS 平台无需手动调用，`check_keyring_available` 等函数会自动触发惰性初始化。
/// Android 平台需在 JNI 主线程（JVM 上下文就绪后）显式调用此函数，确保 keyring
/// 在正确的线程上完成初始化。
pub fn setup_default_keyring() {
    LazyLock::force(&KEYRING_INIT);
}

pub struct PrivateKeyHandle {
    private_key: Zeroizing<Vec<u8>>,
    identifier: String,
    data_dir: String,
    locked: bool,
}

impl PrivateKeyHandle {
    pub fn generate_and_save_master_key() -> anyhow::Result<[u8; 32]> {
        let key: [u8; 32] = rand::random();
        let key_hex = hex::encode(key);
        match keyring::Entry::new(KEYRING_SERVICE, MASTER_KEY_IDENTIFIER) {
            Ok(entry) => {
                entry.set_password(&key_hex)?;
                tracing::info!("Generated and saved master key to Keyring");
                Ok(key)
            }
            Err(e) => {
                anyhow::bail!("Failed to create Keyring entry for master key: {e}")
            }
        }
    }

    pub fn load_master_key() -> anyhow::Result<Option<Zeroizing<[u8; 32]>>> {
        match keyring::Entry::new(KEYRING_SERVICE, MASTER_KEY_IDENTIFIER) {
            Ok(entry) => match entry.get_password() {
                Ok(pwd) if !pwd.trim().is_empty() => {
                    let decoded = Zeroizing::new(hex::decode(pwd.trim())?);
                    if decoded.len() == 32 {
                        let mut key = Zeroizing::new([0u8; 32]);
                        key.copy_from_slice(&decoded);
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
                    tracing::debug!("Failed to get master key from Keyring: {e}");
                    Ok(None)
                }
            },
            Err(e) => {
                tracing::debug!("Failed to create Keyring entry for master key: {e}");
                Ok(None)
            }
        }
    }

    pub fn has_master_key() -> bool {
        Self::load_master_key().ok().flatten().is_some()
    }

    /// 使用 HKDF 从 master key 派生出每个身份专用的 AES 密钥
    fn derive_aes_key(master_key: &[u8; 32], identifier: &str) -> [u8; 32] {
        let hk = Hkdf::<Sha256>::new(Some(b"openwire-key-derivation"), master_key);
        let mut aes_key = [0u8; 32];
        hk.expand(identifier.as_bytes(), &mut aes_key)
            .expect("HKDF expand should not fail with valid output length");
        aes_key
    }

    pub fn check_keyring_available() -> bool {
        ensure_keyring_init();
        keyring::Entry::new(KEYRING_SERVICE, "test_availability").is_ok()
    }

    pub fn delete_master_key() -> anyhow::Result<()> {
        let entry = keyring::Entry::new(KEYRING_SERVICE, MASTER_KEY_IDENTIFIER)?;
        match entry.get_password() {
            Ok(_) => {
                entry.delete_credential()?;
                tracing::info!("Deleted master key from Keyring");
                Ok(())
            }
            Err(keyring::Error::NoEntry) => {
                tracing::debug!("Master key not found in Keyring, skipping deletion");
                Ok(())
            }
            Err(e) => {
                tracing::error!("Failed to check master key in Keyring: {e:?}");
                Err(anyhow::anyhow!("Keyring 不可用，无法删除 master key: {e}"))
            }
        }
    }

    fn hash_identifier(identifier: &str) -> String {
        let hash = blake3::hash(identifier.as_bytes());
        hex::encode(&hash.as_bytes()[..16])
    }

    fn encrypted_file_path(data_dir: &str, identifier: &str) -> std::path::PathBuf {
        let short_name = Self::hash_identifier(identifier);
        Path::new(data_dir).join("keys").join(format!("{}.enc", short_name))
    }

    pub fn save_encrypted_private_key(
        data_dir: &str,
        identifier: &str,
        private_key: &[u8],
        master_key: &[u8; 32],
    ) -> anyhow::Result<()> {
        use aes_gcm::aead::{Aead, KeyInit};
        use aes_gcm::{Aes256Gcm, Key, Nonce};

        // 安全校验：规范化路径防止 ../ 遍历，但不改变写入路径
        let data_path = Path::new(data_dir);
        let _ = data_path.canonicalize()
            .map_err(|e| anyhow::anyhow!("无效的 data_dir '{}': {}", data_path.display(), e))?;
        let keys_dir = data_path.join("keys");
        std::fs::create_dir_all(&keys_dir)?;

        // 使用 HKDF 派生出每个身份专用的 AES 密钥
        let derived = Self::derive_aes_key(master_key, identifier);
        let key = Key::<Aes256Gcm>::from(derived);
        let cipher = Aes256Gcm::new(&key);

        let nonce = Nonce::from(rand::random::<[u8; 12]>());
        let ciphertext = cipher
            .encrypt(&nonce, private_key)
            .map_err(|e| anyhow::anyhow!("AES-256-GCM encryption failed: {e:?}"))?;

        let path = Self::encrypted_file_path(data_dir, identifier);
        let mut file_content = nonce.to_vec();
        file_content.extend_from_slice(&ciphertext);
        std::fs::write(&path, file_content)?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Ok(metadata) = std::fs::metadata(&path) {
                let mut perms = metadata.permissions();
                perms.set_mode(0o600);
                let _ = std::fs::set_permissions(&path, perms);
            }
        }

        Ok(())
    }

    pub fn load_encrypted_private_key(
        data_dir: &str,
        identifier: &str,
        master_key: &[u8; 32],
    ) -> anyhow::Result<Option<Vec<u8>>> {
        use aes_gcm::aead::{Aead, KeyInit};
        use aes_gcm::{Aes256Gcm, Key, Nonce};

        let path = Self::encrypted_file_path(data_dir, identifier);
        let file_content = match std::fs::read(&path) {
            Ok(content) => content,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        if file_content.len() < 12 {
            tracing::warn!(
                "Encrypted key file for {} is too short ({} bytes), ignoring",
                identifier,
                file_content.len()
            );
            return Ok(None);
        }

        let (nonce_bytes, ciphertext) = file_content.split_at(12);
        let nonce_array: [u8; 12] = nonce_bytes
            .try_into()
            .map_err(|_| anyhow::anyhow!("Invalid nonce length"))?;
        let nonce = Nonce::from(nonce_array);
        // 使用 HKDF 派生出每个身份专用的 AES 密钥
        let derived = Self::derive_aes_key(master_key, identifier);
        let key = Key::<Aes256Gcm>::from(derived);
        let cipher = Aes256Gcm::new(&key);

        match cipher.decrypt(&nonce, ciphertext) {
            Ok(plaintext) => return Ok(Some(plaintext)),
            Err(_) => {
                // 回退到旧格式（无 HKDF）：直接用 master_key 作为 AES 密钥
                let old_key = Key::<Aes256Gcm>::from(*master_key);
                let old_cipher = Aes256Gcm::new(&old_key);
                match old_cipher.decrypt(&nonce, ciphertext) {
                    Ok(plaintext) => {
                        tracing::info!("以旧格式解密身份 {} 成功，自动迁移到 HKDF 格式", identifier);
                        if let Err(e) = Self::save_encrypted_private_key(data_dir, identifier, &plaintext, master_key) {
                            tracing::warn!("迁移身份 {} 到 HKDF 格式失败: {e}", identifier);
                        }
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
        }
    }

    pub fn delete_encrypted_private_key(data_dir: &str, identifier: &str) {
        let path = Self::encrypted_file_path(data_dir, identifier);
        if path.exists() {
            if let Err(e) = std::fs::remove_file(&path) {
                tracing::warn!(
                    "Failed to delete encrypted key file for {}: {}",
                    identifier,
                    e
                );
            }
        }
    }

    pub fn save(data_dir: &str, identifier: &str, private_key: &[u8]) -> anyhow::Result<Self> {
        tracing::debug!("Saving private key for identifier: {}", identifier);

        if !Self::check_keyring_available() {
            anyhow::bail!(
                "Keyring is not available on this platform. \
                 OpenWire requires a system keyring (Windows Credential Manager, \
                 macOS Keychain, Linux Secret Service, or Android/iOS keystore) \
                 to securely store encryption keys."
            );
        }

        let master_key = match Self::load_master_key()? {
            Some(key) => key,
            None => {
                tracing::info!("No master key found in Keyring, generating new one");
                let raw_key = Self::generate_and_save_master_key()?;
                Zeroizing::new(raw_key)
            }
        };

        Self::save_encrypted_private_key(data_dir, identifier, private_key, &master_key)?;

        let mut handle = Self {
            private_key: Zeroizing::new(private_key.to_vec()),
            identifier: identifier.to_string(),
            data_dir: data_dir.to_string(),
            locked: false,
        };
        handle.lock_memory()?;
        tracing::info!(
            "Saved private key for {} (Keyring + encrypted file)",
            identifier
        );
        Ok(handle)
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

        let mut handle = Self {
            private_key: Zeroizing::new(private_key),
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

    pub fn diagnose_storage(identifier: &str) -> String {
        let mut report = format!("=== Private Key Storage for {identifier} ===\n");
        match Self::load_master_key() {
            Ok(Some(_)) => report.push_str("Keyring master key: Available\n"),
            Ok(None) => report.push_str("Keyring master key: Not available\n"),
            Err(e) => report.push_str(&format!("Keyring master key error: {e}\n")),
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
        self.unlock_memory();
    }
}
