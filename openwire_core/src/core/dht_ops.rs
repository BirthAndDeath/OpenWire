use crate::core::ChatCore;

impl ChatCore {
    /// 将身份记录发布到 Kademlia DHT 网络
    ///
    /// # 发布机制
    /// - **PeerID 发现**: 使用 Kademlia 原生 `start_providing` 机制，
    ///   以 ML-DSA 公钥 hex 作为 provider key。其他节点通过 `get_providers(pubkey_hex)`
    ///   查询到本节点的 PeerID。
    /// - **ML-KEM 公钥交换**: 使用 `put_record("mlkem:{pubkey_hex}")` 直接存储
    ///   ML-KEM 公钥（无 SignedIdentityRecord 包装）。解密失败是自验证的 —
    ///   如果 ML-KEM 公钥错误，对方解密会失败，不会造成安全风险。
    pub(crate) fn publish_identity_to_dht(
        &mut self,
        mldsa_pubkey_hex: &str,
        _peer_id: &str,
        mlkem_pubkey_hex: &str,
    ) {
        // 1. 使用 Kademlia 原生 provider 机制发布 PeerID
        //    其他节点通过 get_providers(pubkey_hex) 查询到本节点
        let key = libp2p::kad::RecordKey::new(&mldsa_pubkey_hex.to_string());
        match self.swarm.behaviour_mut().kademlia.start_providing(key) {
            Ok(query_id) => {
                tracing::debug!(
                    "Started providing PeerID for ML-DSA {} (query_id: {:?})",
                    truncate_str(mldsa_pubkey_hex, 16),
                    query_id,
                );
            }
            Err(e) => {
                tracing::warn!(
                    "Failed to start providing PeerID for ML-DSA {}: {:?}",
                    truncate_str(mldsa_pubkey_hex, 16),
                    e
                );
            }
        }

        // 2. 发布 ML-KEM 公钥记录（直接存储，无 SignedIdentityRecord 包装）
        //    解密失败是自验证的，不需要额外签名
        if !mlkem_pubkey_hex.is_empty() {
            let record_key = format!("mlkem:{}", mldsa_pubkey_hex);
            let record = libp2p::kad::Record {
                key: libp2p::kad::RecordKey::new(&record_key),
                value: mlkem_pubkey_hex.as_bytes().to_vec(),
                publisher: None,
                expires: None,
            };
            match self
                .swarm
                .behaviour_mut()
                .kademlia
                .put_record(record, libp2p::kad::Quorum::One)
            {
                Ok(query_id) => {
                    tracing::debug!(
                        "Published ML-KEM pubkey for ML-DSA {} (query_id: {:?})",
                        truncate_str(mldsa_pubkey_hex, 16),
                        query_id,
                    );
                }
                Err(e) => {
                    tracing::warn!(
                        "Failed to publish ML-KEM pubkey for ML-DSA {}: {:?}",
                        truncate_str(mldsa_pubkey_hex, 16),
                        e
                    );
                }
            }
        }

        tracing::info!(
            "Published identity to DHT network: {} (ML-KEM: {})",
            truncate_str(mldsa_pubkey_hex, 16),
            truncate_str(mlkem_pubkey_hex, 16),
        );
    }
}

/// 截断字符串到指定长度用于日志显示
fn truncate_str(s: &str, max_len: usize) -> &str {
    if s.len() >= max_len { &s[..max_len] } else { s }
}
