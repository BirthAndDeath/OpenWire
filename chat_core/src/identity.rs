use aws_lc_rs::kem::{DecapsulationKey, ML_KEM_768};
use libp2p::identity;

use crate::{coreconfig::CoreConfig, storage};

/// 生成ML-KEM密钥对
fn generate_mlkem_keypair() -> anyhow::Result<(Vec<u8>, Vec<u8>)> {
    let decap_key = DecapsulationKey::generate(&ML_KEM_768)
        .map_err(|e| anyhow::anyhow!("Failed to generate ML-KEM keypair: {:?}", e))?;

    let encap_key = decap_key
        .encapsulation_key()
        .map_err(|e| anyhow::anyhow!("Failed to get encapsulation key: {:?}", e))?;

    let public_key = encap_key
        .key_bytes()
        .map_err(|e| anyhow::anyhow!("Failed to serialize public key: {:?}", e))?;
    let secret_key = decap_key
        .key_bytes()
        .map_err(|e| anyhow::anyhow!("Failed to serialize private key: {:?}", e))?;

    Ok((public_key.as_ref().to_vec(), secret_key.as_ref().to_vec()))
}

/// 生成临时PeerID（Ed25519密钥对）
pub fn generate_temporary_peerid() -> anyhow::Result<identity::Keypair> {
    let keypair = identity::Keypair::generate_ed25519();
    Ok(keypair)
}

/// 生成ML-KEM身份并保存
pub async fn generate_mlkem_identity(cfg: &CoreConfig) -> anyhow::Result<Vec<u8>> {
    let pool = storage::pool().ok_or_else(|| anyhow::anyhow!("Database not initialized"))?;

    // 生成ML-KEM密钥对
    let (public_key, secret_key) = generate_mlkem_keypair()?;
    let identity_id = hex::encode(&public_key);

    tracing::info!("Generating new ML-KEM identity: {}", identity_id);

    // 使用rootcell的PrivateKeyHandle保存私钥
    let (_handle, backend) = rootcell::identity::PrivateKeyHandle::save(
        &cfg.data_dir,
        &identity_id,
        &secret_key,
    )?;

    match backend {
        rootcell::identity::StorageBackend::Keyring => {
            tracing::info!("ML-KEM private key saved to keyring for {}", identity_id);
        }
        rootcell::identity::StorageBackend::EncryptedFile => {
            tracing::info!(
                "ML-KEM private key saved to encrypted file for {} (keyring unavailable)",
                identity_id
            );
        }
    }

    // 私钥已在PrivateKeyHandle中管理，这里立即清零临时副本
    drop(secret_key);

    // 添加ML-KEM身份到数据库
    storage::add_mlkem_identity(pool, &identity_id, &public_key).await?;
    tracing::debug!("Added ML-KEM identity to database: {}", identity_id);

    // 设置为当前身份
    storage::set_current_mlkem_identity(pool, &identity_id).await?;
    tracing::info!("Set current ML-KEM identity to: {}", identity_id);

    tracing::info!(
        "ML-KEM identity generation completed successfully: {}",
        identity_id
    );
    Ok(public_key)
}

/// 加载或生成ML-KEM身份
pub async fn load_or_generate_mlkem_identity(cfg: &CoreConfig) -> anyhow::Result<Vec<u8>> {
    let pool = storage::pool().ok_or_else(|| anyhow::anyhow!("Database not initialized"))?;
    tracing::info!(
        "Loading or generating ML-KEM identity, data_dir: {:?}",
        cfg.data_dir
    );

    if let Some((identity_id, public_key)) = storage::get_current_mlkem_public_key(pool).await? {
        tracing::info!("Found current ML-KEM identity in database: {}", identity_id);

        // 诊断私钥存储状态
        let diagnosis =
            rootcell::identity::PrivateKeyHandle::diagnose_storage(&cfg.data_dir, &identity_id);
        tracing::debug!("ML-KEM private key storage diagnosis:\n{}", diagnosis);

        // 尝试从存储加载私钥（使用PrivateKeyHandle）
        match rootcell::identity::PrivateKeyHandle::load(&cfg.data_dir, &identity_id) {
            Ok(handle) => {
                tracing::info!(
                    "Successfully loaded ML-KEM private key for {} (backend: {:?})",
                    identity_id,
                    handle.backend()
                );
                // Handle会在drop时自动清理和解锁内存
                drop(handle);
                Ok(public_key)
            }
            Err(e) => {
                tracing::error!(
                    "Failed to load ML-KEM private key for {}: {}. The existing identity record will be removed and a new one generated.",
                    identity_id,
                    e
                );
                // 私钥丢失时，删除无效的身份记录，然后生成新身份
                if let Err(del_err) =
                    storage::delete_mlkem_identity(pool, &cfg.data_dir, &identity_id).await
                {
                    tracing::warn!(
                        "Failed to delete invalid ML-KEM identity {}: {}",
                        identity_id,
                        del_err
                    );
                }
                generate_mlkem_identity(cfg).await
            }
        }
    } else {
        tracing::debug!("No current ML-KEM identity found in database, generating new identity");
        // 生成新身份
        generate_mlkem_identity(cfg).await
    }
}
