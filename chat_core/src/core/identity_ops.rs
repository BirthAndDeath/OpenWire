use zeroize::Zeroizing;

use crate::{core::ChatCore, crypto, identity, p2p, storage};

impl ChatCore {
    pub(crate) async fn generate_identity(&mut self) {
        // 使用统一的完整身份生成逻辑（ML-DSA + 临时 ML-KEM）
        let temp_cfg = crate::coreconfig::CoreConfig {
            data_dir: self.data_dir.clone(),
            ..Default::default()
        };

        match crate::identity::generate_complete_identity(&temp_cfg).await {
            Ok((mldsa_public_key, mlkem_public_key)) => {
                let mldsa_public_key = mldsa_public_key.to_vec();
                let mldsa_id = hex::encode(&mldsa_public_key);
                let mlkem_id = hex::encode(&mlkem_public_key);
                tracing::info!(
                    "Generated new identity: ML-DSA={}, ML-KEM={} (ephemeral)",
                    &mldsa_id[..16],
                    &mlkem_id[..16]
                );

                // 更新核心身份字段，使新身份立即生效
                self.mldsa_pubkey_hex = Some(mldsa_id.clone());
                self.mldsa_identity_id = Some(mldsa_id.clone());
                self.mlkem_pubkey_hex = Some(mlkem_id.clone());

                // 从存储加载新身份的 ML-DSA 私钥并缓存到内存
                // 使用 Zeroizing 包装，确保私钥在内存中可被自动清零
                match rootcell::identity::PrivateKeyHandle::load(
                    &self.data_dir.to_string_lossy(),
                    &format!("{}_mldsa", mldsa_id),
                    None,
                ) {
                    Ok(handle) => {
                        self.mldsa_private_key =
                            Some(Zeroizing::new(handle.get_private_key().to_vec()));
                    }
                    Err(e) => {
                        tracing::error!("Failed to cache ML-DSA private key for new identity: {e}");
                    }
                }

                // 生成新的临时 PeerID 并重新初始化 swarm
                match identity::generate_temporary_peerid() {
                    Ok(keypair) => {
                        let peer_id = keypair.public().to_peer_id();
                        match p2p::swarm_init(&self.data_dir, keypair.clone()) {
                            Ok(p2p::SwarmWithValidator { swarm, validator }) => {
                                self.swarm = swarm;
                                self.validator = validator;
                                self.identity_keypair = keypair;
                                self.current_peer_id = Some(peer_id);

                                // 立即发布新身份到 DHT（使用缓存的连接）
                                if let Ok(store) = self.get_dht_store() {
                                    let _ = store.set_pubkey_peerid(&mldsa_id, &peer_id);
                                    let _ = store.set_mlkem_pubkey(&mldsa_id, &mlkem_id);
                                }

                                tracing::info!(
                                    "Swarm reinitialized for new identity, PeerID={}",
                                    peer_id
                                );
                            }
                            Err(e) => {
                                tracing::error!(
                                    "Failed to reinitialize swarm for new identity: {e}"
                                );
                            }
                        }
                    }
                    Err(e) => {
                        tracing::error!(
                            "Failed to generate temporary PeerID for new identity: {e}"
                        );
                    }
                }

                // 更新数据库中的当前身份标记（自动切换到新生成的身份）
                if let Some(pool) = storage::pool() {
                    let _ = storage::set_current_identity(pool, &mldsa_id).await;
                }

                let msg = format!("已生成并切换到新身份: {}..", &mldsa_id[..16]);
                self.send_log_mpsc(msg).await;
            }
            Err(e) => {
                tracing::error!("Failed to generate identity: {e}");
                let msg = format!("生成身份失败: {}", e);
                self.send_warning_mpsc(msg).await;
            }
        }
    }

    /// 选择当前身份（通过 ML-DSA identity_id）
    /// 运行时切换：重新加载密钥、重新生成 ML-KEM/PeerID、重新初始化 swarm
    pub(crate) async fn select_identity(&mut self, identity_id: String) {
        // 1. 更新数据库中的当前身份标记
        if let Some(pool) = storage::pool() {
            if let Err(e) = storage::set_current_identity(pool, &identity_id).await {
                tracing::error!("Failed to set current identity in DB: {e}");
                let msg = format!("切换身份失败: {}", e);
                self.send_warning_mpsc(msg).await;
                return;
            }
        } else {
            tracing::error!("Database pool not available for identity switch");
            return;
        }

        // 2. 加载新身份的 ML-DSA 私钥并提取公钥
        let mldsa_handle = match rootcell::identity::PrivateKeyHandle::load(
            &self.data_dir.to_string_lossy(),
            &format!("{}_mldsa", identity_id),
            None,
        ) {
            Ok(handle) => handle,
            Err(e) => {
                tracing::error!("Failed to load ML-DSA private key for {}: {e}", identity_id);
                let msg = format!("切换身份失败：无法加载私钥: {}", e);
                self.send_warning_mpsc(msg).await;
                return;
            }
        };
        let mldsa_public_key = match crate::identity::extract_public_key_from_private(
            mldsa_handle.get_private_key(),
            true,
        ) {
            Ok(pk) => pk,
            Err(e) => {
                tracing::error!("Failed to extract ML-DSA public key: {e}");
                self.send_warning_mpsc(format!("切换身份失败：无法提取公钥: {}", e))
                    .await;
                return;
            }
        };
        let mldsa_pubkey_hex = hex::encode(&mldsa_public_key);
        tracing::info!(
            "Loaded ML-DSA key for identity: {}",
            &mldsa_pubkey_hex[..16]
        );

        // 3. 生成新的临时 ML-KEM 密钥对
        let (mlkem_public_key, mlkem_secret_key) = match crypto::generate_mlkem_keypair() {
            Ok(kp) => kp,
            Err(e) => {
                tracing::error!("Failed to generate ML-KEM keypair: {e}");
                self.send_warning_mpsc(format!("切换身份失败：无法生成会话密钥: {}", e))
                    .await;
                return;
            }
        };
        // 保存新的 ML-KEM 私钥到 Keyring
        // 使用 Zeroizing 包装临时副本，确保保存后 drop 时自动清零内存
        let mlkem_secret_key = Zeroizing::new(mlkem_secret_key);
        if let Err(e) = rootcell::identity::PrivateKeyHandle::save(
            &self.data_dir.to_string_lossy(),
            &format!("{}_mlkem", identity_id),
            &mlkem_secret_key,
        ) {
            tracing::error!("Failed to save ML-KEM private key: {e}");
            self.send_warning_mpsc(format!("切换身份失败：无法保存会话密钥: {}", e))
                .await;
            return;
        }
        // Zeroizing 的 drop 会自动清零内存，无需手动 drop
        let mlkem_pubkey_hex = hex::encode(&mlkem_public_key);

        // 4. 生成新的临时 PeerID（Ed25519）
        let keypair = match identity::generate_temporary_peerid() {
            Ok(kp) => kp,
            Err(e) => {
                tracing::error!("Failed to generate temporary PeerID: {e}");
                self.send_warning_mpsc(format!("切换身份失败：无法生成网络标识: {}", e))
                    .await;
                return;
            }
        };
        let peer_id = keypair.public().to_peer_id();

        // 5. 重新初始化 swarm（新 PeerID 需要新传输层身份）
        let p2p::SwarmWithValidator { swarm, validator } =
            match p2p::swarm_init(&self.data_dir, keypair.clone()) {
                Ok(sv) => sv,
                Err(e) => {
                    tracing::error!("Failed to reinitialize swarm: {e}");
                    self.send_warning_mpsc(format!("切换身份失败：无法重建网络连接: {}", e))
                        .await;
                    return;
                }
            };

        // 6. 更新 ChatCore 字段
        self.swarm = swarm;
        self.validator = validator;
        self.identity_keypair = keypair;
        self.mldsa_pubkey_hex = Some(mldsa_pubkey_hex.clone());
        self.current_peer_id = Some(peer_id);
        self.mldsa_identity_id = Some(identity_id.clone());
        self.mlkem_pubkey_hex = Some(mlkem_pubkey_hex.clone());
        // 缓存 ML-DSA 私钥到内存，避免后续发送消息时重复访问 Keyring
        // 使用 Zeroizing 包装，确保私钥在内存中可被自动清零
        self.mldsa_private_key = Some(Zeroizing::new(mldsa_handle.get_private_key().to_vec()));

        // 7. 立即发布新身份到 DHT（使用缓存的连接）
        if let Ok(store) = self.get_dht_store() {
            let _ = store.set_pubkey_peerid(&mldsa_pubkey_hex, &peer_id);
            let _ = store.set_mlkem_pubkey(&mldsa_pubkey_hex, &mlkem_pubkey_hex);
        }

        tracing::info!(
            "Runtime identity switch complete: ML-DSA={}, ML-KEM={}, PeerID={}",
            &mldsa_pubkey_hex[..16],
            &mlkem_pubkey_hex[..16],
            peer_id
        );
        let msg = format!("已切换到身份: {}..", &identity_id[..16]);
        self.send_log_mpsc(msg).await;
    }

    pub(crate) async fn delete_identity(&mut self, identity_id: String) {
        if let Some(pool) = storage::pool() {
            match storage::delete_identity(pool, &self.data_dir, &identity_id).await {
                Ok(_) => {
                    tracing::info!("Deleted identity: {}", identity_id);

                    // 清理本地 DHT 数据库中该身份的所有记录
                    if let Ok(store) = self.get_dht_store() {
                        let _ = store.remove_pubkey_peerid(&identity_id);
                        let _ = store.remove_mlkem_pubkey(&identity_id);
                        tracing::info!(
                            "Cleaned up local DHT records for deleted identity: {}",
                            &identity_id[..16]
                        );
                    }

                    // 发布空记录到 Kademlia 网络，覆盖其他节点缓存的旧记录
                    // 注意：DHT 是分布式网络，无法强制删除其他节点上的记录，
                    // 但通过发布空值记录可以覆盖旧记录，使后续查询返回空值。
                    // 使用 SignedIdentityRecord 格式（空 value 表示删除），
                    // 接收方在反序列化失败时会自动拒绝该记录。
                    let peerid_key = format!("peerid:{}", identity_id);
                    let mlkem_key = format!("mlkem:{}", identity_id);
                    // 使用空 Vec 作为记录值，接收方反序列化 SignedIdentityRecord 时会失败，
                    // 从而自动拒绝该记录（视为未找到）。
                    let empty_record = |key: String| libp2p::kad::Record {
                        key: libp2p::kad::RecordKey::new(&key),
                        value: Vec::new(), // 空值，反序列化会失败，接收方视为记录不存在
                        publisher: None,
                        expires: None,
                    };
                    let _ = self
                        .swarm
                        .behaviour_mut()
                        .kademlia
                        .put_record(empty_record(peerid_key), libp2p::kad::Quorum::One);
                    let _ = self
                        .swarm
                        .behaviour_mut()
                        .kademlia
                        .put_record(empty_record(mlkem_key), libp2p::kad::Quorum::One);
                    tracing::info!(
                        "Published tombstone records to DHT network for deleted identity: {}",
                        &identity_id[..16]
                    );

                    // 如果删除的是当前身份，重置 ChatCore 字段
                    if self.mldsa_identity_id.as_deref() == Some(&identity_id) {
                        self.mldsa_pubkey_hex = None;
                        self.mldsa_identity_id = None;
                        self.mlkem_pubkey_hex = None;
                        self.mldsa_private_key = None;
                        self.current_peer_id = None;
                        tracing::warn!(
                            "Deleted current identity {}, ChatCore fields reset",
                            identity_id
                        );
                    }
                }
                Err(e) => {
                    tracing::error!("Failed to delete identity {}: {e}", identity_id);
                }
            }
        }
    }
}
