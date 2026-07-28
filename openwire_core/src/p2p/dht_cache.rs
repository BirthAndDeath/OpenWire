use libp2p::PeerId;
use std::sync::Arc;

use crate::error::DhtResult;

#[cfg(feature = "mem_dht")]
use dashmap::DashMap;

const MAX_PUBKEY_PEERID: usize = 10_000;
const MAX_MLKEM_KEYS: usize = 5_000;
const MAX_MULTIADDR_PEERS: usize = 2_000;
const MAX_ADDRS_PER_PEER: usize = 20;

/// 线程安全的 DHT 缓存，存储 pubkey↔peerid、ML-KEM 公钥、Multiaddr 映射。
///
/// 内部实现由 `mem_dht` 特性开关决定：
/// * `mem_dht` 启用 → 使用 `DashMap`（无锁并发）
/// * `mem_dht` 禁用 → 使用 `Mutex<HashMap>`（轻量回退）
pub struct DhtCache {
    #[cfg(feature = "mem_dht")]
    pubkey_peerid: DashMap<String, String>,
    #[cfg(not(feature = "mem_dht"))]
    pubkey_peerid: std::sync::Mutex<std::collections::HashMap<String, String>>,

    #[cfg(feature = "mem_dht")]
    mlkem_pubkeys: DashMap<String, String>,
    #[cfg(not(feature = "mem_dht"))]
    mlkem_pubkeys: std::sync::Mutex<std::collections::HashMap<String, String>>,

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
            mlkem_pubkeys: DashMap::new(),
            #[cfg(not(feature = "mem_dht"))]
            mlkem_pubkeys: std::sync::Mutex::new(std::collections::HashMap::new()),
            #[cfg(feature = "mem_dht")]
            multiaddrs: DashMap::new(),
            #[cfg(not(feature = "mem_dht"))]
            multiaddrs: std::sync::Mutex::new(std::collections::HashMap::new()),
        })
    }

    /// 设置 ML-DSA 公钥 → PeerID 映射
    pub fn set_pubkey_peerid(&self, pubkey_hex: &str, peer_id: &PeerId) -> DhtResult<()> {
        let val = peer_id.to_string();
        #[cfg(feature = "mem_dht")]
        {
            if self.pubkey_peerid.len() >= MAX_PUBKEY_PEERID && !self.pubkey_peerid.contains_key(pubkey_hex) {
                tracing::warn!("DHT pubkey_peerid cache at capacity (max={}), skipping {}", MAX_PUBKEY_PEERID, pubkey_hex);
                return Ok(());
            }
            self.pubkey_peerid.insert(pubkey_hex.to_string(), val);
        }
        #[cfg(not(feature = "mem_dht"))]
        {
            let mut map = self.pubkey_peerid.lock().unwrap();
            if map.len() >= MAX_PUBKEY_PEERID && !map.contains_key(pubkey_hex) {
                tracing::warn!("DHT pubkey_peerid cache at capacity (max={}), skipping {}", MAX_PUBKEY_PEERID, pubkey_hex);
                return Ok(());
            }
            map.insert(pubkey_hex.to_string(), val);
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
            let map = self.pubkey_peerid.lock().unwrap();
            Ok(map.get(pubkey_hex).and_then(|v| v.parse::<PeerId>().ok()))
        }
    }

    /// 通过 PeerID 反查 ML-DSA 公钥
    pub fn get_pubkey_by_peerid(&self, peer_id: &PeerId) -> DhtResult<Option<String>> {
        let peer_id_str = peer_id.to_string();
        #[cfg(feature = "mem_dht")]
        {
            let result = self.pubkey_peerid.iter().find(|entry| entry.value() == &peer_id_str).map(|entry| entry.key().clone());
            Ok(result)
        }
        #[cfg(not(feature = "mem_dht"))]
        {
            let map = self.pubkey_peerid.lock().unwrap();
            Ok(map.iter().find(|(_, v)| *v == &peer_id_str).map(|(k, _)| k.clone()))
        }
    }

    /// 删除 ML-DSA 公钥 → PeerID 映射
    pub fn remove_pubkey_peerid(&self, pubkey_hex: &str) -> DhtResult<()> {
        #[cfg(feature = "mem_dht")]
        { self.pubkey_peerid.remove(pubkey_hex); }
        #[cfg(not(feature = "mem_dht"))]
        { self.pubkey_peerid.lock().unwrap().remove(pubkey_hex); }
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
            let map = self.pubkey_peerid.lock().unwrap();
            Ok(map.keys().cloned().collect())
        }
    }

    /// 设置 ML-DSA 公钥 → ML-KEM 公钥映射
    pub fn set_mlkem_pubkey(&self, mldsa_pubkey_hex: &str, mlkem_pubkey_hex: &str) -> DhtResult<()> {
        #[cfg(feature = "mem_dht")]
        {
            if self.mlkem_pubkeys.len() >= MAX_MLKEM_KEYS && !self.mlkem_pubkeys.contains_key(mldsa_pubkey_hex) {
                tracing::warn!("DHT mlkem_pubkeys cache at capacity (max={}), skipping {}", MAX_MLKEM_KEYS, mldsa_pubkey_hex);
                return Ok(());
            }
            self.mlkem_pubkeys.insert(mldsa_pubkey_hex.to_string(), mlkem_pubkey_hex.to_string());
        }
        #[cfg(not(feature = "mem_dht"))]
        {
            let mut map = self.mlkem_pubkeys.lock().unwrap();
            if map.len() >= MAX_MLKEM_KEYS && !map.contains_key(mldsa_pubkey_hex) {
                tracing::warn!("DHT mlkem_pubkeys cache at capacity (max={}), skipping {}", MAX_MLKEM_KEYS, mldsa_pubkey_hex);
                return Ok(());
            }
            map.insert(mldsa_pubkey_hex.to_string(), mlkem_pubkey_hex.to_string());
        }
        Ok(())
    }

    /// 通过 ML-DSA 公钥查询对应的 ML-KEM 公钥
    pub fn get_mlkem_pubkey(&self, mldsa_pubkey_hex: &str) -> DhtResult<Option<String>> {
        #[cfg(feature = "mem_dht")]
        {
            Ok(self.mlkem_pubkeys.get(mldsa_pubkey_hex).map(|v| v.value().clone()))
        }
        #[cfg(not(feature = "mem_dht"))]
        {
            Ok(self.mlkem_pubkeys.lock().unwrap().get(mldsa_pubkey_hex).cloned())
        }
    }

    /// 删除 ML-DSA 公钥 → ML-KEM 公钥映射
    pub fn remove_mlkem_pubkey(&self, mldsa_pubkey_hex: &str) -> DhtResult<()> {
        #[cfg(feature = "mem_dht")]
        { self.mlkem_pubkeys.remove(mldsa_pubkey_hex); }
        #[cfg(not(feature = "mem_dht"))]
        { self.mlkem_pubkeys.lock().unwrap().remove(mldsa_pubkey_hex); }
        Ok(())
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
            let mut map = self.multiaddrs.lock().unwrap();
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
            let mut map = self.multiaddrs.lock().unwrap();
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