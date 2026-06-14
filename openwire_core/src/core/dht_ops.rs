use crate::actor::p2p::P2pCommand;
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
    pub(crate) async fn publish_identity_to_dht(
        &mut self,
        mldsa_pubkey_hex: &str,
        _peer_id: &str,
        mlkem_pubkey_hex: &str,
    ) {
        // 通过 P2pActor 发布身份到 DHT
        let _ = self.p2p_handle.send(
            crate::actor::ActorCommand::Custom(P2pCommand::PublishIdentity {
                mldsa_pubkey_hex: mldsa_pubkey_hex.to_string(),
                mlkem_pubkey_hex: mlkem_pubkey_hex.to_string(),
            }),
        ).await;

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
