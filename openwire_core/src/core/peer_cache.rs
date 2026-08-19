use std::collections::HashMap;
use std::sync::Arc;

use libp2p::PeerId;

use crate::p2p::dht_cache::DhtCache;

pub(crate) struct PeerCache {
    pub dht: Arc<DhtCache>,
    pub peerid_to_pubkey: HashMap<PeerId, String>,
    pub peerid_to_mlkem: HashMap<PeerId, String>,
    pub dht_key_to_pubkey: HashMap<String, String>,
}

impl PeerCache {
    pub fn new(dht: Arc<DhtCache>) -> Self {
        Self {
            dht,
            peerid_to_pubkey: HashMap::new(),
            peerid_to_mlkem: HashMap::new(),
            dht_key_to_pubkey: HashMap::new(),
        }
    }

    pub fn dht(&self) -> &DhtCache {
        &self.dht
    }

    /// 缓存 PeerID → ML-DSA 公钥映射，返回是否新映射。
    /// 如果该 PeerID 当前已连接，由调用方决定是否刷新在线状态。
    pub fn cache_pubkey(&mut self, pid: PeerId, pubkey: String) -> bool {
        self.peerid_to_pubkey.insert(pid, pubkey).is_none()
    }

    /// 解析已连接 PeerID 对应的 ML-DSA 公钥列表
    pub fn resolve_online(&self, connected: &HashMap<PeerId, usize>) -> Vec<String> {
        let mut online = Vec::with_capacity(connected.len());
        for pid in connected.keys() {
            if let Some(pubkey) = self.peerid_to_pubkey.get(pid) {
                online.push(pubkey.clone());
            } else if let Some(pubkey) = self.dht.get_pubkey_by_peerid(pid) {
                online.push(pubkey);
            }
        }
        online
    }
}