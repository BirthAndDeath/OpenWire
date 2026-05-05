use aws_lc_rs::kem::{DecapsulationKey, ML_KEM_768};
use aws_lc_rs::signature::KeyPair;
use libp2p::identity;
use zeroize::Zeroizing;

use crate::{coreconfig::CoreConfig, storage};

/// 生成临时PeerID（Ed25519密钥对，仅用于传输层）
pub fn generate_temporary_peerid() -> crate::error::IdentityResult<identity::Keypair> {
    let keypair = identity::Keypair::generate_ed25519();
    Ok(keypair)
}

/// 完整身份信息，包含 ML-DSA 公钥、ML-KEM 公钥和 ML-KEM 解封装密钥对象
///
/// # 设计说明
/// 返回 DecapsulationKey 对象而非私钥字节，是因为 aws-lc-rs 的
/// `DecapsulationKey::key_bytes()` 输出格式与 `DecapsulationKey::new()` 输入格式不兼容
/// （已知的库限制），无法通过序列化/反序列化私钥字节来重建 DecapsulationKey。
pub struct CompleteIdentity {
    /// ML-DSA 公钥（持久化身份标识）
    pub mldsa_public_key: Zeroizing<Vec<u8>>,
    /// ML-KEM 公钥（临时会话密钥交换）
    pub mlkem_public_key: Vec<u8>,
    /// ML-KEM 解封装密钥对象（由 ChatCore 缓存，避免序列化/反序列化问题）
    pub mlkem_decap_key: DecapsulationKey,
}

/// 生成完整的身份系统：ML-DSA（持久化签名）+ ML-KEM（临时密钥交换）+ PeerID（临时传输）
///
/// # 设计说明
/// - **ML-DSA 公钥**：持久化身份标识，用于签名验证，唯一标识联系人
/// - **ML-KEM 公钥**：临时密钥交换，每次会话重新生成，不持久化存储
/// - **PeerID（Ed25519）**：临时传输层标识，每次启动重新生成
pub async fn generate_complete_identity(
    cfg: &CoreConfig,
) -> crate::error::IdentityResult<CompleteIdentity> {
    let pool = storage::pool().ok_or(crate::error::IdentityError::DatabaseNotInitialized)?;

    // 1. 生成 ML-DSA 密钥对（持久化身份签名）
    let (mldsa_public_key, mldsa_secret_key) = crate::signature::generate_mldsa_keypair()?;
    // 使用 ML-DSA 公钥的 hex 作为内部身份 ID
    let identity_id = hex::encode(&mldsa_public_key);

    tracing::info!(
        "Generating new complete identity with ML-DSA signing key: {}",
        identity_id
    );

    // 2. 保存 ML-DSA 私钥到安全存储
    // 使用 Zeroizing 包装临时副本，确保保存后 drop 时自动清零内存
    let mldsa_secret_key = Zeroizing::new(mldsa_secret_key);
    let data_dir = cfg.data_dir.to_string_lossy().to_string();
    let (_handle, _backend) = rootcell::identity::PrivateKeyHandle::save(
        &data_dir,
        &format!("{}_mldsa", identity_id),
        &mldsa_secret_key,
    )
    .map_err(|e| {
        crate::error::IdentityError::RootCellIdentityLoadFailed(Box::new(std::io::Error::new(
            std::io::ErrorKind::Other,
            e.to_string(),
        )))
    })?;

    tracing::info!("ML-DSA private key saved for {}", identity_id);

    // Zeroizing 的 drop 会自动清零内存，无需手动 drop

    // 3. 生成临时 ML-KEM 密钥对（用于密钥交换/加密，不持久化存储）
    let decap_key = DecapsulationKey::generate(&ML_KEM_768)
        .map_err(crate::error::IdentityError::GenerateMlKemKeypairFailed)?;
    let encap_key = decap_key
        .encapsulation_key()
        .map_err(crate::error::IdentityError::GetEncapsulationKeyFailed)?;
    let mlkem_public_key = encap_key
        .key_bytes()
        .map_err(crate::error::IdentityError::SerializeMlKemPublicKeyFailed)?;

    // 4. 添加身份到数据库（以 ML-DSA 公钥 hex 为 identity_id）
    storage::add_identity(pool, &identity_id).await?;
    storage::set_current_identity(pool, &identity_id).await?;

    tracing::info!("Set current identity to: {}", identity_id);
    tracing::info!(
        "Complete identity generation completed successfully: {}",
        identity_id
    );

    Ok(CompleteIdentity {
        mldsa_public_key: Zeroizing::new(mldsa_public_key),
        mlkem_public_key: mlkem_public_key.as_ref().to_vec(),
        mlkem_decap_key: decap_key,
    })
}

/// 加载或生成完整身份
pub async fn load_or_generate_complete_identity(
    cfg: &CoreConfig,
) -> crate::error::IdentityResult<CompleteIdentity> {
    let pool = storage::pool().ok_or(crate::error::IdentityError::DatabaseNotInitialized)?;
    tracing::info!("Loading or generating complete identity");

    // 尝试从数据库加载当前身份信息（ML-DSA identity_id）
    if let Some(identity_id) = storage::get_current_identity(pool).await? {
        tracing::info!("Found current identity in database: {}", identity_id);

        // 诊断私钥存储状态
        let mldsa_diagnosis = rootcell::identity::PrivateKeyHandle::diagnose_storage(&format!(
            "{}_mldsa",
            identity_id
        ));
        tracing::debug!("ML-DSA private key storage diagnosis:\n{}", mldsa_diagnosis);

        // 尝试从存储加载 ML-DSA 私钥（只调用一次 Keyring，避免重复 Keyring 访问）
        let data_dir = cfg.data_dir.to_string_lossy().to_string();
        match rootcell::identity::PrivateKeyHandle::load(
            &data_dir,
            &format!("{}_mldsa", identity_id),
            cfg.passwd.as_deref(),
        ) {
            Ok(mldsa_handle) => {
                tracing::info!("Successfully loaded ML-DSA private key for {}", identity_id);

                // 从加载的私钥 Handle 中提取 ML-DSA 公钥
                let mldsa_public_key =
                    extract_public_key_from_private(mldsa_handle.get_private_key(), true)?;

                // 生成新的临时 ML-KEM 密钥对（每次会话重新生成）
                // 直接生成 DecapsulationKey 对象，避免序列化/反序列化问题
                let decap_key = DecapsulationKey::generate(&ML_KEM_768)
                    .map_err(crate::error::IdentityError::GenerateMlKemKeypairFailed)?;
                let encap_key = decap_key
                    .encapsulation_key()
                    .map_err(crate::error::IdentityError::GetEncapsulationKeyFailed)?;
                let mlkem_public_key = encap_key
                    .key_bytes()
                    .map_err(crate::error::IdentityError::SerializeMlKemPublicKeyFailed)?;

                tracing::info!("Generated new ephemeral ML-KEM keypair for session");

                Ok(CompleteIdentity {
                    mldsa_public_key: Zeroizing::new(mldsa_public_key),
                    mlkem_public_key: mlkem_public_key.as_ref().to_vec(),
                    mlkem_decap_key: decap_key,
                })
            }
            Err(_) => {
                tracing::error!(
                    "Failed to load ML-DSA private key for {}. Generating new identity.",
                    identity_id
                );

                // 删除无效的身份记录
                if let Err(del_err) =
                    storage::delete_identity(pool, &cfg.data_dir, &identity_id).await
                {
                    tracing::warn!(
                        "Failed to delete invalid identity {}: {}",
                        identity_id,
                        del_err
                    );
                }

                generate_complete_identity(cfg).await
            }
        }
    } else {
        tracing::debug!("No current identity found in database, generating new identity");
        generate_complete_identity(cfg).await
    }
}

/// 从私钥字节提取公钥
pub fn extract_public_key_from_private(
    private_key_bytes: &[u8],
    is_mldsa: bool,
) -> crate::error::IdentityResult<Vec<u8>> {
    if is_mldsa {
        // 使用 ML-DSA 65 从原始私钥提取公钥
        use aws_lc_rs::unstable::signature::ML_DSA_65_SIGNING;
        use aws_lc_rs::unstable::signature::PqdsaKeyPair;
        let key_pair = PqdsaKeyPair::from_raw_private_key(&ML_DSA_65_SIGNING, private_key_bytes)
            .map_err(crate::error::IdentityError::ParseMlDsaPrivateKeyFailed)?;
        Ok(key_pair.public_key().as_ref().to_vec())
    } else {
        Err(crate::error::IdentityError::MlKemKeyNotExtractable)
    }
}
