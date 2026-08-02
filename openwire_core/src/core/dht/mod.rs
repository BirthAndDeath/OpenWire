use crate::actor::p2p::P2pCommand;
use crate::core::ChatCore;

impl ChatCore {
    /// 将身份发布到 Kademlia DHT 网络
    ///
    /// 使用 SHA256(ML-DSA 公钥) 作为 provider key 隐藏原始公钥。
    /// ML-KEM 公钥不再存入 DHT，改为通过 FriendOnline 直接传递。
    pub(crate) async fn publish_identity_to_dht(&mut self, mldsa_pubkey_hex: &str) {
        if let Err(e) = self.p2p_handle.tx.try_send(
            P2pCommand::PublishIdentity {
                mldsa_pubkey_hex: mldsa_pubkey_hex.to_string(),
            },
        ) {
            tracing::warn!("Failed to publish identity via P2pActor: {e:?}");
            self.send_warning_mpsc(format!("DHT 身份发布失败: {e}")).await;
        } else {
            tracing::info!(
                "Published identity to DHT network: {}",
                &mldsa_pubkey_hex[..16.min(mldsa_pubkey_hex.len())],
            );
        }
    }
}