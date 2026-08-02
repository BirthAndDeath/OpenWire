//! NetEvent 处理逻辑
//!
//! 处理 NetEvent 协议的请求和响应：
//! - 收到 FriendOnline 通知后，通过 DHT 查询获取对方的最新信息
//! - 连接建立后，主动向对方发送 FriendOnline 通知
//!
//! 注意：handle_friend_online 已停用，因为 RedbRecordStore 不再作为 Kademlia 储存使用。
//! FriendOnline 的实际处理在 event_loop.rs 中直接操作 swarm。

use libp2p::PeerId;
#[cfg(feature = "redb_dht")]
use std::sync::Arc;

use crate::p2p::netevent::NetEventRequest;

/// 处理收到的 FriendOnline 通知
///
/// 已停用：RedbRecordStore 不再作为 Kademlia 储存使用。
#[cfg(feature = "redb_dht")]
#[allow(dead_code)]
pub fn handle_friend_online(
    request: &NetEventRequest,
    store: Option<Arc<redb::Database>>,
) {
    let (mldsa_pubkey_hex, peer_id_str, listen_addrs) = match request {
        NetEventRequest::FriendOnline {
            mldsa_pubkey_hex,
            peer_id,
            listen_addrs,
            mlkem_pubkey_hex: _,
        } => (mldsa_pubkey_hex, peer_id, listen_addrs),
        _ => return,
    };

    let pubkey_short = if mldsa_pubkey_hex.len() >= 16 {
        &mldsa_pubkey_hex[..16]
    } else {
        mldsa_pubkey_hex
    };

    tracing::info!(
        "收到好友上线通知: {}.., PeerID={}",
        pubkey_short,
        peer_id_str
    );

    if let Some(ref db) = store {
        let record_store = crate::server_redb_store::RedbRecordStore::new(db.clone());

        if let Ok(peer_id) = peer_id_str.parse::<PeerId>() {
            let _ = record_store.set_pubkey_peerid(mldsa_pubkey_hex, &peer_id);
        }
        // ML-KEM 公钥不再通过 DHT 缓存，改为 FriendOnline 直接携带
    }
}

/// 构建 FriendOnline 请求
///
/// 当连接建立时，主动通知对方自己的身份信息。
pub fn build_friend_online_request(
    mldsa_pubkey_hex: &str,
    peer_id: &PeerId,
    listen_addrs: &[libp2p::Multiaddr],
    mlkem_pubkey_hex: &str,
) -> NetEventRequest {
    NetEventRequest::FriendOnline {
        mldsa_pubkey_hex: mldsa_pubkey_hex.to_string(),
        peer_id: peer_id.to_string(),
        listen_addrs: listen_addrs.iter().map(|a| a.to_string()).collect(),
        mlkem_pubkey_hex: mlkem_pubkey_hex.to_string(),
    }
}