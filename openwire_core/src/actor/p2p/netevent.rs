//! NetEvent 处理逻辑
//!
//! 处理 NetEvent 协议的请求和响应：
//! - 收到 FriendOnline 通知后，通过 DHT 查询获取对方的最新信息
//! - 连接建立后，主动向对方发送 FriendOnline 通知

use libp2p::kad;
use libp2p::PeerId;
use std::sync::Arc;

use crate::p2p::dht::RedbRecordStore;
use crate::p2p::netevent::NetEventRequest;

/// 处理收到的 FriendOnline 通知
///
/// 当收到好友上线通知时，触发 DHT 查询以获取对方的最新信息：
/// 1. GetProviders 查询对方的 PeerID（如果尚未缓存）
/// 2. GetRecord 查询对方的 ML-KEM 公钥（如果尚未缓存）
pub fn handle_friend_online(
    kademlia: &mut kad::Behaviour<RedbRecordStore>,
    request: &NetEventRequest,
    store: Option<Arc<redb::Database>>,
) {
    // 匹配枚举变体以获取字段
    let (mldsa_pubkey_hex, peer_id_str, listen_addrs) = match request {
        NetEventRequest::FriendOnline {
            mldsa_pubkey_hex,
            peer_id,
            listen_addrs,
        } => (mldsa_pubkey_hex, peer_id, listen_addrs),
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

    // 将对方的多地址添加到 Kademlia 路由表
    for addr_str in listen_addrs {
        if let Ok(addr) = addr_str.parse::<libp2p::Multiaddr>() {
            if let Ok(peer_id) = peer_id_str.parse::<PeerId>() {
                kademlia.add_address(&peer_id, addr);
            }
        }
    }

    // 如果本地 DHT 数据库可用，缓存对方的信息
    if let Some(ref db) = store {
        let record_store = RedbRecordStore::new(db.clone());

        // 缓存 PeerID 映射
        if let Ok(peer_id) = peer_id_str.parse::<PeerId>() {
            let _ = record_store.set_pubkey_peerid(mldsa_pubkey_hex, &peer_id);
        }

        // 检查是否已有 ML-KEM 公钥，如果没有则发起 DHT 查询
        let has_mlkem = record_store
            .get_mlkem_pubkey(mldsa_pubkey_hex)
            .ok()
            .flatten()
            .map(|s| !s.is_empty())
            .unwrap_or(false);

        if !has_mlkem {
            let mlkem_key = format!("mlkem:{}", mldsa_pubkey_hex);
            let key = libp2p::kad::RecordKey::new(&mlkem_key);
            let _query_id = kademlia.get_record(key);
            tracing::debug!(
                "已发起 ML-KEM 公钥 DHT 查询: {}..",
                pubkey_short
            );
        }
    }

    // 发起 GetProviders 查询以确认对方的 PeerID
    let key = libp2p::kad::RecordKey::new(&mldsa_pubkey_hex);
    let _query_id = kademlia.get_providers(key);
    tracing::debug!(
        "已发起 GetProviders 查询: {}..",
        pubkey_short
    );
}

/// 构建 FriendOnline 请求
///
/// 当连接建立时，主动通知对方自己的身份信息。
pub fn build_friend_online_request(
    mldsa_pubkey_hex: &str,
    peer_id: &PeerId,
    listen_addrs: &[libp2p::Multiaddr],
) -> NetEventRequest {
    NetEventRequest::FriendOnline {
        mldsa_pubkey_hex: mldsa_pubkey_hex.to_string(),
        peer_id: peer_id.to_string(),
        listen_addrs: listen_addrs.iter().map(|a| a.to_string()).collect(),
    }
}
