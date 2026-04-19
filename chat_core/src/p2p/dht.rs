use libp2p::kad::{ProviderRecord, Record, RecordKey, store::RecordStore};
use libp2p::{Multiaddr, PeerId};
use rand::prelude::*;
use rand::rng;
use redb::{Database, ReadableDatabase, ReadableTable, TableDefinition};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const RECORDS_TABLE: TableDefinition<&str, &[u8]> = TableDefinition::new("records");
const PROVIDERS_TABLE: TableDefinition<&str, &[u8]> = TableDefinition::new("providers");
const PEER_MULTIADDRS_TABLE: TableDefinition<&str, &[u8]> = TableDefinition::new("peer_multiaddrs");
const PUBKEY_PEERID_TABLE: TableDefinition<&str, &str> = TableDefinition::new("pubkey_peerid");

#[derive(Serialize, Deserialize)]
struct StoredRecord {
    value: Vec<u8>,
    publisher: Option<String>,
    expires: Option<u64>, // unix timestamp
}

#[derive(Serialize, Deserialize)]
struct StoredProvider {
    provider: String,
    expires: Option<u64>,
}

pub struct RedbRecordStore {
    db: Arc<Database>,
}

/// 资源限制配置
#[derive(Debug, Clone)]
pub struct ResourceLimits {
    /// 每个节点的最大记录数
    pub max_records_per_peer: usize,
    /// 总存储大小限制（字节）
    pub max_total_size: usize,
    /// 是否启用资源限制
    pub enabled: bool,
}

impl Default for ResourceLimits {
    fn default() -> Self {
        Self {
            max_records_per_peer: 1000,
            max_total_size: 100 * 1024 * 1024, // 100MB
            enabled: true,
        }
    }
}

/// 节点统计信息
#[derive(Debug, Clone)]
struct PeerStats {
    records_count: usize,
    total_size: usize,
    last_updated: SystemTime,
}

impl Default for PeerStats {
    fn default() -> Self {
        Self {
            records_count: 0,
            total_size: 0,
            last_updated: SystemTime::now(),
        }
    }
}

/// 带资源限制的记录存储
pub struct ResourceLimitedRecordStore {
    inner: RedbRecordStore,
    limits: ResourceLimits,
    peer_stats: std::sync::RwLock<std::collections::HashMap<PeerId, PeerStats>>,
    total_size: std::sync::atomic::AtomicUsize,
}

impl ResourceLimitedRecordStore {
    /// 创建带资源限制的存储
    pub fn new(db: Arc<Database>, limits: ResourceLimits) -> Self {
        Self {
            inner: RedbRecordStore::new(db),
            limits,
            peer_stats: std::sync::RwLock::new(std::collections::HashMap::new()),
            total_size: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    /// 检查节点是否超过资源限制
    pub fn check_peer_quota(&self, peer_id: &PeerId, record_size: usize) -> bool {
        if !self.limits.enabled {
            return true;
        }

        let stats = self.peer_stats.read().unwrap();
        if let Some(peer_stats) = stats.get(peer_id) {
            // 检查记录数量限制
            if peer_stats.records_count >= self.limits.max_records_per_peer {
                return false;
            }

            // 检查总大小限制
            let current_total = self.total_size.load(std::sync::atomic::Ordering::Relaxed);
            if current_total + record_size > self.limits.max_total_size {
                return false;
            }
        }

        true
    }

    /// 更新节点统计信息
    pub fn update_peer_stats(&self, peer_id: &PeerId, record_size: usize, increment: bool) {
        let mut stats = self.peer_stats.write().unwrap();
        let peer_stats = stats.entry(*peer_id).or_default();

        if increment {
            peer_stats.records_count += 1;
            peer_stats.total_size += record_size;
            self.total_size
                .fetch_add(record_size, std::sync::atomic::Ordering::Relaxed);
        } else {
            peer_stats.records_count = peer_stats.records_count.saturating_sub(1);
            peer_stats.total_size = peer_stats.total_size.saturating_sub(record_size);
            self.total_size
                .fetch_sub(record_size, std::sync::atomic::Ordering::Relaxed);
        }

        peer_stats.last_updated = SystemTime::now();
    }

    /// 清理过期记录的统计信息（需要在外部定期调用）
    pub fn cleanup_expired_stats(&self, active_peers: &std::collections::HashSet<PeerId>) {
        let mut stats = self.peer_stats.write().unwrap();
        stats.retain(|peer_id, _| active_peers.contains(peer_id));
    }

    /// 获取内部存储（用于需要直接访问的场景）
    pub fn inner(&self) -> &RedbRecordStore {
        &self.inner
    }

    /// 获取内部存储的可变引用
    pub fn inner_mut(&mut self) -> &mut RedbRecordStore {
        &mut self.inner
    }
}

// 为 ResourceLimitedRecordStore 实现 RecordStore trait
impl libp2p::kad::store::RecordStore for ResourceLimitedRecordStore {
    type RecordsIter<'a> = std::vec::IntoIter<std::borrow::Cow<'a, libp2p::kad::Record>>;
    type ProvidedIter<'a> = std::vec::IntoIter<std::borrow::Cow<'a, libp2p::kad::ProviderRecord>>;

    fn put(&mut self, record: libp2p::kad::Record) -> Result<(), libp2p::kad::store::Error> {
        // 检查资源限制
        if let Some(publisher) = &record.publisher
            && !self.check_peer_quota(publisher, record.value.len())
        {
            return Err(libp2p::kad::store::Error::MaxRecords);
        }

        // 调用内部存储的 put 方法
        let result = self.inner.put(record.clone());

        // 如果成功，更新统计信息
        if result.is_ok()
            && let Some(publisher) = &record.publisher
        {
            self.update_peer_stats(publisher, record.value.len(), true);
        }

        result
    }

    fn get(
        &self,
        key: &libp2p::kad::RecordKey,
    ) -> Option<std::borrow::Cow<'_, libp2p::kad::Record>> {
        self.inner.get(key)
    }

    fn remove(&mut self, key: &libp2p::kad::RecordKey) {
        // 先获取记录以更新统计信息
        if let Some(record) = self.inner.get(key)
            && let Some(publisher) = record.publisher
        {
            self.update_peer_stats(&publisher, record.value.len(), false);
        }

        self.inner.remove(key);
    }

    fn records(&self) -> Self::RecordsIter<'_> {
        self.inner.records()
    }

    fn add_provider(
        &mut self,
        record: libp2p::kad::ProviderRecord,
    ) -> Result<(), libp2p::kad::store::Error> {
        self.inner.add_provider(record)
    }

    fn providers(&self, key: &libp2p::kad::RecordKey) -> Vec<libp2p::kad::ProviderRecord> {
        self.inner.providers(key)
    }

    fn provided(&self) -> Self::ProvidedIter<'_> {
        self.inner.provided()
    }

    fn remove_provider(&mut self, key: &libp2p::kad::RecordKey, provider: &PeerId) {
        self.inner.remove_provider(key, provider)
    }
}

impl RedbRecordStore {
    pub fn new(db: Arc<Database>) -> Self {
        Self { db }
    }

    fn now_unix() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }

    fn is_expired(expires: Option<u64>) -> bool {
        expires.is_some_and(|exp| Self::now_unix() > exp)
    }

    // Multiaddr management
    pub fn add_multiaddr(&self, peer_id: &PeerId, multiaddr: &Multiaddr) -> anyhow::Result<()> {
        let peer_id_str = peer_id.to_string();
        let multiaddr_str = multiaddr.to_string();

        let write_txn = self.db.as_ref().begin_write()?;
        {
            let mut table = write_txn.open_table(PEER_MULTIADDRS_TABLE)?;
            let mut addrs = {
                let existing = table.get(peer_id_str.as_str())?;
                if let Some(data) = existing {
                    postcard::from_bytes(data.value())?
                } else {
                    Vec::new()
                }
            };
            if !addrs.contains(&multiaddr_str) {
                addrs.push(multiaddr_str);
            }
            let serialized = postcard::to_allocvec(&addrs)?;
            table.insert(peer_id_str.as_str(), serialized.as_slice())?;
        }
        write_txn.commit()?;
        Ok(())
    }

    pub fn remove_multiaddr(&self, peer_id: &PeerId, multiaddr: &Multiaddr) -> anyhow::Result<()> {
        let peer_id_str = peer_id.to_string();
        let multiaddr_str = multiaddr.to_string();

        let write_txn = self.db.as_ref().begin_write()?;
        {
            let mut table = write_txn.open_table(PEER_MULTIADDRS_TABLE)?;
            let addrs_bytes = {
                let existing = table.get(peer_id_str.as_str())?;
                existing.map(|data| data.value().to_vec())
            };
            if let Some(data_vec) = addrs_bytes {
                let mut addrs: Vec<String> = postcard::from_bytes(&data_vec)?;
                addrs.retain(|a| a != &multiaddr_str);
                if addrs.is_empty() {
                    table.remove(peer_id_str.as_str())?;
                } else {
                    let serialized = postcard::to_allocvec(&addrs)?;
                    table.insert(peer_id_str.as_str(), serialized.as_slice())?;
                }
            }
        }
        write_txn.commit()?;
        Ok(())
    }

    pub fn get_multiaddrs(&self, peer_id: &PeerId) -> anyhow::Result<Vec<Multiaddr>> {
        let peer_id_str = peer_id.to_string();
        let read_txn = self.db.as_ref().begin_read()?;
        let table = read_txn.open_table(PEER_MULTIADDRS_TABLE)?;
        if let Some(data) = table.get(peer_id_str.as_str())? {
            let addrs: Vec<String> = postcard::from_bytes(data.value())?;
            Ok(addrs.into_iter().filter_map(|s| s.parse().ok()).collect())
        } else {
            Ok(Vec::new())
        }
    }

    pub fn get_random_multiaddr(&self, peer_id: &PeerId) -> anyhow::Result<Option<Multiaddr>> {
        let mut addrs = self.get_multiaddrs(peer_id)?;
        if addrs.is_empty() {
            return Ok(None);
        }
        let mut rng = rng();
        addrs.shuffle(&mut rng);
        Ok(Some(addrs[0].clone()))
    }

    // Pubkey to PeerID mapping (temporary, stored in DHT)
    pub fn set_pubkey_peerid(&self, pubkey_hex: &str, peer_id: &PeerId) -> anyhow::Result<()> {
        let peer_id_str = peer_id.to_string();
        let write_txn = self.db.as_ref().begin_write()?;
        {
            let mut table = write_txn.open_table(PUBKEY_PEERID_TABLE)?;
            table.insert(pubkey_hex, peer_id_str.as_str())?;
        }
        write_txn.commit()?;
        Ok(())
    }

    pub fn get_peerid_by_pubkey(&self, pubkey_hex: &str) -> anyhow::Result<Option<PeerId>> {
        let read_txn = self.db.as_ref().begin_read()?;
        let table = read_txn.open_table(PUBKEY_PEERID_TABLE)?;
        if let Some(data) = table.get(pubkey_hex)? {
            let peer_id_str: &str = data.value();
            Ok(Some(peer_id_str.parse()?))
        } else {
            Ok(None)
        }
    }

    pub fn remove_pubkey_peerid(&self, pubkey_hex: &str) -> anyhow::Result<()> {
        let write_txn = self.db.as_ref().begin_write()?;
        {
            let mut table = write_txn.open_table(PUBKEY_PEERID_TABLE)?;
            table.remove(pubkey_hex)?;
        }
        write_txn.commit()?;
        Ok(())
    }
}

impl RecordStore for RedbRecordStore {
    type RecordsIter<'a> = std::vec::IntoIter<Cow<'a, Record>>;
    type ProvidedIter<'a> = std::vec::IntoIter<Cow<'a, ProviderRecord>>;

    fn put(&mut self, record: Record) -> Result<(), libp2p::kad::store::Error> {
        let key_str = std::str::from_utf8(record.key.as_ref())
            .map_err(|_| libp2p::kad::store::Error::MaxRecords)?;
        let stored = StoredRecord {
            value: record.value,
            publisher: record.publisher.map(|p| p.to_string()),
            expires: record
                .expires
                .map(|i| i.elapsed().as_secs() + Self::now_unix()),
        };
        let serialized =
            postcard::to_allocvec(&stored).map_err(|_| libp2p::kad::store::Error::MaxRecords)?;

        let write_txn = self
            .db
            .begin_write()
            .map_err(|_| libp2p::kad::store::Error::MaxRecords)?;
        {
            let mut table = write_txn
                .open_table(RECORDS_TABLE)
                .map_err(|_| libp2p::kad::store::Error::MaxRecords)?;
            table
                .insert(key_str, serialized.as_slice())
                .map_err(|_| libp2p::kad::store::Error::MaxRecords)?;
        }
        write_txn
            .commit()
            .map_err(|_| libp2p::kad::store::Error::MaxRecords)?;
        Ok(())
    }

    fn get(&self, key: &RecordKey) -> Option<Cow<'_, Record>> {
        let key_str = std::str::from_utf8(key.as_ref()).ok()?;
        let read_txn = self.db.as_ref().begin_read().ok()?;
        let table = read_txn.open_table(RECORDS_TABLE).ok()?;
        let data = table.get(key_str).ok()??;
        let stored: StoredRecord = postcard::from_bytes(data.value()).ok()?;
        if Self::is_expired(stored.expires) {
            return None;
        }
        let publisher = stored.publisher.and_then(|s| s.parse().ok());
        let expires = stored
            .expires
            .map(|exp| Instant::now() + Duration::from_secs(exp.saturating_sub(Self::now_unix())));
        Some(Cow::Owned(Record {
            key: key.clone(),
            value: stored.value,
            publisher,
            expires,
        }))
    }

    fn remove(&mut self, key: &RecordKey) {
        let key_str = std::str::from_utf8(key.as_ref()).unwrap_or("");
        let write_txn = self.db.as_ref().begin_write().ok();
        if let Some(txn) = write_txn {
            {
                let table = txn.open_table(RECORDS_TABLE).ok();
                if let Some(mut t) = table {
                    t.remove(key_str).ok();
                }
            }

            txn.commit().ok();
        }
    }

    fn records(&self) -> Self::RecordsIter<'_> {
        let mut records = Vec::new();
        let read_txn = self.db.as_ref().begin_read().ok();
        if let Some(txn) = read_txn
            && let Ok(table) = txn.open_table(RECORDS_TABLE)
        {
            let iter = table.iter().ok();
            if let Some(mut iter) = iter {
                while let Some(Ok((key, value))) = iter.next() {
                    let key_str = key.value();
                    let data = value.value();
                    if let Ok(stored) = postcard::from_bytes::<StoredRecord>(data)
                        && !Self::is_expired(stored.expires)
                    {
                        let publisher = stored.publisher.and_then(|s| s.parse().ok());
                        let expires = stored.expires.map(|exp| {
                            Instant::now()
                                + Duration::from_secs(exp.saturating_sub(Self::now_unix()))
                        });
                        // RecordKey is created from the UTF-8 string bytes
                        // This matches how we stored it in put() using std::str::from_utf8
                        records.push(Cow::Owned(Record {
                            key: RecordKey::from(key_str.as_bytes().to_vec()),
                            value: stored.value,
                            publisher,
                            expires,
                        }));
                    }
                }
            }
        }
        records.into_iter()
    }

    fn add_provider(&mut self, record: ProviderRecord) -> Result<(), libp2p::kad::store::Error> {
        let key_str = std::str::from_utf8(record.key.as_ref())
            .map_err(|_| libp2p::kad::store::Error::MaxRecords)?;

        let new_stored = StoredProvider {
            provider: record.provider.to_string(),
            expires: record
                .expires
                .map(|i| i.elapsed().as_secs() + Self::now_unix()),
        };

        let write_txn = self
            .db
            .as_ref()
            .begin_write()
            .map_err(|_| libp2p::kad::store::Error::MaxRecords)?;
        {
            let mut table = write_txn
                .open_table(PROVIDERS_TABLE)
                .map_err(|_| libp2p::kad::store::Error::MaxRecords)?;

            let mut providers: Vec<StoredProvider> = {
                let existing = table.get(key_str).ok().flatten();
                if let Some(data) = existing {
                    postcard::from_bytes(data.value()).unwrap_or_default()
                } else {
                    Vec::new()
                }
            };

            // Check if provider already exists, if so update expiration, otherwise push
            if let Some(existing_provider) = providers
                .iter_mut()
                .find(|p| p.provider == new_stored.provider)
            {
                existing_provider.expires = new_stored.expires;
            } else {
                providers.push(new_stored);
            }

            let serialized = postcard::to_allocvec(&providers)
                .map_err(|_| libp2p::kad::store::Error::MaxRecords)?;
            table
                .insert(key_str, serialized.as_slice())
                .map_err(|_| libp2p::kad::store::Error::MaxRecords)?;
        }
        write_txn
            .commit()
            .map_err(|_| libp2p::kad::store::Error::MaxRecords)?;
        Ok(())
    }

    fn providers(&self, key: &RecordKey) -> Vec<ProviderRecord> {
        let key_str = std::str::from_utf8(key.as_ref()).unwrap_or("");
        let read_txn = self.db.as_ref().begin_read().ok();
        if let Some(txn) = read_txn {
            let table = txn.open_table(PROVIDERS_TABLE).ok();
            if let Some(t) = table
                && let Ok(Some(data)) = t.get(key_str)
                && let Ok(providers) = postcard::from_bytes::<Vec<StoredProvider>>(data.value())
            {
                return providers
                    .into_iter()
                    .filter_map(|stored| {
                        if Self::is_expired(stored.expires) {
                            None
                        } else {
                            let provider = stored.provider.parse().ok()?;
                            let expires = stored.expires.map(|exp| {
                                Instant::now()
                                    + Duration::from_secs(exp.saturating_sub(Self::now_unix()))
                            });
                            // 从 PEER_MULTIADDRS_TABLE 获取该 provider 的地址
                            let addresses = self.get_multiaddrs(&provider).unwrap_or_default();

                            Some(ProviderRecord {
                                key: key.clone(),
                                provider,
                                expires,
                                addresses,
                            })
                        }
                    })
                    .collect();
            }
        }
        Vec::new()
    }

    fn provided(&self) -> Self::ProvidedIter<'_> {
        let mut provided_records = Vec::new();
        let read_txn = self.db.as_ref().begin_read().ok();
        if let Some(txn) = read_txn
            && let Ok(table) = txn.open_table(PROVIDERS_TABLE)
        {
            let iter = table.iter().ok();
            if let Some(mut iter) = iter {
                while let Some(Ok((key, value))) = iter.next() {
                    let key_str = key.value();
                    let data = value.value();
                    if let Ok(providers) = postcard::from_bytes::<Vec<StoredProvider>>(data) {
                        for stored in providers {
                            if !Self::is_expired(stored.expires)
                                && let Ok(provider) = stored.provider.parse()
                            {
                                let expires = stored.expires.map(|exp| {
                                    Instant::now()
                                        + Duration::from_secs(exp.saturating_sub(Self::now_unix()))
                                });
                                // 从 PEER_MULTIADDRS_TABLE 获取该 provider 的地址
                                let addresses = self.get_multiaddrs(&provider).unwrap_or_default();

                                provided_records.push(Cow::Owned(ProviderRecord {
                                    key: RecordKey::from(key_str.as_bytes().to_vec()),
                                    provider,
                                    expires,
                                    addresses,
                                }));
                            }
                        }
                    }
                }
            }
        }
        provided_records.into_iter()
    }

    fn remove_provider(&mut self, key: &RecordKey, provider: &PeerId) {
        let key_str = std::str::from_utf8(key.as_ref()).unwrap_or("");
        let provider_str = provider.to_string();
        let write_txn = self.db.as_ref().begin_write().ok();
        if let Some(txn) = write_txn {
            {
                let table = txn.open_table(PROVIDERS_TABLE).ok();
                if let Some(mut t) = table {
                    let providers_bytes = {
                        let existing = t.get(key_str).ok().flatten();
                        existing.map(|data| data.value().to_vec())
                    };
                    if let Some(data_vec) = providers_bytes
                        && let Ok(mut providers) =
                            postcard::from_bytes::<Vec<StoredProvider>>(&data_vec)
                    {
                        providers.retain(|p| p.provider != provider_str);
                        if providers.is_empty() {
                            t.remove(key_str).ok();
                        } else {
                            let serialized = postcard::to_allocvec(&providers).unwrap();
                            t.insert(key_str, serialized.as_slice()).ok();
                        }
                    }
                }
            }
            txn.commit().ok();
        }
    }
}
