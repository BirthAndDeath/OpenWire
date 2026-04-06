use libp2p::identity;

use crate::{coreconfig::CoreConfig, storage};

/// 生成新身份并保存到存储
async fn generate_and_save_identity(cfg: &CoreConfig) -> anyhow::Result<identity::Keypair> {
    let pool = storage::pool().ok_or_else(|| anyhow::anyhow!("Database not initialized"))?;

    let keypair = identity::Keypair::generate_ed25519();
    let peer_id = keypair.public().to_peer_id();
    let peer_id_str = peer_id.to_string();
    let public_key_bytes = keypair.public().encode_protobuf();
    let private_key_bytes = keypair.to_protobuf_encoding()?;

    tracing::info!("Generating new identity: {}", peer_id_str);
    
    // 第一步：保存私钥到文件系统/keyring（遵循"先落盘文件资源"原则）
    match storage::set_private_key(&cfg.data_dir, &peer_id_str, &private_key_bytes) {
        Ok(used_file) => {
            if used_file {
                tracing::info!("Private key saved to local file for {}", peer_id_str);
            } else {
                tracing::info!("Private key saved to keyring for {}", peer_id_str);
            }
        }
        Err(e) => {
            tracing::error!("Failed to save private key for {}: {}", peer_id_str, e);
            anyhow::bail!("Failed to save private key: {}", e);
        }
    }
    
    // 第二步：添加身份到数据库
    storage::add_identity(pool, &peer_id_str, &public_key_bytes).await?;
    tracing::debug!("Added identity to database: {}", peer_id_str);
    
    // 第三步：设置为当前身份
    storage::set_current_identity(pool, &peer_id_str).await?;
    tracing::info!("Set current identity to: {}", peer_id_str);

    tracing::info!("Identity generation completed successfully: {}", peer_id_str);
    Ok(keypair)
}

/// 生成新身份（公共接口）
pub async fn generate_identity(cfg: &CoreConfig) -> anyhow::Result<identity::Keypair> {
    generate_and_save_identity(cfg).await
}

/// 加载或生成身份
pub async fn load_or_generate_identity(cfg: &CoreConfig) -> anyhow::Result<identity::Keypair> {
    let pool = storage::pool().ok_or_else(|| anyhow::anyhow!("Database not initialized"))?;
    tracing::info!(
        "Loading or generating identity, data_dir: {:?}",
        cfg.data_dir
    );

    if let Some((peer_id_str, public_key_bytes)) = storage::get_current_identity(pool).await? {
        tracing::info!("Found current identity in database: {}", peer_id_str);
        
        // 诊断私钥存储状态
        let diagnosis = storage::diagnose_private_key_storage(&cfg.data_dir, &peer_id_str);
        tracing::debug!("Private key storage diagnosis:\n{}", diagnosis);

        // 尝试从存储加载私钥
        match storage::get_private_key(&cfg.data_dir, &peer_id_str) {
            Ok(private_key_bytes) => {
                // 显式标注类型以确保正确推断为 Vec<u8>
                let private_key_bytes: Vec<u8> = private_key_bytes;
                tracing::info!("Successfully loaded private key for {}", peer_id_str);
                // 重建Keypair
                let keypair = identity::Keypair::from_protobuf_encoding(&private_key_bytes)?;
                // 验证peer_id和public_key匹配
                if keypair.public().to_peer_id().to_string() != peer_id_str
                    || keypair.public().encode_protobuf() != public_key_bytes
                {
                    tracing::warn!(
                        "Stored peer_id or public_key does not match reconstructed keypair"
                    );
                    anyhow::bail!(
                        "Identity mismatch for {}. Please clear database or restore correct private key.",
                        peer_id_str
                    );
                } else {
                    tracing::info!("Identity loaded successfully: {}", peer_id_str);
                    Ok(keypair)
                }
            }
            Err(e) => {
                tracing::error!(
                    "Failed to load private key for {}: {}. The existing identity record will be removed and a new one generated.",
                    peer_id_str,
                    e
                );
                // 私钥丢失时，删除无效的 identity 记录，然后生成新身份
                // 这样可以避免数据库中积累无效的 identity 记录
                if let Err(del_err) =
                    storage::delete_identity(pool, &cfg.data_dir, &peer_id_str).await
                {
                    tracing::warn!(
                        "Failed to delete invalid identity {}: {}",
                        peer_id_str,
                        del_err
                    );
                }
                generate_and_save_identity(cfg).await
            }
        }
    } else {
        tracing::debug!("No current identity found in database, generating new identity");
        // 生成新身份
        generate_and_save_identity(cfg).await
    }
}
