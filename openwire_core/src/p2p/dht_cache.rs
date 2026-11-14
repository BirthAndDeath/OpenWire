use dashmap::DashMap;
use libp2p::PeerId;
use std::sync::Arc;

const MAX_PUBKEY_PEERID: usize = 10_000;
const MAX_MULTIADDR_PEERS: usize = 2_000;
const MAX_ADDRS_PER_PEER: usize = 20;

/// 线程安全的 DHT 缓存，存储 pubkey↔peerid、Multiaddr 映射。
///
/// `pubkey_peerid` 存解析后的 `PeerId` 对象而非字符串：`get_peerid_by_pubkey`
/// 是每条发送/接收消息的热路径，避免每次查询重新 base58 解析。
pub struct DhtCache {
    pubkey_peerid: DashMap<String, PeerId>,
    peerid_pubkey: DashMap<String, String>,
    multiaddrs: DashMap<String, Vec<String>>,
    /// 账号（ML-DSA 公钥）→ 该账号的在线设备 PeerID 列表。
    ///
    /// 多设备语义单独维护，与 `pubkey_peerid`（主/最近设备，热路径单值查询）分开：
    /// 由 FriendOnline 签名验证后追加设备，供「账号级多设备同步/逐设备加密投递」使用。
    /// 设备下线（FriendOffline / 连接关闭）时移除。上限防止被恶意对端用同一账号刷爆。
    pubkey_devices: DashMap<String, Vec<PeerId>>,
    /// SHA256(ML-DSA 公钥)（即 dht_key 摘要）→ 该账号当前 PeerID。
    ///
    /// 供扫码暂存（仅持摘要、无完整公钥）经中继反查对端当前 PeerID，解决
    /// 二维码 peer_id 轮换/直连地址不可达时"等待对方上线"长期卡住的问题。
    /// 与 `pubkey_peerid` 同步写入（来源同为签名验证过的 FriendOnline）。
    digest_peerid: DashMap<String, PeerId>,
}

impl DhtCache {
    /// 创建新的 DHT 缓存实例
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            pubkey_peerid: DashMap::new(),
            peerid_pubkey: DashMap::new(),
            multiaddrs: DashMap::new(),
            pubkey_devices: DashMap::new(),
            digest_peerid: DashMap::new(),
        })
    }

    /// 设置 ML-DSA 公钥 → PeerID 映射（双向索引）
    pub fn set_pubkey_peerid(&self, pubkey_hex: &str, peer_id: &PeerId) {
        let peer_id_str = peer_id.to_string();
        // 注：DashMap 不保序，iter().next() 是任意键而非最旧条目，
        // 此处仅作容量兜底，淘汰任意键即可。
        if self.pubkey_peerid.len() >= MAX_PUBKEY_PEERID && !self.pubkey_peerid.contains_key(pubkey_hex)
            && let Some(entry) = self.pubkey_peerid.iter().next() {
                let old_key = entry.key().clone();
                let old_val = entry.value().to_string();
                drop(entry);
                self.pubkey_peerid.remove(&old_key);
                self.peerid_pubkey.remove(&old_val);
                tracing::debug!("DHT pubkey_peerid cache at capacity, evicted an entry (arbitrary key)");
            }
        if let Some(old) = self.pubkey_peerid.get(pubkey_hex) {
            let old_str = old.value().to_string();
            drop(old);
            self.peerid_pubkey.remove(&old_str);
        }
        self.pubkey_peerid.insert(pubkey_hex.to_string(), *peer_id);
        self.peerid_pubkey.insert(peer_id_str, pubkey_hex.to_string());
    }

    /// 通过 ML-DSA 公钥查询 PeerID（O(1)，无字符串解析）
    pub fn get_peerid_by_pubkey(&self, pubkey_hex: &str) -> Option<PeerId> {
        self.pubkey_peerid.get(pubkey_hex).map(|v| *v.value())
    }

    /// 通过 PeerID 反查 ML-DSA 公钥
    pub fn get_pubkey_by_peerid(&self, peer_id: &PeerId) -> Option<String> {
        self.peerid_pubkey.get(&peer_id.to_string()).map(|v| v.clone())
    }

    /// 设置 SHA256(公钥) 摘要 → PeerID 映射（与 set_pubkey_peerid 同步写入）
    pub fn set_digest_peerid(&self, digest_hex: &str, peer_id: &PeerId) {
        if self.digest_peerid.len() >= MAX_PUBKEY_PEERID && !self.digest_peerid.contains_key(digest_hex)
            && let Some(entry) = self.digest_peerid.iter().next()
        {
            let old_key = entry.key().clone();
            drop(entry);
            self.digest_peerid.remove(&old_key);
            tracing::debug!("DHT digest_peerid cache at capacity, evicted an entry (arbitrary key)");
        }
        self.digest_peerid.insert(digest_hex.to_string(), *peer_id);
    }

    /// 通过 SHA256(公钥) 摘要查询 PeerID（扫码暂存项仅持摘要时反查对端当前 PeerID）
    pub fn get_peerid_by_digest(&self, digest_hex: &str) -> Option<PeerId> {
        self.digest_peerid.get(digest_hex).map(|v| *v.value())
    }

    /// 删除 ML-DSA 公钥 → PeerID 映射（同时清理反向索引）
    pub fn remove_pubkey_peerid(&self, pubkey_hex: &str) {
        if let Some((_, peer_id)) = self.pubkey_peerid.remove(pubkey_hex) {
            self.peerid_pubkey.remove(&peer_id.to_string());
        }
    }

    /// 获取所有缓存的 ML-DSA 公钥列表
    pub fn get_all_pubkeys(&self) -> Vec<String> {
        self.pubkey_peerid.iter().map(|entry| entry.key().clone()).collect()
    }

    /// 将 PeerID 登记为该账号（公钥）的一台在线设备（多设备集合）。
    ///
    /// 幂等追加（已存在则忽略）。**仅由 FriendOnline 签名验证通过后调用**，
    /// 防止恶意对端以任意 PeerID 冒充实名设备污染设备列表。
    pub fn add_pubkey_device(&self, pubkey_hex: &str, peer_id: &PeerId) {
        const MAX_DEVICES_PER_ACCOUNT: usize = 16;
        const MAX_ACCOUNTS: usize = 4096;
        // 账号集合需整体封顶：任意对端可自签 FriendOnline 引入新公钥，
        // 若只按账号内设备数封顶，账号个数仍可无界增长（内存耗尽）
        if !self.pubkey_devices.contains_key(pubkey_hex)
            && self.pubkey_devices.len() >= MAX_ACCOUNTS
        {
            let short: String = pubkey_hex.chars().take(16).collect();
            tracing::warn!("pubkey_devices at capacity (max={MAX_ACCOUNTS}), skipping {short}..");
            return;
        }
        let mut devices = self.pubkey_devices.entry(pubkey_hex.to_string()).or_default();
        if !devices.contains(peer_id) && devices.len() < MAX_DEVICES_PER_ACCOUNT {
            devices.push(*peer_id);
        }
    }

    /// 将 PeerID 从该账号的在线设备集合中移除（设备下线/连接关闭时调用）。
    /// 集合空时删除键。
    pub fn remove_pubkey_device(&self, pubkey_hex: &str, peer_id: &PeerId) {
        if let Some(mut devices) = self.pubkey_devices.get_mut(pubkey_hex) {
            devices.retain(|p| p != peer_id);
            if devices.is_empty() {
                drop(devices);
                self.pubkey_devices.remove(pubkey_hex);
            }
        }
    }

    /// 获取某账号当前全部在线设备 PeerID（多设备投递/同步用）。
    /// 空 = 该账号无设备在线。
    pub fn list_pubkey_devices(&self, pubkey_hex: &str) -> Vec<PeerId> {
        self.pubkey_devices
            .get(pubkey_hex)
            .map(|devices| devices.value().clone())
            .unwrap_or_default()
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