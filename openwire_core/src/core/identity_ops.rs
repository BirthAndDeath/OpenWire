use aws_lc_rs::kem::{DecapsulationKey, ML_KEM_768};
use zeroize::Zeroizing;

use crate::{core::ChatCore, identity, p2p, storage};

impl ChatCore {
    /// 生成新身份（ML-DSA 密钥对 + 临时 ML-KEM 密钥对）
    pub(crate) async fn generate_identity(&mut self) {
        let temp_cfg = crate::coreconfig::CoreConfig {
            data_dir: self.data_dir.clone(),
            ..Default::default()
        };

        match crate::identity::generate_complete_identity(&temp_cfg).await {
            Ok(identity) => {
                let mldsa_id = hex::encode(&identity.mldsa_public_key);
                let mlkem_id = hex::encode(&identity.mlkem_public_key);
                tracing::info!(
                    "Generated new identity: ML-DSA={}, ML-KEM={} (ephemeral)",
                    &mldsa_id[..16],
                    &mlkem_id[..16]
                );

                // 更新核心身份字段
                self.mldsa_pubkey_hex = Some(mldsa_id.clone());
                self.mldsa_identity_id = Some(mldsa_id.clone());
                self.mlkem_pubkey_hex = Some(mlkem_id.clone());
                self.mlkem_decap_key = Some(identity.mlkem_decap_key);

                // 加载新身份的 ML-DSA 私钥并缓存到内存
                self.cache_mldsa_private_key(&mldsa_id);

                // 重新初始化 swarm（新 PeerID）
                self.reinitialize_swarm();

                // 立即发布新身份到 DHT
                if let Ok(store) = self.get_dht_store() {
                    if let Some(peer_id) = self.current_peer_id {
                        let _ = store.set_pubkey_peerid(&mldsa_id, &peer_id);
                    }
                    let _ = store.set_mlkem_pubkey(&mldsa_id, &mlkem_id);
                }

                // 更新数据库中的当前身份标记
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

    /// 选择当前身份（运行时切换）
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
        // 直接生成 DecapsulationKey 对象，避免序列化/反序列化问题
        let mlkem_decap_key = match DecapsulationKey::generate(&ML_KEM_768) {
            Ok(key) => key,
            Err(e) => {
                tracing::error!("Failed to generate ML-KEM DecapsulationKey: {:?}", e);
                self.send_warning_mpsc(format!("切换身份失败：无法生成会话密钥: {:?}", e))
                    .await;
                return;
            }
        };
        let mlkem_encap_key = match mlkem_decap_key.encapsulation_key() {
            Ok(key) => key,
            Err(e) => {
                tracing::error!("Failed to get ML-KEM EncapsulationKey: {:?}", e);
                self.send_warning_mpsc(format!("切换身份失败：无法获取加密密钥: {:?}", e))
                    .await;
                return;
            }
        };
        let mlkem_public_key = match mlkem_encap_key.key_bytes() {
            Ok(key) => key.as_ref().to_vec(),
            Err(e) => {
                tracing::error!("Failed to serialize ML-KEM public key: {:?}", e);
                self.send_warning_mpsc(format!("切换身份失败：无法序列化公钥: {:?}", e))
                    .await;
                return;
            }
        };
        let mlkem_pubkey_hex = hex::encode(&mlkem_public_key);

        // 4. 生成新的临时 PeerID 并重新初始化 swarm
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

        let dht_db = self.dht_db.clone().unwrap();
        let relay_nodes: Vec<(String, String)> = Vec::new();
        let bootstrap_nodes: Vec<(String, String)> = Vec::new();
        let swarm = match p2p::swarm_init(&self.data_dir, keypair.clone(), dht_db, &relay_nodes, &bootstrap_nodes) {
            Ok(s) => s,
            Err(e) => {
                tracing::error!("Failed to reinitialize swarm: {e}");
                self.send_warning_mpsc(format!("切换身份失败：无法重建网络连接: {}", e))
                    .await;
                return;
            }
        };

        // 5. 更新 ChatCore 字段
        self.identity_keypair = keypair;
        self.mldsa_pubkey_hex = Some(mldsa_pubkey_hex.clone());
        self.current_peer_id = Some(peer_id);
        self.mldsa_identity_id = Some(identity_id.clone());
        self.mlkem_pubkey_hex = Some(mlkem_pubkey_hex.clone());
        self.mlkem_decap_key = Some(mlkem_decap_key);
        self.mldsa_private_key = Some(Zeroizing::new(mldsa_handle.get_private_key().to_vec()));

        // 6. 重新创建 P2pActor（使用新的 swarm）
        let (p2p_event_tx, p2p_event_rx) = tokio::sync::mpsc::channel(super::CHANNEL_CAPACITY);
        let p2p_actor = crate::actor::p2p::P2pActor::new(
            swarm,
            self.dht_db.clone(),
            self.data_dir.clone(),
            p2p_event_tx,
        );
        let p2p_handle = crate::actor::p2p::start_p2p_actor(
            p2p_actor,
            super::CHANNEL_CAPACITY,
            self.core_handle.shutdown_token.clone(),
        );
        self.p2p_handle = p2p_handle;
        self.rx_p2p_event = p2p_event_rx;

        // 7. 立即发布新身份到 DHT
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

    /// 删除指定身份（移除密钥文件 + DHT 记录 + 数据库记录）
    pub(crate) async fn delete_identity(&mut self, identity_id: String) {
        if let Some(pool) = storage::pool() {
            match storage::delete_identity(pool, &self.data_dir, &identity_id).await {
                Ok(_) => {
                    tracing::info!("Deleted identity: {}", identity_id);

                    // 清理本地 DHT 数据库
                    if let Ok(store) = self.get_dht_store() {
                        let _ = store.remove_pubkey_peerid(&identity_id);
                        let _ = store.remove_mlkem_pubkey(&identity_id);
                    }

                    // 发布空记录到 Kademlia 网络（墓碑记录）
                    self.publish_tombstone_records(&identity_id).await;

                    // 如果删除的是当前身份，重置字段
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

    /// 缓存 ML-DSA 私钥到内存
    fn cache_mldsa_private_key(&mut self, identity_id: &str) {
        match rootcell::identity::PrivateKeyHandle::load(
            &self.data_dir.to_string_lossy(),
            &format!("{}_mldsa", identity_id),
            None,
        ) {
            Ok(handle) => {
                self.mldsa_private_key = Some(Zeroizing::new(handle.get_private_key().to_vec()));
            }
            Err(e) => {
                tracing::error!("Failed to cache ML-DSA private key for new identity: {e}");
            }
        }
    }

    /// 重新初始化 swarm（生成新 PeerID）
    fn reinitialize_swarm(&mut self) {
        let dht_db = self.dht_db.clone().unwrap();
        let relay_nodes: Vec<(String, String)> = Vec::new();
        let bootstrap_nodes: Vec<(String, String)> = Vec::new();
        match identity::generate_temporary_peerid() {
            Ok(keypair) => {
                let peer_id = keypair.public().to_peer_id();
                match p2p::swarm_init(&self.data_dir, keypair.clone(), dht_db, &relay_nodes, &bootstrap_nodes) {
                    Ok(swarm) => {
                        self.identity_keypair = keypair;
                        self.current_peer_id = Some(peer_id);

                        // 重新创建 P2pActor（使用新的 swarm）
                        let (p2p_event_tx, p2p_event_rx) =
                            tokio::sync::mpsc::channel(super::CHANNEL_CAPACITY);
                        let p2p_actor = crate::actor::p2p::P2pActor::new(
                            swarm,
                            self.dht_db.clone(),
                            self.data_dir.clone(),
                            p2p_event_tx,
                        );
                        let p2p_handle = crate::actor::p2p::start_p2p_actor(
                            p2p_actor,
                            super::CHANNEL_CAPACITY,
                            self.core_handle.shutdown_token.clone(),
                        );
                        self.p2p_handle = p2p_handle;
                        self.rx_p2p_event = p2p_event_rx;

                        tracing::info!("Swarm reinitialized, PeerID={}", peer_id);
                    }
                    Err(e) => {
                        tracing::error!("Failed to reinitialize swarm: {e}");
                    }
                }
            }
            Err(e) => {
                tracing::error!("Failed to generate temporary PeerID: {e}");
            }
        }
    }

    /// 发布空记录到 Kademlia 网络（墓碑记录）
    async fn publish_tombstone_records(&mut self, identity_id: &str) {
        // 通过 P2pActor 发布墓碑记录（空 ML-KEM 公钥表示删除）
        let _ = self.p2p_handle.send(
            crate::actor::ActorCommand::Custom(
                crate::actor::p2p::P2pCommand::PublishIdentity {
                    mldsa_pubkey_hex: identity_id.to_string(),
                    mlkem_pubkey_hex: String::new(),
                },
            ),
        ).await;
        tracing::info!(
            "Published tombstone records to DHT network for deleted identity: {}",
            &identity_id[..16]
        );
    }
}
