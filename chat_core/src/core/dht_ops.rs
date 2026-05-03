use crate::{core::ChatCore, p2p, signature::SignedIdentityRecord};

impl ChatCore {
    /// 将身份记录发布到 Kademlia DHT 网络
    ///
    /// 发布两条记录到网络：
    /// 1. "peerid:{mldsa_pubkey_hex}" -> 签名的 PeerID 记录（用于查找联系人的当前 PeerID）
    /// 2. "mlkem:{mldsa_pubkey_hex}" -> 签名的 ML-KEM 公钥 hex（用于获取联系人的临时加密密钥）
    ///
    /// 每条记录使用 ML-DSA 私钥签名，接收方通过签名验证记录的真实性。
    /// 签名中包含发布者 PeerID，防止重放攻击。
    pub(crate) fn publish_identity_to_dht(
        &mut self,
        mldsa_pubkey_hex: &str,
        peer_id: &str,
        mlkem_pubkey_hex: &str,
    ) {
        let mldsa_private_key = match self.mldsa_private_key.as_ref() {
            Some(key) => key,
            None => {
                tracing::warn!("Cannot publish to DHT: ML-DSA private key not loaded");
                return;
            }
        };
        let current_peer_id = match self.current_peer_id {
            Some(pid) => pid,
            None => {
                tracing::warn!("Cannot publish to DHT: current PeerID not set");
                return;
            }
        };

        // 1. 发布签名的 ML-DSA pubkey -> PeerID 映射到 Kademlia 网络
        let peerid_key = format!("peerid:{}", mldsa_pubkey_hex);
        let signed_peerid = match SignedIdentityRecord::sign(
            mldsa_private_key,
            &current_peer_id,
            peerid_key.as_bytes(),
            peer_id.to_string(),
        ) {
            Ok(record) => record,
            Err(e) => {
                tracing::warn!("Failed to sign PeerID record: {:?}", e);
                return;
            }
        };
        let peerid_value = match postcard::to_allocvec(&signed_peerid) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("Failed to serialize signed PeerID record: {:?}", e);
                return;
            }
        };
        let peerid_record = libp2p::kad::Record {
            key: libp2p::kad::RecordKey::new(&peerid_key),
            value: peerid_value,
            publisher: None,
            expires: None,
        };
        match self
            .swarm
            .behaviour_mut()
            .kademlia
            .put_record(peerid_record, libp2p::kad::Quorum::One)
        {
            Ok(query_id) => {
                tracing::debug!(
                    "Published signed PeerID record to DHT network for {} (query_id: {:?})",
                    &mldsa_pubkey_hex[..16],
                    query_id,
                );
            }
            Err(e) => {
                tracing::warn!(
                    "Failed to publish signed PeerID record to DHT network: {:?}",
                    e
                );
            }
        }

        // 2. 发布签名的 ML-DSA pubkey -> ML-KEM pubkey 映射到 Kademlia 网络
        if !mlkem_pubkey_hex.is_empty() {
            let mlkem_key = format!("mlkem:{}", mldsa_pubkey_hex);
            let signed_mlkem = match SignedIdentityRecord::sign(
                mldsa_private_key,
                &current_peer_id,
                mlkem_key.as_bytes(),
                mlkem_pubkey_hex.to_string(),
            ) {
                Ok(record) => record,
                Err(e) => {
                    tracing::warn!("Failed to sign ML-KEM record: {:?}", e);
                    return;
                }
            };
            let mlkem_value = match postcard::to_allocvec(&signed_mlkem) {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!("Failed to serialize signed ML-KEM record: {:?}", e);
                    return;
                }
            };
            let mlkem_record = libp2p::kad::Record {
                key: libp2p::kad::RecordKey::new(&mlkem_key),
                value: mlkem_value,
                publisher: None,
                expires: None,
            };
            match self
                .swarm
                .behaviour_mut()
                .kademlia
                .put_record(mlkem_record, libp2p::kad::Quorum::One)
            {
                Ok(query_id) => {
                    tracing::debug!(
                        "Published signed ML-KEM record to DHT network for {} (query_id: {:?})",
                        &mldsa_pubkey_hex[..16],
                        query_id,
                    );
                }
                Err(e) => {
                    tracing::warn!(
                        "Failed to publish signed ML-KEM record to DHT network: {:?}",
                        e
                    );
                }
            }
        }

        tracing::info!(
            "Published signed identity to DHT network: {} (PeerID: {}, ML-KEM: {})",
            &mldsa_pubkey_hex[..16],
            &peer_id[..8],
            &mlkem_pubkey_hex[..16],
        );
    }

    /// 通过 Kademlia get_record 从 DHT 网络查询联系人的 ML-KEM 公钥
    pub(crate) async fn query_mlkem_from_dht_network(
        &mut self,
        mldsa_pubkey_hex: &str,
    ) -> Option<Vec<u8>> {
        let mlkem_key = format!("mlkem:{}", mldsa_pubkey_hex);
        let key = libp2p::kad::RecordKey::new(&mlkem_key);

        tracing::info!(
            "Querying ML-KEM pubkey from DHT network for {}",
            &mldsa_pubkey_hex[..16]
        );

        // 注册 ML-KEM 查询回调，等待网络查询结果
        // 使用独立的 mlkem_query_callbacks，通过 oneshot channel 直接传递 ML-KEM hex 值
        // 避免先写入 redb 数据库再读取的竞态条件
        let query_id = format!("mlkem_{}", mldsa_pubkey_hex);
        let rx = p2p::register_mlkem_query_callback(query_id.clone());

        // 发起 Kademlia get_record 查询
        let _query_id = self.swarm.behaviour_mut().kademlia.get_record(key);

        // 等待结果（超时 30 秒）
        match tokio::time::timeout(std::time::Duration::from_secs(30), rx).await {
            Ok(Ok(Some(mlkem_hex))) => {
                // 直接从 oneshot channel 获取 ML-KEM hex 值，无需查询数据库
                match hex::decode(&mlkem_hex) {
                    Ok(mlkem_bytes) => {
                        tracing::info!(
                            "Found ML-KEM pubkey via DHT network for {}",
                            &mldsa_pubkey_hex[..16]
                        );
                        Some(mlkem_bytes)
                    }
                    Err(e) => {
                        tracing::warn!(
                            "Failed to decode ML-KEM hex for {}: {}",
                            &mldsa_pubkey_hex[..16],
                            e
                        );
                        None
                    }
                }
            }
            Ok(Ok(None)) => {
                tracing::info!(
                    "DHT ML-KEM query: no record found for {}",
                    &mldsa_pubkey_hex[..16]
                );
                None
            }
            Ok(Err(_)) => {
                tracing::warn!("DHT ML-KEM query cancelled for {}", &mldsa_pubkey_hex[..16]);
                None
            }
            Err(_) => {
                tracing::warn!("DHT ML-KEM query timeout for {}", &mldsa_pubkey_hex[..16]);
                // 清理回调
                p2p::mlkem_query_callbacks()
                    .lock()
                    .unwrap()
                    .remove(&query_id);
                None
            }
        }
    }
}
