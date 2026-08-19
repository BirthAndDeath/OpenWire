//！DHT 本地缓存查询

use crate::p2p::dht_cache::DhtCache;
use libp2p::PeerId;
use std::sync::Arc;

/// 从本地 DHT 缓存查询 ML-DSA 公钥对应的 PeerID
///
/// 此函数仅查询本地内存缓存，不发起网络 DHT 查询。
pub fn lookup_peerid_by_pubkey(
    cache: &Arc<DhtCache>,
    pubkey_hex: &str,
) -> Option<PeerId> {
    cache.get_peerid_by_pubkey(pubkey_hex)
}