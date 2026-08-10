use aws_lc_rs::kem::{DecapsulationKey, ML_KEM_768};
use aws_lc_rs::signature::KeyPair;
use libp2p::identity;
use rootcell::store::EncryptedStore;
use zeroize::Zeroizing;

use crate::{coreconfig::CoreConfig, peerid_store::PeerIdConfig, storage};

/// 生成临时 PeerId（ed25519 密钥对）
///
/// 如果提供了 `PeerIdConfig`（持久化），则恢复存储的密钥对实现 PeerID 稳定；
/// 否则每次生成全新的密钥对（向后兼容，用于测试等场景）。
pub fn generate_temporary_peerid() -> crate::error::IdentityResult<identity::Keypair> {
    let keypair = identity::Keypair::generate_ed25519();
    Ok(keypair)
}

/// 从持久化配置恢复 PeerID，或生成新的
pub fn load_or_create_peerid(
    data_dir: &std::path::Path,
) -> (identity::Keypair, PeerIdConfig) {
    let config = PeerIdConfig::load_or_create(data_dir);
    match config.to_keypair() {
        Ok(kp) => {
            tracing::info!("Restored PeerID from persistent storage: {}", kp.public().to_peer_id());
            (kp, config)
        }
        Err(e) => {
            tracing::warn!("Failed to restore PeerID from config, generating new: {e}");
            // 删除加密存储中的损坏条目，避免 load_or_create 再次读取相同损坏数据
            let data_dir_str = data_dir.to_string_lossy();
            if let Ok(store) = EncryptedStore::open() {
                store.delete(&data_dir_str, crate::peerid_store::STORE_IDENTIFIER);
            }
            // 同时清理旧的明文文件（若存在）
            let config_path = PeerIdConfig::path(data_dir);
            if config_path.exists() {
                if let Err(e) = std::fs::remove_file(&config_path) {
                    tracing::warn!("Failed to remove corrupt PeerIdConfig file: {e}");
                }
            }
            let config = PeerIdConfig::load_or_create(data_dir);
            let kp = config
                .to_keypair()
                .expect("Freshly created PeerIdConfig should always yield a valid keypair");
            (kp, config)
        }
    }
}

/// 完整身份信息（ML-DSA 签名 + ML-KEM 封装密钥 + ML-DSA 私钥）
pub struct CompleteIdentity {
    /// ML-DSA 公钥（用于身份标识与消息验签）
    pub mldsa_public_key: Zeroizing<Vec<u8>>,
    /// ML-DSA 私钥（用于消息签名，已从 Keyring 加载）
    pub mldsa_private_key: Zeroizing<Vec<u8>>,
    /// ML-KEM 公钥（用于临时密钥交换）
    pub mlkem_public_key: Vec<u8>,
    /// ML-KEM 解封装密钥（会话级，缓存在内存中）
    pub mlkem_decap_key: DecapsulationKey,
}

/// 生成新的完整身份并设为当前身份
pub async fn generate_complete_identity(
    cfg: &CoreConfig,
) -> crate::error::IdentityResult<CompleteIdentity> {
    let pool = storage::pool().ok_or(crate::error::IdentityError::DatabaseNotInitialized)?;

    let (mldsa_public_key, mldsa_secret_key) = crate::signature::generate_mldsa_keypair()?;
    let identity_id = hex::encode(&mldsa_public_key);

    tracing::info!(
        "Generating new complete identity with ML-DSA signing key: {}..",
        &identity_id[..16]
    );

    let mldsa_secret_key = Zeroizing::new(mldsa_secret_key);
    let data_dir = cfg.data_dir.to_string_lossy().to_string();
    let identifier = format!("{}_mldsa", identity_id);

    rootcell::identity::PrivateKeyHandle::save(&data_dir, &identifier, &mldsa_secret_key)
        .map_err(|e| {
            crate::error::IdentityError::RootCellIdentityLoadFailed(Box::new(
                std::io::Error::other(e.to_string()),
            ))
        })?;

    tracing::info!("ML-DSA identity saved: {}", &identity_id[..16]);

    let decap_key = DecapsulationKey::generate(&ML_KEM_768)
        .map_err(crate::error::IdentityError::GenerateMlKemKeypairFailed)?;
    let encap_key = decap_key
        .encapsulation_key()
        .map_err(crate::error::IdentityError::GetEncapsulationKeyFailed)?;
    let mlkem_public_key = encap_key
        .key_bytes()
        .map_err(crate::error::IdentityError::SerializeMlKemPublicKeyFailed)?;

    storage::add_identity(pool, &identity_id).await?;
    storage::set_current_identity(pool, &identity_id).await?;

    tracing::info!(
        "Complete identity generation completed successfully: {}..",
        &identity_id[..16]
    );

    Ok(CompleteIdentity {
        mldsa_public_key: Zeroizing::new(mldsa_public_key),
        mldsa_private_key: mldsa_secret_key,
        mlkem_public_key: mlkem_public_key.as_ref().to_vec(),
        mlkem_decap_key: decap_key,
    })
}

/// 加载当前身份；若不存在或加载失败则生成新身份
pub async fn load_or_generate_complete_identity(
    cfg: &CoreConfig,
) -> crate::error::IdentityResult<CompleteIdentity> {
    let pool = storage::pool().ok_or(crate::error::IdentityError::DatabaseNotInitialized)?;
    tracing::info!("Loading or generating complete identity");

    if let Some(identity_id) = storage::get_current_identity(pool).await? {
        tracing::info!(
            "Found current identity in database: {}..",
            &identity_id[..16]
        );

        let data_dir = cfg.data_dir.to_string_lossy().to_string();
        match rootcell::identity::PrivateKeyHandle::load(
            &data_dir,
            &format!("{}_mldsa", identity_id),
        ) {
            Ok(mldsa_handle) => {
                tracing::info!("Loaded ML-DSA identity: {}..", &identity_id[..16]);

                let mldsa_private_key = Zeroizing::new(mldsa_handle.get_private_key().to_vec());
                let mldsa_public_key =
                    extract_public_key_from_private(mldsa_handle.get_private_key(), true)?;

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
                    mldsa_private_key,
                    mlkem_public_key: mlkem_public_key.as_ref().to_vec(),
                    mlkem_decap_key: decap_key,
                })
            }
            Err(_) => {
                tracing::error!(
                    "Failed to load ML-DSA private key for {}. Generating new identity.",
                    &identity_id[..16]
                );

                if let Err(del_err) =
                    storage::delete_identity(pool, &cfg.data_dir, &identity_id).await
                {
                    tracing::warn!(
                        "Failed to delete invalid identity {}..: {}",
                        &identity_id[..16],
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

/// 从私钥字节中提取公钥
pub fn extract_public_key_from_private(
    private_key_bytes: &[u8],
    is_mldsa: bool,
) -> crate::error::IdentityResult<Vec<u8>> {
    if is_mldsa {
        use aws_lc_rs::unstable::signature::ML_DSA_65_SIGNING;
        use aws_lc_rs::unstable::signature::PqdsaKeyPair;
        let key_pair = PqdsaKeyPair::from_raw_private_key(&ML_DSA_65_SIGNING, private_key_bytes)
            .map_err(crate::error::IdentityError::ParseMlDsaPrivateKeyFailed)?;
        Ok(key_pair.public_key().as_ref().to_vec())
    } else {
        Err(crate::error::IdentityError::MlKemKeyNotExtractable)
    }
}
