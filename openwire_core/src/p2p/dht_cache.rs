use dashmap::DashMap;
use libp2p::PeerId;
use std::sync::Arc;

use crate::error::DhtResult;

const MAX_PUBKEY_PEERID: usize = 10_000;
const MAX_MLKEM_KEYS: usize = 5_000;
const MAX_MULTIADDR_PEERS: usize = 2_000;
const MAX_ADDRS_PER_PEER: usize = 20;

/// 基于 DashMap 的内存 DHT 缓存（替代旧的红数据库记录存储）
///
/// 存储 pubkey↔peerid 映射、mlkem 公钥缓存。
/// 所有数据在进程退出后自动释放，无需持久化。
/// 达到上限时跳过新插入，不淘汰已有条目。
pub struct DhtCache {
    pubkey_peerid: DashMap<String, String>,
    mlkem_pubkeys: DashMap<String, String>,
    multiaddrs: DashMap<String, Vec<String>>,
}

impl DhtCache {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            pubkey_peerid: DashMap::new(),
            mlkem_pubkeys: DashMap::new(),
            multiaddrs: DashMap::new(),
        })
    }

    pub fn set_pubkey_peerid(&self, pubkey_hex: &str, peer_id: &PeerId) -> DhtResult<()> {
        if self.pubkey_peerid.len() >= MAX_PUBKEY_PEERID && !self.pubkey_peerid.contains_key(pubkey_hex)
        {
            tracing::warn!(
                "DHT pubkey_peerid cache at capacity (max={}), skipping {}",
                MAX_PUBKEY_PEERID,
                pubkey_hex
            );
            return Ok(());
        }
        self.pubkey_peerid
            .insert(pubkey_hex.to_string(), peer_id.to_string());
        Ok(())
    }

    pub fn get_peerid_by_pubkey(&self, pubkey_hex: &str) -> DhtResult<Option<PeerId>> {
        let result = self.pubkey_peerid.get(pubkey_hex).and_then(|v| {
            v.value().parse::<PeerId>().ok()
        });
        Ok(result)
    }

    pub fn get_pubkey_by_peerid(&self, peer_id: &PeerId) -> DhtResult<Option<String>> {
        let peer_id_str = peer_id.to_string();
        let result = self
            .pubkey_peerid
            .iter()
            .find(|entry| entry.value() == &peer_id_str)
            .map(|entry| entry.key().clone());
        Ok(result)
    }

    pub fn remove_pubkey_peerid(&self, pubkey_hex: &str) -> DhtResult<()> {
        self.pubkey_peerid.remove(pubkey_hex);
        Ok(())
    }

    pub fn get_all_pubkeys(&self) -> DhtResult<Vec<String>> {
        let keys: Vec<String> = self
            .pubkey_peerid
            .iter()
            .map(|entry| entry.key().clone())
            .collect();
        Ok(keys)
    }

    pub fn set_mlkem_pubkey(
        &self,
        mldsa_pubkey_hex: &str,
        mlkem_pubkey_hex: &str,
    ) -> DhtResult<()> {
        if self.mlkem_pubkeys.len() >= MAX_MLKEM_KEYS && !self.mlkem_pubkeys.contains_key(mldsa_pubkey_hex)
        {
            tracing::warn!(
                "DHT mlkem_pubkeys cache at capacity (max={}), skipping {}",
                MAX_MLKEM_KEYS,
                mldsa_pubkey_hex
            );
            return Ok(());
        }
        self.mlkem_pubkeys
            .insert(mldsa_pubkey_hex.to_string(), mlkem_pubkey_hex.to_string());
        Ok(())
    }

    pub fn get_mlkem_pubkey(&self, mldsa_pubkey_hex: &str) -> DhtResult<Option<String>> {
        let result = self
            .mlkem_pubkeys
            .get(mldsa_pubkey_hex)
            .map(|v| v.value().clone());
        Ok(result)
    }

    pub fn remove_mlkem_pubkey(&self, mldsa_pubkey_hex: &str) -> DhtResult<()> {
        self.mlkem_pubkeys.remove(mldsa_pubkey_hex);
        Ok(())
    }

    pub fn add_multiaddr(&self, peer_id: &PeerId, multiaddr: &libp2p::Multiaddr) -> DhtResult<()> {
        let key = peer_id.to_string();
        let addr_str = multiaddr.to_string();

        // 新增 peer 时检查总数上限
        if !self.multiaddrs.contains_key(&key)
            && self.multiaddrs.len() >= MAX_MULTIADDR_PEERS
        {
            tracing::warn!(
                "DHT multiaddrs cache at capacity (max={}), skipping peer {}",
                MAX_MULTIADDR_PEERS,
                key
            );
            return Ok(());
        }

        let mut addrs = self.multiaddrs.entry(key).or_default();
        if !addrs.contains(&addr_str) && addrs.len() < MAX_ADDRS_PER_PEER {
            addrs.push(addr_str);
        }
        Ok(())
    }

    pub fn remove_multiaddr(
        &self,
        peer_id: &PeerId,
        multiaddr: &libp2p::Multiaddr,
    ) -> DhtResult<()> {
        let key = peer_id.to_string();
        let addr_str = multiaddr.to_string();
        if let Some(mut entry) = self.multiaddrs.get_mut(&key) {
            entry.retain(|a| a != &addr_str);
            if entry.is_empty() {
                drop(entry);
                self.multiaddrs.remove(&key);
            }
        }
        Ok(())
    }

    pub fn get_multiaddrs(&self, peer_id: &PeerId) -> DhtResult<Vec<libp2p::Multiaddr>> {
        let key = peer_id.to_string();
        let result = self.multiaddrs.get(&key).map_or(Vec::new(), |addrs| {
            addrs
                .value()
                .iter()
                .filter_map(|s| s.parse().ok())
                .collect()
        });
        Ok(result)
    }
}