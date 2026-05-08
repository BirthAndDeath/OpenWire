use crate::{core::ChatCore, p2p, storage};

impl ChatCore {
    pub(crate) async fn add_contact(
        &mut self,
        mldsa_pubkey_hex: String,
        mlkem_public_key: Vec<u8>,
        name: Option<String>,
    ) -> bool {
        // 验证身份绑定：检查 DHT 中是否存在该 ML-DSA 身份的绑定记录
        if !mlkem_public_key.is_empty() {
            let mlkem_pubkey_hex = hex::encode(&mlkem_public_key);
            match p2p::verify_identity_binding(
                &self.data_dir,
                &mldsa_pubkey_hex,
                None,
                Some(&mlkem_pubkey_hex),
                self.dht_db.clone(),
            ) {
                Ok(true) => {
                    tracing::info!(
                        "Identity binding verified for contact {} (ML-KEM matches DHT)",
                        &mldsa_pubkey_hex[..16]
                    );
                }
                Ok(false) => {
                    tracing::warn!(
                        "Identity binding verification failed for contact {}: ML-KEM mismatch or not found in DHT",
                        &mldsa_pubkey_hex[..16]
                    );
                    let msg = format!(
                        "警告：无法验证联系人 {} 的身份绑定（DHT 中未找到对应的 ML-KEM 公钥记录），请确认公钥来源可靠",
                        &mldsa_pubkey_hex[..16]
                    );
                    self.send_warning_mpsc(msg).await;
                }
                Err(e) => {
                    tracing::warn!(
                        "Identity binding check error for contact {}: {}",
                        &mldsa_pubkey_hex[..16],
                        e
                    );
                }
            }
        }

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
        let store = match self.get_dht_store() {
            Ok(store) => store,
            Err(_) => return Vec::new(),
        };
        match store.get_mlkem_pubkey(mldsa_pubkey_hex) {
            Ok(Some(hex_str)) => match hex::decode(&hex_str) {
                Ok(bytes) => bytes,
                Err(_) => Vec::new(),
            },
            _ => Vec::new(),
        }
    }

    /// 通过 DHT 网络发现联系人（非阻塞版本）
    pub(crate) async fn discover_contact(&mut self, mldsa_pubkey_hex: &str, name: Option<String>) {
        let pubkey_short = &mldsa_pubkey_hex[..16];
        tracing::info!("Discovering contact {} via DHT network", pubkey_short);

        let store = self.get_dht_store();
        let (has_local_peerid, has_local_mlkem) = match store {
            Ok(ref store) => (
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
            ),
            Err(_) => (false, false),
        };

        // 如果本地已有完整信息，直接添加联系人
        if has_local_peerid && has_local_mlkem {
            tracing::info!(
                "DHT discovery: contact {} already cached locally, adding directly",
                pubkey_short
            );
            let mlkem_key = self.get_cached_mlkem_bytes(mldsa_pubkey_hex);
            self.add_contact(mldsa_pubkey_hex.to_string(), mlkem_key, name)
                .await;
            let msg = format!("已通过 DHT 发现并添加联系人: {}..", pubkey_short);
            self.send_log_mpsc(msg).await;
            return;
        }

        // 本地没有缓存，发起网络 DHT 查询但不阻塞等待
        if !has_local_peerid {
            let record_key = format!("peerid:{}", mldsa_pubkey_hex);
            let key = libp2p::kad::RecordKey::new(&record_key);
            let _rx = p2p::register_dht_query_callback(mldsa_pubkey_hex.to_string());
            let _query_id = self.swarm.behaviour_mut().kademlia.get_record(key);
            tracing::debug!(
                "DHT discovery: initiated PeerID query for {} (non-blocking)",
                pubkey_short
            );
        }

        if !has_local_mlkem {
            let mlkem_key = format!("mlkem:{}", mldsa_pubkey_hex);
            let key = libp2p::kad::RecordKey::new(&mlkem_key);
            let query_id = format!("mlkem_{}", mldsa_pubkey_hex);
            let _rx = p2p::register_mlkem_query_callback(query_id);
            let _query_id = self.swarm.behaviour_mut().kademlia.get_record(key);
            tracing::debug!(
                "DHT discovery: initiated ML-KEM query for {} (non-blocking)",
                pubkey_short
            );
        }

        // 如果本地已有部分信息，也尝试添加联系人
        if has_local_peerid || has_local_mlkem {
            let mlkem_key = if has_local_mlkem {
                self.get_cached_mlkem_bytes(mldsa_pubkey_hex)
            } else {
                Vec::new()
            };
            self.add_contact(mldsa_pubkey_hex.to_string(), mlkem_key, name)
                .await;
            let msg = format!("已通过 DHT 发现并添加联系人: {}..", pubkey_short);
            self.send_log_mpsc(msg).await;
        } else {
            tracing::info!(
                "DHT discovery: no cached data for contact {}, initiated network queries (results will arrive via swarm events)",
                pubkey_short
            );
        }
    }
}
