use crate::{core::ChatCore, signature::SignedIdentityRecord};

impl ChatCore {
    /// 将身份记录发布到 Kademlia DHT 网络
    pub(crate) fn publish_identity_to_dht(
        &mut self,
        mldsa_pubkey_hex: &str,
        peer_id: &str,
        mlkem_pubkey_hex: &str,
    ) {
        let mldsa_private_key = match self.mldsa_private_key.as_ref() {
            Some(key) => key.clone(),
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

        // 1. 发布签名的 PeerID 记录
        self.publish_signed_record(
            &mldsa_private_key,
            &current_peer_id,
            &format!("peerid:{}", mldsa_pubkey_hex),
            peer_id.to_string(),
            "PeerID",
        );

        // 2. 发布签名的 ML-KEM 公钥记录
        if !mlkem_pubkey_hex.is_empty() {
            self.publish_signed_record(
                &mldsa_private_key,
                &current_peer_id,
                &format!("mlkem:{}", mldsa_pubkey_hex),
                mlkem_pubkey_hex.to_string(),
                "ML-KEM",
            );
        }

        tracing::info!(
            "Published signed identity to DHT network: {} (PeerID: {}, ML-KEM: {})",
            truncate_str(mldsa_pubkey_hex, 16),
            truncate_str(peer_id, 8),
            truncate_str(mlkem_pubkey_hex, 16),
        );
    }

    /// 发布签名的 DHT 记录
    fn publish_signed_record(
        &mut self,
        private_key: &[u8],
        current_peer_id: &libp2p::PeerId,
        key_str: &str,
        value: String,
        label: &str,
    ) {
        let signed = match SignedIdentityRecord::sign(
            private_key,
            current_peer_id,
            key_str.as_bytes(),
            value,
        ) {
            Ok(record) => record,
            Err(e) => {
                tracing::warn!("Failed to sign {} record: {:?}", label, e);
                return;
            }
        };
        let serialized = match postcard::to_allocvec(&signed) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("Failed to serialize signed {} record: {:?}", label, e);
                return;
            }
        };
        let record = libp2p::kad::Record {
            key: libp2p::kad::RecordKey::new(&key_str.to_string()),
            value: serialized,
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
                    "Published signed {} record to DHT network (query_id: {:?})",
                    label,
                    query_id,
                );
            }
            Err(e) => {
                tracing::warn!(
                    "Failed to publish signed {} record to DHT network: {:?}",
                    label,
                    e
                );
            }
        }
    }
}

/// 截断字符串到指定长度用于日志显示
fn truncate_str(s: &str, max_len: usize) -> &str {
    if s.len() >= max_len { &s[..max_len] } else { s }
}
