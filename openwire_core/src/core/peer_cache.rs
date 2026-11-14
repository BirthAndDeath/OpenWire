use std::collections::HashMap;
use std::sync::Arc;

use libp2p::PeerId;

use crate::p2p::dht_cache::DhtCache;

/// DHT 查询键映射上限（防御性，联系人数量正常远小于此值）
const MAX_DHT_KEY_PUBKEY_ENTRIES: usize = 10_000;
/// 缓存 PeerID 映射上限（防御性，条目数=当前连接数+已映射的节点数）
const MAX_PEERID_ENTRIES: usize = 10_000;

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

    /// 插入 DHT 查询键 → 公钥映射，超过上限时淘汰一条（防御性限界）
    pub fn cache_dht_key(&mut self, key: String, pubkey: String) {
        if !self.dht_key_to_pubkey.contains_key(&key)
            && self.dht_key_to_pubkey.len() >= MAX_DHT_KEY_PUBKEY_ENTRIES
            && let Some(arbitrary) = self.dht_key_to_pubkey.keys().next().cloned()
        {
            // HashMap 不保序，keys().next() 是任意键而非最旧条目，仅作容量兜底
            self.dht_key_to_pubkey.remove(&arbitrary);
        }
        self.dht_key_to_pubkey.insert(key, pubkey);
    }

    /// 缓存 PeerID → ML-DSA 公钥映射，返回是否新映射。
    /// 如果该 PeerID 当前已连接，由调用方决定是否刷新在线状态。
    pub fn cache_pubkey(&mut self, pid: PeerId, pubkey: String) -> bool {
        self.peerid_to_pubkey.insert(pid, pubkey).is_none()
    }

    /// 缓存 PeerID → ML-KEM 公钥，超过上限时淘汰一条（防御性限界）
    pub fn cache_mlkem(&mut self, pid: PeerId, mlkem_hex: String) {
        if !self.peerid_to_mlkem.contains_key(&pid)
            && self.peerid_to_mlkem.len() >= MAX_PEERID_ENTRIES
            && let Some(arbitrary) = self.peerid_to_mlkem.keys().next().copied()
        {
            // HashMap 不保序，keys().next() 是任意键而非最旧条目，仅作容量兜底
            self.peerid_to_mlkem.remove(&arbitrary);
        }
        self.peerid_to_mlkem.insert(pid, mlkem_hex);
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