use aws_lc_rs::kem::{DecapsulationKey, ML_KEM_768};
use zeroize::Zeroizing;

use libp2p::identity;

use crate::{
    actor::p2p::{P2pActorBuilder, P2pActorHandle, P2pCommand},
    core::ChatCore, p2p, peerid_store::PeerIdConfig, storage,
};

/// 关闭旧的 P2pActor 并等待事件循环退出
///
/// 先发送 SaveRoutingTable 持久化路由表，再发送 Shutdown 命令。
/// 调用方应在创建新 actor 前调用此函数，确保旧 actor 完全退出。
async fn shutdown_old_actor(handle: &P2pActorHandle) {
    let _ = handle.tx.send(P2pCommand::SaveRoutingTable).await;
    let _ = handle.tx.send(P2pCommand::Shutdown).await;
    // 让出执行权，给旧 actor 处理 Shutdown 命令的时间
    tokio::task::yield_now().await;
}

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

                self.peerid_to_mlkem.clear();
                self.mldsa_pubkey_hex = Some(mldsa_id.clone());
                self.mldsa_identity_id = Some(mldsa_id.clone());
                self.mlkem_pubkey_hex = Some(mlkem_id.clone());
                self.mlkem_decap_key = Some(identity.mlkem_decap_key);

                self.cache_mldsa_private_key(&mldsa_id);

                self.reinitialize_swarm().await;

                let store = self.get_dht_store();
                if let Some(peer_id) = self.current_peer_id {
                    let _ = store.set_pubkey_peerid(&mldsa_id, &peer_id);
                }

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
        // 所有 fallible 操作完成后才写 DB，避免中间失败导致 DB 与内存状态不一致

        let mldsa_handle = match rootcell::identity::PrivateKeyHandle::load(
            &self.data_dir.to_string_lossy(),
            &format!("{}_mldsa", identity_id),
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

        // 身份切换时复用设备级 PeerID，保持网络拓扑稳定
        // 必须在 shutdown_old_actor 之前 resolve，避免 fallible 失败后无 actor 可用
        let (keypair, peerid_config) = match self.ensure_peerid().await {
            Ok(v) => v,
            Err(e) => {
                self.send_warning_mpsc(format!("切换身份失败：无法加载 PeerID: {e}")).await;
                return;
            }
        };
        self.peerid_config = Some(peerid_config);
        let peer_id = keypair.public().to_peer_id();

        shutdown_old_actor(&self.p2p_handle).await;

        if let Err(e) = self.rebuild_p2p_stack(keypair).await {
            self.send_warning_mpsc(format!("切换身份失败：{}", e)).await;
            return;
        }

        // 所有 fallible 操作成功，写入 DB 持久化当前身份
        if let Some(pool) = storage::pool() {
            if let Err(e) = storage::set_current_identity(pool, &identity_id).await {
                tracing::error!("Failed to set current identity in DB: {e}");
                self.send_warning_mpsc(format!("切换身份失败：{}", e)).await;
                return;
            }
        } else {
            tracing::error!("Database pool not available for identity switch");
            return;
        }

        self.peerid_to_mlkem.clear();
        self.mldsa_pubkey_hex = Some(mldsa_pubkey_hex.clone());
        self.current_peer_id = Some(peer_id);
        self.mldsa_identity_id = Some(identity_id.clone());
        self.mlkem_pubkey_hex = Some(mlkem_pubkey_hex.clone());
        self.mlkem_decap_key = Some(mlkem_decap_key);
        self.mldsa_private_key = Some(Zeroizing::new(mldsa_handle.get_private_key().to_vec()));

        let store = self.get_dht_store();
        let _ = store.set_pubkey_peerid(&mldsa_pubkey_hex, &peer_id);

        tracing::info!(
            "Runtime identity switch complete: ML-DSA={}, ML-KEM={}, PeerID={}",
            &mldsa_pubkey_hex[..16],
            &mlkem_pubkey_hex[..16],
            peer_id
        );
        let msg = format!("已切换到身份: {}..", &identity_id[..16]);
        self.send_log_mpsc(msg).await;
    }

    /// 删除指定身份
    pub(crate) async fn delete_identity(&mut self, identity_id: String) {
        if let Some(pool) = storage::pool() {
            match storage::delete_identity(pool, &self.data_dir, &identity_id).await {
                Ok(_) => {
                    tracing::info!("Deleted identity: {}", identity_id);

                    let store = self.get_dht_store();
                    let _ = store.remove_pubkey_peerid(&identity_id);

                    self.stop_dht_providing(&identity_id).await;

                    if self.mldsa_identity_id.as_deref() == Some(&identity_id) {
                        self.mldsa_pubkey_hex = None;
                        self.mldsa_identity_id = None;
                        self.mlkem_pubkey_hex = None;
                        self.mldsa_private_key = None;
                        self.current_peer_id = None;
                        self.peerid_to_mlkem.clear();
                    }
                }
                Err(e) => {
                    tracing::error!("Failed to delete identity {}: {e}", identity_id);
                }
            }
        }
    }

    fn cache_mldsa_private_key(&mut self, identity_id: &str) {
        match rootcell::identity::PrivateKeyHandle::load(
            &self.data_dir.to_string_lossy(),
            &format!("{}_mldsa", identity_id),
        ) {
            Ok(handle) => {
                self.mldsa_private_key = Some(Zeroizing::new(handle.get_private_key().to_vec()));
            }
            Err(e) => {
                tracing::error!("Failed to cache ML-DSA private key for new identity: {e}");
            }
        }
    }

    async fn rebuild_p2p_stack(&mut self, keypair: libp2p::identity::Keypair) -> Result<(), String> {
        let swarm = p2p::swarm_init(&self.data_dir, keypair.clone(), &self.bootstrap_nodes, self.peerid_config.as_ref())
            .map_err(|e| format!("Failed to reinitialize swarm: {}", e))?;
        self.identity_keypair = keypair;
        self.current_peer_id = Some(*swarm.local_peer_id());
        let (p2p_handle, rx_p2p_event) = P2pActorBuilder::new()
            .swarm(swarm)
            .dht_cache(self.dht_cache.clone())
            .data_dir(self.data_dir.clone())
            .relay_nodes(self.relay_nodes.clone())
            .bootstrap_nodes(self.bootstrap_nodes.clone())
            .channel_size(crate::core::CHANNEL_CAPACITY)
            .cancellation_token(self.core_handle.shutdown_token.clone())
            .start();
        self.p2p_handle = p2p_handle;
        self.rx_p2p_event = rx_p2p_event;
        Ok(())
    }

    #[allow(clippy::unused_async)]
    async fn ensure_peerid(&mut self) -> Result<(identity::Keypair, PeerIdConfig), String> {
        match self.peerid_config.take() {
            Some(config) => match config.to_keypair() {
                Ok(kp) => Ok((kp, config)),
                Err(e) => {
                    tracing::warn!("Failed to restore PeerID from config, deleting corrupted entry: {e}");
                    crate::identity::load_or_create_peerid(&self.data_dir)
                }
            },
            None => crate::identity::load_or_create_peerid(&self.data_dir),
        }
    }

    async fn reinitialize_swarm(&mut self) {
        // 先 resolve PeerID 再关机，避免 fallible 失败后无 actor 可用
        let (keypair, peerid_config) = match self.ensure_peerid().await {
            Ok(v) => v,
            Err(e) => {
                tracing::error!("重新初始化 swarm 失败：无法加载 PeerID: {e}");
                self.send_warning_mpsc(format!("网络重建失败：无法加载 PeerID: {e}")).await;
                return;
            }
        };
        self.peerid_config = Some(peerid_config);
        shutdown_old_actor(&self.p2p_handle).await;
        if let Err(e) = self.rebuild_p2p_stack(keypair).await {
            tracing::error!("{e}");
        }
    }

    async fn stop_dht_providing(&mut self, identity_id: &str) {
        let _ = self.p2p_handle.tx.try_send(
            crate::actor::p2p::P2pCommand::StopProviding {
                mldsa_pubkey_hex: identity_id.to_string(),
            },
        );
        tracing::info!(
            "Stopped DHT providing for deleted identity: {}",
            &identity_id[..16]
        );
    }
}