use libp2p::PeerId;
use std::sync::Arc;

use crate::error::DhtResult;

#[cfg(feature = "mem_dht")]
use dashmap::DashMap;

const MAX_PUBKEY_PEERID: usize = 10_000;
const MAX_MULTIADDR_PEERS: usize = 2_000;
const MAX_ADDRS_PER_PEER: usize = 20;

/// 线程安全的 DHT 缓存，存储 pubkey↔peerid、Multiaddr 映射。
///
/// ML-KEM 公钥不再缓存于 DHT，改为通过 FriendOnline 直接传递，
/// 存储在 ChatCore::peerid_to_mlkem 中。
///
/// 内部实现由 `mem_dht` 特性开关决定：
/// * `mem_dht` 启用 → 使用 `DashMap`（无锁并发）
/// * `mem_dht` 禁用 → 使用 `Mutex<HashMap>`（轻量回退）
pub struct DhtCache {
    #[cfg(feature = "mem_dht")]
    pubkey_peerid: DashMap<String, String>,
    #[cfg(not(feature = "mem_dht"))]
    pubkey_peerid: std::sync::Mutex<std::collections::HashMap<String, String>>,

    // Reverse index: PeerID → ML-DSA pubkey hex, for O(1) reverse lookups
    #[cfg(feature = "mem_dht")]
    peerid_pubkey: DashMap<String, String>,
    #[cfg(not(feature = "mem_dht"))]
    peerid_pubkey: std::sync::Mutex<std::collections::HashMap<String, String>>,

    #[cfg(feature = "mem_dht")]
    multiaddrs: DashMap<String, Vec<String>>,
    #[cfg(not(feature = "mem_dht"))]
    multiaddrs: std::sync::Mutex<std::collections::HashMap<String, Vec<String>>>,
}

impl DhtCache {
    /// 创建新的 DHT 缓存实例（返回 `Arc` 包装）
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            #[cfg(feature = "mem_dht")]
            pubkey_peerid: DashMap::new(),
            #[cfg(not(feature = "mem_dht"))]
            pubkey_peerid: std::sync::Mutex::new(std::collections::HashMap::new()),
            #[cfg(feature = "mem_dht")]
            peerid_pubkey: DashMap::new(),
            #[cfg(not(feature = "mem_dht"))]
            peerid_pubkey: std::sync::Mutex::new(std::collections::HashMap::new()),
            #[cfg(feature = "mem_dht")]
            multiaddrs: DashMap::new(),
            #[cfg(not(feature = "mem_dht"))]
            multiaddrs: std::sync::Mutex::new(std::collections::HashMap::new()),
        })
    }

    /// 设置 ML-DSA 公钥 → PeerID 映射（双向，O(1) 正查和反查）
    ///
    /// 达到容量上限时，自动淘汰一条最旧的条目（LRU 近似策略）。
    pub fn set_pubkey_peerid(&self, pubkey_hex: &str, peer_id: &PeerId) -> DhtResult<()> {
        let peer_id_str = peer_id.to_string();
        #[cfg(feature = "mem_dht")]
        {
            // Evict one entry if at capacity and this is a new key
            if self.pubkey_peerid.len() >= MAX_PUBKEY_PEERID && !self.pubkey_peerid.contains_key(pubkey_hex)
                && let Some(entry) = self.pubkey_peerid.iter().next() {
                    let old_key = entry.key().clone();
                    let old_val = entry.value().clone();
                    drop(entry);
                    self.pubkey_peerid.remove(&old_key);
                    self.peerid_pubkey.remove(&old_val);
                    tracing::debug!("DHT pubkey_peerid cache at capacity, evicted oldest entry");
                }
            // Remove old reverse entry if this pubkey already had a different peer_id
            if let Some(old) = self.pubkey_peerid.get(pubkey_hex) {
                self.peerid_pubkey.remove(old.value());
            }
            self.pubkey_peerid.insert(pubkey_hex.to_string(), peer_id_str.clone());
            self.peerid_pubkey.insert(peer_id_str, pubkey_hex.to_string());
        }
        #[cfg(not(feature = "mem_dht"))]
        {
            let mut map = self.pubkey_peerid.lock().unwrap_or_else(|e| e.into_inner());
            let mut rev = self.peerid_pubkey.lock().unwrap_or_else(|e| e.into_inner());
            if map.len() >= MAX_PUBKEY_PEERID && !map.contains_key(pubkey_hex) {
                // Evict oldest entry
                if let Some(old_key) = map.keys().next().cloned() {
                    if let Some(old_val) = map.remove(&old_key) {
                        rev.remove(&old_val);
                    }
                    tracing::debug!("DHT pubkey_peerid cache at capacity, evicted oldest entry");
                }
            }
            // Remove old reverse entry if this pubkey already had a different peer_id
            if let Some(old_peer_id) = map.get(pubkey_hex) {
                rev.remove(old_peer_id);
            }
            map.insert(pubkey_hex.to_string(), peer_id_str.clone());
            rev.insert(peer_id_str, pubkey_hex.to_string());
        }
        Ok(())
    }

    /// 通过 ML-DSA 公钥查询 PeerID
    pub fn get_peerid_by_pubkey(&self, pubkey_hex: &str) -> DhtResult<Option<PeerId>> {
        #[cfg(feature = "mem_dht")]
        {
            let result = self.pubkey_peerid.get(pubkey_hex).and_then(|v| v.value().parse::<PeerId>().ok());
            Ok(result)
        }
        #[cfg(not(feature = "mem_dht"))]
        {
            let map = self.pubkey_peerid.lock().unwrap_or_else(|e| e.into_inner());
            Ok(map.get(pubkey_hex).and_then(|v| v.parse::<PeerId>().ok()))
        }
    }

    /// 通过 PeerID 反查 ML-DSA 公钥（O(1)，使用双向索引）
    pub fn get_pubkey_by_peerid(&self, peer_id: &PeerId) -> DhtResult<Option<String>> {
        let peer_id_str = peer_id.to_string();
        #[cfg(feature = "mem_dht")]
        {
            Ok(self.peerid_pubkey.get(&peer_id_str).map(|v| v.clone()))
        }
        #[cfg(not(feature = "mem_dht"))]
        {
            let rev = self.peerid_pubkey.lock().unwrap_or_else(|e| e.into_inner());
            Ok(rev.get(&peer_id_str).cloned())
        }
    }

    /// 删除 ML-DSA 公钥 → PeerID 映射（同时清理双向索引）
    pub fn remove_pubkey_peerid(&self, pubkey_hex: &str) -> DhtResult<()> {
        #[cfg(feature = "mem_dht")]
        {
            if let Some((_, peer_id)) = self.pubkey_peerid.remove(pubkey_hex) {
                self.peerid_pubkey.remove(&peer_id);
            }
        }
        #[cfg(not(feature = "mem_dht"))]
        {
            let mut map = self.pubkey_peerid.lock().unwrap_or_else(|e| e.into_inner());
            let mut rev = self.peerid_pubkey.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(peer_id) = map.remove(pubkey_hex) {
                rev.remove(&peer_id);
            }
        }
        Ok(())
    }

    /// 获取所有缓存的 ML-DSA 公钥列表
    pub fn get_all_pubkeys(&self) -> DhtResult<Vec<String>> {
        #[cfg(feature = "mem_dht")]
        {
            Ok(self.pubkey_peerid.iter().map(|entry| entry.key().clone()).collect())
        }
        #[cfg(not(feature = "mem_dht"))]
        {
            let map = self.pubkey_peerid.lock().unwrap_or_else(|e| e.into_inner());
            Ok(map.keys().cloned().collect())
        }
    }

    /// 添加 peer 的 Multiaddr 到缓存
    pub fn add_multiaddr(&self, peer_id: &PeerId, multiaddr: &libp2p::Multiaddr) -> DhtResult<()> {
        let key = peer_id.to_string();
        let addr_str = multiaddr.to_string();
        #[cfg(feature = "mem_dht")]
        {
            if !self.multiaddrs.contains_key(&key) && self.multiaddrs.len() >= MAX_MULTIADDR_PEERS {
                tracing::warn!("DHT multiaddrs cache at capacity (max={}), skipping peer {}", MAX_MULTIADDR_PEERS, key);
                return Ok(());
            }
            let mut addrs = self.multiaddrs.entry(key).or_default();
            if !addrs.contains(&addr_str) && addrs.len() < MAX_ADDRS_PER_PEER {
                addrs.push(addr_str);
            }
        }
        #[cfg(not(feature = "mem_dht"))]
        {
            let mut map = self.multiaddrs.lock().unwrap_or_else(|e| e.into_inner());
            if !map.contains_key(&key) && map.len() >= MAX_MULTIADDR_PEERS {
                tracing::warn!("DHT multiaddrs cache at capacity (max={}), skipping peer {}", MAX_MULTIADDR_PEERS, key);
                return Ok(());
            }
            let addrs = map.entry(key).or_default();
            if !addrs.contains(&addr_str) && addrs.len() < MAX_ADDRS_PER_PEER {
                addrs.push(addr_str);
            }
        }
        Ok(())
    }

    /// 从缓存中删除 peer 的某个 Multiaddr
    pub fn remove_multiaddr(&self, peer_id: &PeerId, multiaddr: &libp2p::Multiaddr) -> DhtResult<()> {
        let key = peer_id.to_string();
        let addr_str = multiaddr.to_string();
        #[cfg(feature = "mem_dht")]
        {
            if let Some(mut entry) = self.multiaddrs.get_mut(&key) {
                entry.retain(|a| a != &addr_str);
                if entry.is_empty() {
                    drop(entry);
                    self.multiaddrs.remove(&key);
                }
            }
        }
        #[cfg(not(feature = "mem_dht"))]
        {
            let mut map = self.multiaddrs.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(addrs) = map.get_mut(&key) {
                addrs.retain(|a| a != &addr_str);
                if addrs.is_empty() {
                    map.remove(&key);
                }
            }
        }
        Ok(())
    }

    /// 获取 peer 缓存的所有 Multiaddr
    pub fn get_multiaddrs(&self, peer_id: &PeerId) -> DhtResult<Vec<libp2p::Multiaddr>> {
        let key = peer_id.to_string();
        #[cfg(feature = "mem_dht")]
        {
            Ok(self.multiaddrs.get(&key).map_or(Vec::new(), |addrs| {
                addrs.value().iter().filter_map(|s| s.parse().ok()).collect()
            }))
        }
        #[cfg(not(feature = "mem_dht"))]
        {
            let map = self.multiaddrs.lock().unwrap();
            Ok(map.get(&key).map_or(Vec::new(), |addrs| {
                addrs.iter().filter_map(|s| s.parse().ok()).collect()
            }))
        }
    }
}