use crate::actor::p2p::P2pCommand;
use crate::{core::ChatCore, storage};

impl ChatCore {
    pub(crate) async fn add_contact(
        &mut self,
        mldsa_pubkey_hex: String,
        mlkem_public_key: Vec<u8>,
        name: Option<String>,
    ) -> bool {
        let owner_identity_id = self.mldsa_identity_id.as_deref().unwrap_or("");

        if let Some(pool) = storage::pool() {
            match storage::upsert_contact(
                pool,
                owner_identity_id,
                &mldsa_pubkey_hex,
                name.as_deref(),
                Some(&mlkem_public_key),
            )
            .await
            {
                Ok(_) => {
                    tracing::info!("Successfully added contact: {}", &mldsa_pubkey_hex[..16]);
                    let msg = format!("好友 {} 添加成功", &mldsa_pubkey_hex[..16]);
                    self.send_log_mpsc(msg).await;
                    // 添加联系人后，发起 DHT 查询以获取对方的最新信息（使用 try_send 避免阻塞）
                    let _ = self.p2p_handle.tx.try_send(
                        crate::actor::ActorCommand::Custom(P2pCommand::GetProviders {
                            key: mldsa_pubkey_hex.clone(),
                        }),
                    );
                    // Fix 4: 重新发布自身身份到 DHT，确保对方能通过 DHT 反向发现我方
                    // 直接写入本地 DHT 存储 + 发起网络发布（使用 try_send 避免阻塞）
                    let store = self.get_dht_store();
                    if let (Some(pubkey), Some(pid)) = (&self.mldsa_pubkey_hex, &self.current_peer_id) {
                        let _ = store.set_pubkey_peerid(pubkey, pid);
                    }
                    let _ = self.p2p_handle.tx.try_send(
                        crate::actor::ActorCommand::Custom(P2pCommand::PublishIdentity {
                            mldsa_pubkey_hex: self.mldsa_pubkey_hex.clone().unwrap_or_default(),
                            mlkem_pubkey_hex: self.mlkem_pubkey_hex.clone().unwrap_or_default(),
                        }),
                    );
                    let mlkem_key = format!("mlkem:{}", mldsa_pubkey_hex);
                    let _ = self.p2p_handle.tx.try_send(
                        crate::actor::ActorCommand::Custom(P2pCommand::GetRecord {
                            key: mlkem_key,
                        }),
                    );
                    true
                }
                Err(e) => {
                    tracing::error!("Failed to save contact: {e}");
                    let msg = format!("保存好友信息失败: {}", e);
                    self.send_warning_mpsc(msg).await;
                    false
                }
            }
        } else {
            tracing::error!("Database pool not available");
            self.send_warning_mpsc("数据库不可用".to_string()).await;
            false
        }
    }

    /// 从 DHT 本地数据库获取缓存的 ML-KEM 公钥字节
    fn get_cached_mlkem_bytes(&self, mldsa_pubkey_hex: &str) -> Vec<u8> {
        let store = self.get_dht_store();
        match store.get_mlkem_pubkey(mldsa_pubkey_hex) {
            Ok(Some(hex_str)) => match hex::decode(&hex_str) {
                Ok(bytes) => bytes,
                Err(_) => Vec::new(),
            },
            _ => Vec::new(),
        }
    }

    /// 通过 DHT 网络发现联系人（非阻塞版本）
    ///
    /// 如果是首次发现（联系人尚未在数据库中），会调用 add_contact 并发送日志。
    /// 如果联系人已存在，则只刷新 DHT 查询，不重复添加和输出日志。
    pub(crate) async fn discover_contact(&mut self, mldsa_pubkey_hex: &str, name: Option<String>) {
        let pubkey_short = &mldsa_pubkey_hex[..16];
        tracing::info!("Discovering contact {} via DHT network", pubkey_short);

        // 检查联系人是否已在数据库中（避免重复添加和日志输出）
        let is_existing_contact = match self.mldsa_identity_id.as_ref() {
            Some(owner_id) => match storage::pool() {
                Some(pool) => storage::is_contact_exists(pool, owner_id, mldsa_pubkey_hex)
                    .await
                    .unwrap_or(false),
                None => false,
            },
            None => false,
        };

        let store = self.get_dht_store();
        let (has_local_peerid, has_local_mlkem) = (
            store
                .get_peerid_by_pubkey(mldsa_pubkey_hex)
                .ok()
                .flatten()
                .is_some(),
            store
                .get_mlkem_pubkey(mldsa_pubkey_hex)
                .ok()
                .flatten()
                .is_some(),
        );

        // 如果本地已有完整信息
        if has_local_peerid && has_local_mlkem {
            if is_existing_contact {
                // 已存在的联系人：只刷新 DHT 查询，不重复添加
                tracing::debug!(
                    "DHT discovery: contact {} already exists, refreshing DHT query",
                    pubkey_short
                );
                // 通过 P2pActor 发起 GetProviders 以刷新在线状态（使用 try_send 避免阻塞）
                let _ = self.p2p_handle.tx.try_send(
                    crate::actor::ActorCommand::Custom(P2pCommand::GetProviders {
                        key: mldsa_pubkey_hex.to_string(),
                    }),
                );
            } else {
                // 新联系人：直接添加
                tracing::info!(
                    "DHT discovery: contact {} already cached locally, adding directly",
                    pubkey_short
                );
                let mlkem_key = self.get_cached_mlkem_bytes(mldsa_pubkey_hex);
                self.add_contact(mldsa_pubkey_hex.to_string(), mlkem_key, name)
                    .await;
                let msg = format!("已通过 DHT 发现并添加联系人: {}..", pubkey_short);
                self.send_log_mpsc(msg).await;
            }
            return;
        }

        // 本地没有缓存，发起网络 DHT 查询但不阻塞等待
        if !has_local_peerid {
            let _ = self.p2p_handle.tx.try_send(
                crate::actor::ActorCommand::Custom(P2pCommand::GetProviders {
                    key: mldsa_pubkey_hex.to_string(),
                }),
            );
            tracing::debug!(
                "DHT discovery: initiated GetProviders for {} (non-blocking)",
                pubkey_short
            );
        }

        if !has_local_mlkem {
            let mlkem_key = format!("mlkem:{}", mldsa_pubkey_hex);
            let _ = self.p2p_handle.tx.try_send(
                crate::actor::ActorCommand::Custom(P2pCommand::GetRecord {
                    key: mlkem_key,
                }),
            );
            tracing::debug!(
                "DHT discovery: initiated ML-KEM query for {} (non-blocking)",
                pubkey_short
            );
        }

        // 如果本地已有部分信息
        if has_local_peerid || has_local_mlkem {
            if is_existing_contact {
                // 已存在的联系人：只刷新 DHT 查询，不重复添加
                tracing::debug!(
                    "DHT discovery: contact {} already exists, refreshing DHT query",
                    pubkey_short
                );
            } else {
                // 新联系人：尝试添加
                let mlkem_key = if has_local_mlkem {
                    self.get_cached_mlkem_bytes(mldsa_pubkey_hex)
                } else {
                    Vec::new()
                };
                self.add_contact(mldsa_pubkey_hex.to_string(), mlkem_key, name)
                    .await;
                let msg = format!("已通过 DHT 发现并添加联系人: {}..", pubkey_short);
                self.send_log_mpsc(msg).await;
            }
        } else {
            tracing::info!(
                "DHT discovery: no cached data for contact {}, initiated network queries (results will arrive via swarm events)",
                pubkey_short
            );
        }
    }
}