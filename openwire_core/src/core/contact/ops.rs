use std::time::Instant;

use crate::actor::p2p::P2pCommand;
use crate::{core::ChatCore, storage};
use sha2::Digest;

impl ChatCore {
    pub(crate) async fn add_contact(
        &mut self,
        mldsa_pubkey_hex: String,
        mlkem_public_key: Vec<u8>,
        name: Option<String>,
    ) -> bool {
        if !crate::signature::validate_mldsa_pubkey_hex(&mldsa_pubkey_hex) {
            tracing::warn!("拒绝添加联系人：ML-DSA 公钥密码学验证失败");
            self.send_warning_mpsc("ML-DSA 公钥无效，拒绝添加".to_string())
                .await;
            return false;
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
                    tracing::info!("Successfully added contact: {}", &mldsa_pubkey_hex[..16.min(mldsa_pubkey_hex.len())]);
                    let msg = format!("好友 {} 添加成功", &mldsa_pubkey_hex[..16.min(mldsa_pubkey_hex.len())]);
                    self.send_log_mpsc(msg).await;
                    // 添加联系人后，立即发起发现（DHT + 中继，双路并行）
                    // 使用 SHA256 哈希隐藏原始公钥
                    let query_key = hex::encode(sha2::Sha256::digest(mldsa_pubkey_hex.as_bytes()));
                    // 预存 DHT 查询键 → 公钥映射，供 GetProvidersResult 反向查找
                    self.peer_cache.dht_key_to_pubkey.insert(query_key.clone(), mldsa_pubkey_hex.clone());
                    if let Err(e) = self.p2p_handle.tx.try_send(P2pCommand::GetProviders {
                        key: query_key,
                    }) {
                        tracing::warn!("Failed to send GetProviders after adding contact: {e:?}");
                    }
                    if let Err(e) = self.p2p_handle.tx.try_send(P2pCommand::DiscoverPeer {
                        mldsa_pubkey_hex: mldsa_pubkey_hex.clone(),
                    }) {
                        tracing::warn!("Failed to send DiscoverPeer after adding contact: {e:?}");
                    }
                    // 重新发布自身身份到 DHT，确保对方能通过 DHT 反向发现我方
                    let store = self.get_dht_store();
                    if let (Some(pubkey), Some(pid)) =
                        (&self.mldsa_pubkey_hex, &self.current_peer_id)
                    {
                        store.set_pubkey_peerid(pubkey, pid);
                    }
                    if let Err(e) = self.p2p_handle.tx.try_send(P2pCommand::PublishIdentity {
                        mldsa_pubkey_hex: self.mldsa_pubkey_hex.clone().unwrap_or_default(),
                    }) {
                        tracing::warn!(
                            "Failed to send PublishIdentity after adding contact: {e:?}"
                        );
                    }
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

    /// 记录每个联系人的最近发现时间，防止过频重复发现
    fn check_discovery_cooldown(&mut self, pubkey: &str) -> bool {
        let now = Instant::now();
        let cooldown = std::time::Duration::from_secs(30);
        if let Some(last) = self.last_discovery_time.get(pubkey)
            && now.duration_since(*last) < cooldown {
                tracing::debug!(
                    "Discover cooldown for {}.., skipping",
                    &pubkey[..16.min(pubkey.len())]
                );
                return false;
            }
        self.last_discovery_time.insert(pubkey.to_string(), now);
        true
    }

    /// 通过 DHT 网络发现联系人
    ///
    /// 发现策略（优先级顺序）：
    ///   1. 本地缓存 → 直接拨号
    ///   2. 已存地址 → 直接拨号（无需 DHT 网络查询）
    ///   3. DHT GetProviders → 网络查询
    ///   4. Relay DiscoverPeer → 中继查询
    ///
    /// 双路并行（DHT + 中继）确保最快发现路径。
    pub(crate) async fn discover_contact(&mut self, mldsa_pubkey_hex: &str, name: Option<String>) {
        let pubkey_short = &mldsa_pubkey_hex[..16.min(mldsa_pubkey_hex.len())];
        tracing::info!("Discovering contact {} via DHT network", pubkey_short);

        // 发现冷却：防止重复发现
        if !self.check_discovery_cooldown(mldsa_pubkey_hex) {
            return;
        }

        // 检查联系人是否已在数据库中
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
        let has_local_peerid = store
            .get_peerid_by_pubkey(mldsa_pubkey_hex)
            .is_some();

        // 如果本地已有 PeerID，直接拨号
        if has_local_peerid {
            let peer_id = store
                .get_peerid_by_pubkey(mldsa_pubkey_hex);
            if let Some(pid) = peer_id
                && !self.connected_peers.contains_key(&pid) {
                    tracing::info!(
                        "DHT discovery: 本地已有 {} 的 PeerID={}，直接拨号",
                        pubkey_short,
                        pid
                    );
                    if let Err(e) = self.p2p_handle.tx.try_send(P2pCommand::Dial { peer_id: pid }) {
                        tracing::warn!("Failed to send Dial for cached contact: {e:?}");
                    }
                }

            if is_existing_contact {
                tracing::debug!(
                    "DHT discovery: contact {} already exists, refreshing DHT query",
                    pubkey_short
                );
                let query_key = hex::encode(sha2::Sha256::digest(mldsa_pubkey_hex.as_bytes()));
                if let Err(e) = self.p2p_handle.tx.try_send(P2pCommand::GetProviders {
                    key: query_key,
                }) {
                    tracing::warn!("Failed to send GetProviders for DHT discovery: {e:?}");
                }
            } else {
                tracing::info!(
                    "DHT discovery: contact {} already cached locally, adding directly",
                    pubkey_short
                );
                self.add_contact(mldsa_pubkey_hex.to_string(), vec![], name)
                    .await;
                let msg = format!("已通过 DHT 发现并添加联系人: {}..", pubkey_short);
                self.send_log_mpsc(msg).await;
            }
            return;
        }

        // === 尝试从存储的多地址直接拨号（无需 DHT 网络查询）===
        // 如果之前成功连接过，存储中可能有对方的历史地址
        self.try_dial_from_stored_addrs(mldsa_pubkey_hex, pubkey_short).await;

        // === 双路并行发现：DHT + 中继 ===
        // 本地没有缓存，发起网络查询但不阻塞等待
        let query_key = hex::encode(sha2::Sha256::digest(mldsa_pubkey_hex.as_bytes()));
        // 预存 DHT 查询键 → 公钥映射，供 GetProvidersResult 反向查找
        self.peer_cache.dht_key_to_pubkey.insert(query_key.clone(), mldsa_pubkey_hex.to_string());
        if let Err(e) = self.p2p_handle.tx.try_send(P2pCommand::GetProviders {
            key: query_key,
        }) {
            tracing::warn!("Failed to send GetProviders for new contact: {e:?}");
        }
        if let Err(e) = self.p2p_handle.tx.try_send(P2pCommand::DiscoverPeer {
            mldsa_pubkey_hex: mldsa_pubkey_hex.to_string(),
        }) {
            tracing::warn!("Failed to send DiscoverPeer for new contact: {e:?}");
        }
        tracing::debug!(
            "DHT discovery: initiated GetProviders + DiscoverPeer + address dial for {} (non-blocking)",
            pubkey_short
        );

        tracing::info!(
            "DHT discovery: no cached data for contact {}, initiated network queries",
            pubkey_short
        );
    }

    /// 尝试从本地存储的 Multiaddr 直接拨号（无需 DHT 网络查询）
    async fn try_dial_from_stored_addrs(&self, mldsa_pubkey_hex: &str, pubkey_short: &str) {
        let store = self.get_dht_store();
        if let Some(peer_id) = store.get_peerid_by_pubkey(mldsa_pubkey_hex) {
            if self.connected_peers.contains_key(&peer_id) {
                return;
            }
            if let addrs = store.get_multiaddrs(&peer_id)
                && !addrs.is_empty() {
                    tracing::info!(
                        "尝试从本地存储的 {} 个地址拨号 {}..",
                        addrs.len(),
                        pubkey_short
                    );
                    for addr in addrs {
                        let _ = self.p2p_handle.tx.try_send(P2pCommand::DialAddr { addr });
                    }
                }
        }
    }
}