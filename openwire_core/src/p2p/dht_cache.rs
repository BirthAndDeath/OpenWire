use dashmap::DashMap;
use libp2p::PeerId;
use std::sync::Arc;

const MAX_PUBKEY_PEERID: usize = 10_000;
const MAX_MULTIADDR_PEERS: usize = 2_000;
const MAX_ADDRS_PER_PEER: usize = 20;

/// 线程安全的 DHT 缓存，存储 pubkey↔peerid、Multiaddr 映射。
pub struct DhtCache {
    pubkey_peerid: DashMap<String, String>,
    peerid_pubkey: DashMap<String, String>,
    multiaddrs: DashMap<String, Vec<String>>,
}

impl DhtCache {
    /// 创建新的 DHT 缓存实例
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            pubkey_peerid: DashMap::new(),
            peerid_pubkey: DashMap::new(),
            multiaddrs: DashMap::new(),
        })
    }

    /// 设置 ML-DSA 公钥 → PeerID 映射（双向索引）
    pub fn set_pubkey_peerid(&self, pubkey_hex: &str, peer_id: &PeerId) {
        let peer_id_str = peer_id.to_string();
        if self.pubkey_peerid.len() >= MAX_PUBKEY_PEERID && !self.pubkey_peerid.contains_key(pubkey_hex)
            && let Some(entry) = self.pubkey_peerid.iter().next() {
                let old_key = entry.key().clone();
                let old_val = entry.value().clone();
                drop(entry);
                self.pubkey_peerid.remove(&old_key);
                self.peerid_pubkey.remove(&old_val);
                tracing::debug!("DHT pubkey_peerid cache at capacity, evicted oldest entry");
            }
        if let Some(old) = self.pubkey_peerid.get(pubkey_hex) {
            self.peerid_pubkey.remove(old.value());
        }
        self.pubkey_peerid.insert(pubkey_hex.to_string(), peer_id_str.clone());
        self.peerid_pubkey.insert(peer_id_str, pubkey_hex.to_string());
    }

    /// 通过 ML-DSA 公钥查询 PeerID
    pub fn get_peerid_by_pubkey(&self, pubkey_hex: &str) -> Option<PeerId> {
        self.pubkey_peerid.get(pubkey_hex).and_then(|v| v.value().parse::<PeerId>().ok())
    }

    /// 通过 PeerID 反查 ML-DSA 公钥
    pub fn get_pubkey_by_peerid(&self, peer_id: &PeerId) -> Option<String> {
        self.peerid_pubkey.get(&peer_id.to_string()).map(|v| v.clone())
    }

    /// 删除 ML-DSA 公钥 → PeerID 映射（同时清理反向索引）
    pub fn remove_pubkey_peerid(&self, pubkey_hex: &str) {
        if let Some((_, peer_id)) = self.pubkey_peerid.remove(pubkey_hex) {
            self.peerid_pubkey.remove(&peer_id);
        }
    }

    /// 获取所有缓存的 ML-DSA 公钥列表
    pub fn get_all_pubkeys(&self) -> Vec<String> {
        self.pubkey_peerid.iter().map(|entry| entry.key().clone()).collect()
    }

    /// 添加 peer 的 Multiaddr 到缓存
    pub fn add_multiaddr(&self, peer_id: &PeerId, multiaddr: &libp2p::Multiaddr) {
        let key = peer_id.to_string();
        let addr_str = multiaddr.to_string();
        if !self.multiaddrs.contains_key(&key) && self.multiaddrs.len() >= MAX_MULTIADDR_PEERS {
            tracing::warn!("DHT multiaddrs cache at capacity (max={}), skipping peer {}", MAX_MULTIADDR_PEERS, key);
            return;
        }
        let mut addrs = self.multiaddrs.entry(key).or_default();
        if !addrs.contains(&addr_str) && addrs.len() < MAX_ADDRS_PER_PEER {
            addrs.push(addr_str);
        }
    }

    /// 从缓存中删除 peer 的某个 Multiaddr
    pub fn remove_multiaddr(&self, peer_id: &PeerId, multiaddr: &libp2p::Multiaddr) {
        let key = peer_id.to_string();
        let addr_str = multiaddr.to_string();
        if let Some(mut entry) = self.multiaddrs.get_mut(&key) {
            entry.retain(|a| a != &addr_str);
            if entry.is_empty() {
                drop(entry);
                self.multiaddrs.remove(&key);
            }
        }
    }

    /// 获取 peer 缓存的所有 Multiaddr
    pub fn get_multiaddrs(&self, peer_id: &PeerId) -> Vec<libp2p::Multiaddr> {
        let key = peer_id.to_string();
        self.multiaddrs.get(&key).map_or(Vec::new(), |addrs| {
            addrs.value().iter().filter_map(|s| s.parse().ok()).collect()
        })
    }
}