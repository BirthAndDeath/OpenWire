//! NetEvent 处理逻辑
//!
//! 处理 NetEvent 协议的请求和响应：
//! - 连接建立后，主动向对方发送 FriendOnline 通知

use libp2p::PeerId;

use crate::p2p::netevent::NetEventRequest;

/// 构建 FriendOnline 请求。
///
/// 当连接建立时，主动通知对方自己的身份信息。
/// 对身份声明添加 ML-DSA 签名，使接收端能验证发送方确实持有该公钥对应的私钥。
///
/// 返回 `None` 表示签名失败（不应发送此请求，否则接收端会返回 `Nack`）。
pub fn build_friend_online_request(
    mldsa_pubkey_hex: &str,
    peer_id: &PeerId,
    listen_addrs: &[libp2p::Multiaddr],
    mlkem_pubkey_hex: &str,
    mldsa_private_key: &[u8],
) -> Option<NetEventRequest> {
    let listen_strs: Vec<String> = listen_addrs.iter().map(|a| a.to_string()).collect();
    let payload = crate::p2p::netevent::friend_online_payload(
        mldsa_pubkey_hex,
        &peer_id.to_string(),
        &listen_strs,
        mlkem_pubkey_hex,
    );
    let signature = match crate::signature::sign_data(mldsa_private_key, &payload) {
        Ok(sig) => sig,
        Err(e) => {
            tracing::warn!("FriendOnline 身份签名失败: {e}");
            return None;
        }
    };
    Some(NetEventRequest::FriendOnline {
        version: Some(crate::p2p::netevent::NETEVENT_VERSION),
        mldsa_pubkey_hex: mldsa_pubkey_hex.to_string(),
        peer_id: peer_id.to_string(),
        listen_addrs: listen_strs,
        mlkem_pubkey_hex: mlkem_pubkey_hex.to_string(),
        signature: Some(signature),
    })
}

/// 构建 FriendOffline 请求（设备下线通知）。
///
/// 本设备优雅退出前调用：通知对端"该设备已下线"。`remaining_peers` 为尽力提供的
/// 该账户其余在线设备 PeerID（空 = 尽力认为全部下线；对端以本地设备集合为准）。
///
/// 返回 `None` 表示签名失败（不应发送此请求，否则接收端会返回 `Nack`）。
pub fn build_friend_offline_request(
    mldsa_pubkey_hex: &str,
    peer_id: &PeerId,
    remaining_peers: &[String],
    mldsa_private_key: &[u8],
) -> Option<NetEventRequest> {
    let payload = crate::p2p::netevent::friend_offline_payload(
        mldsa_pubkey_hex,
        &peer_id.to_string(),
        remaining_peers,
    );
    let signature = match crate::signature::sign_data(mldsa_private_key, &payload) {
        Ok(sig) => sig,
        Err(e) => {
            tracing::warn!("FriendOffline 身份签名失败: {e}");
            return None;
        }
    };
    Some(NetEventRequest::FriendOffline {
        version: Some(crate::p2p::netevent::NETEVENT_VERSION),
        mldsa_pubkey_hex: mldsa_pubkey_hex.to_string(),
        peer_id: peer_id.to_string(),
        remaining_peers: remaining_peers.to_vec(),
        signature: Some(signature),
    })
}