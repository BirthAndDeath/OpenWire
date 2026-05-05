use libp2p::kad::{ProviderRecord, Record, RecordKey, store::RecordStore};
use libp2p::{Multiaddr, PeerId};
use rand::prelude::*;
use rand::rng;
use redb::{Database, ReadableDatabase, ReadableTable, TableDefinition};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::error::{DhtError, DhtResult};
use crate::p2p::validator::RecordValidator;
use crate::signature::DhtRecordSignature;

const RECORDS_TABLE: TableDefinition<&str, &[u8]> = TableDefinition::new("records");
const PROVIDERS_TABLE: TableDefinition<&str, &[u8]> = TableDefinition::new("providers");
const PEER_MULTIADDRS_TABLE: TableDefinition<&str, &[u8]> = TableDefinition::new("peer_multiaddrs");
const PUBKEY_PEERID_TABLE: TableDefinition<&str, &str> = TableDefinition::new("pubkey_peerid");
const PUBKEY_MLKEM_TABLE: TableDefinition<&str, &str> = TableDefinition::new("pubkey_mlkem");

/// 默认每个节点的最大记录数
pub const DEFAULT_MAX_RECORDS_PER_PEER: usize = 1000;
/// 默认总存储大小限制（100MB）
pub const DEFAULT_MAX_TOTAL_SIZE: usize = 100 * 1024 * 1024;

#[derive(Serialize, Deserialize)]
struct StoredRecord {
    value: Vec<u8>,
    publisher: Option<String>,
    expires: Option<u64>,       // unix timestamp
    signature: Option<Vec<u8>>, // ML-DSA/Ed25519 签名
    timestamp: Option<u64>,     // 记录创建时间戳（毫秒）
    salt: Option<[u8; 32]>,     // 防重放盐值
}

#[derive(Serialize, Deserialize)]
struct StoredProvider {
    provider: String,
    expires: Option<u64>,
    signature: Option<Vec<u8>>, // ML-DSA/Ed25519 签名
    timestamp: Option<u64>,     // 提供者注册时间戳（毫秒）
    salt: Option<[u8; 32]>,     // 防重放盐值
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
            max_records_per_peer: DEFAULT_MAX_RECORDS_PER_PEER,
            max_total_size: DEFAULT_MAX_TOTAL_SIZE,
            enabled: true,
        }
    }
}

/// 节点存储统计信息（用于 ResourceLimitedRecordStore 的资源限制跟踪）
/// 注意：此结构与 validator.rs 中的 PeerStats 不同，后者用于签名验证统计
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

/// 带资源限制和签名验证的记录存储
pub struct ResourceLimitedRecordStore {
    inner: RedbRecordStore,
    limits: ResourceLimits,
    peer_stats: std::sync::RwLock<std::collections::HashMap<PeerId, PeerStats>>,
    total_size: std::sync::atomic::AtomicUsize,
    /// 可选的签名验证器，用于在 put() 时验证记录签名
    validator: Option<std::sync::Arc<std::sync::RwLock<RecordValidator>>>,
}

impl ResourceLimitedRecordStore {
    /// 创建带资源限制的存储
    pub fn new(db: Arc<Database>, limits: ResourceLimits) -> Self {
        Self {
            inner: RedbRecordStore::new(db),
            limits,
            peer_stats: std::sync::RwLock::new(std::collections::HashMap::new()),
            total_size: std::sync::atomic::AtomicUsize::new(0),
            validator: None,
        }
    }

    /// 设置签名验证器（用于在 put() 时验证记录签名）
    pub fn set_validator(&mut self, validator: std::sync::Arc<std::sync::RwLock<RecordValidator>>) {
        self.validator = Some(validator);
    }

    /// 获取签名验证器引用
    pub fn get_validator(&self) -> Option<std::sync::Arc<std::sync::RwLock<RecordValidator>>> {
        self.validator.clone()
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

        // 注意：libp2p 的 Record 类型不包含自定义签名元数据（signature/timestamp/salt），
        // 因此无法在此处验证传入记录的签名。签名验证在应用层完成：
        //
        // 1. 当本地节点发布记录时，使用 put_signed_record() 方法存储带签名的记录
        // 2. 当从 DHT 查询到记录时，在 events.rs::handle_kademlia_event() 中
        //    从存储中提取签名元数据并调用 validator.validate_dht_record() 验证
        //
        // 此处的 validator 字段保留供未来扩展使用（例如在 put() 中验证
        // 记录发布者的身份），当前不执行签名验证。

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
        // 预先创建所有需要的表，避免后续 open_table() 时因表不存在而报错
        if let Ok(write_txn) = db.begin_write() {
            let _ = write_txn.open_table(RECORDS_TABLE);
            let _ = write_txn.open_table(PROVIDERS_TABLE);
            let _ = write_txn.open_table(PEER_MULTIADDRS_TABLE);
            let _ = write_txn.open_table(PUBKEY_PEERID_TABLE);
            let _ = write_txn.open_table(PUBKEY_MLKEM_TABLE);
            let _ = write_txn.commit();
        }
        Self { db }
    }

    fn now_unix() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or(Duration::ZERO)
            .as_secs()
    }

    fn is_expired(expires: Option<u64>) -> bool {
        expires.is_some_and(|exp| Self::now_unix() > exp)
    }

    /// 在写事务中执行操作，自动处理 begin_write/commit
    fn with_write_txn<T>(
        &self,
        f: impl FnOnce(&redb::WriteTransaction) -> DhtResult<T>,
    ) -> DhtResult<T> {
        let write_txn = self.db.as_ref().begin_write()?;
        let result = f(&write_txn)?;
        write_txn.commit()?;
        Ok(result)
    }

    /// 在读事务中执行操作，自动处理 begin_read
    fn with_read_txn<T>(
        &self,
        f: impl FnOnce(&redb::ReadTransaction) -> DhtResult<T>,
    ) -> DhtResult<T> {
        let read_txn = self
            .db
            .as_ref()
            .begin_read()
            .map_err(DhtError::ReadTransactionFailed)?;
        f(&read_txn)
    }

    /// 在写事务中执行操作（针对 libp2p kad store Error 类型）
    fn with_write_txn_kad<T>(
        &self,
        f: impl FnOnce(&redb::WriteTransaction) -> Result<T, libp2p::kad::store::Error>,
    ) -> Result<T, libp2p::kad::store::Error> {
        let write_txn = self
            .db
            .begin_write()
            .map_err(|_| libp2p::kad::store::Error::MaxRecords)?;
        let result = f(&write_txn)?;
        write_txn
            .commit()
            .map_err(|_| libp2p::kad::store::Error::MaxRecords)?;
        Ok(result)
    }

    /// 存储带签名的 DHT 记录
    /// 签名元数据（signature/timestamp/salt）由 DhtRecordSignature 提供
    pub fn put_signed_record(
        &self,
        record: Record,
        sig: DhtRecordSignature,
    ) -> Result<(), libp2p::kad::store::Error> {
        let key_str = std::str::from_utf8(record.key.as_ref())
            .map_err(|_| libp2p::kad::store::Error::MaxRecords)?;

        let stored = StoredRecord {
            value: record.value,
            publisher: record.publisher.map(|p| p.to_string()),
            expires: record
                .expires
                .map(|i| i.elapsed().as_secs() + Self::now_unix()),
            signature: Some(sig.signature),
            timestamp: Some(sig.timestamp),
            salt: Some(sig.salt),
        };
        let serialized =
            postcard::to_allocvec(&stored).map_err(|_| libp2p::kad::store::Error::MaxRecords)?;

        self.with_write_txn_kad(|write_txn| {
            let mut table = write_txn
                .open_table(RECORDS_TABLE)
                .map_err(|_| libp2p::kad::store::Error::MaxRecords)?;
            table
                .insert(key_str, serialized.as_slice())
                .map_err(|_| libp2p::kad::store::Error::MaxRecords)?;
            Ok(())
        })
    }

    /// 存储带签名的 Provider 记录
    pub fn add_signed_provider(
        &self,
        record: ProviderRecord,
        sig: DhtRecordSignature,
    ) -> Result<(), libp2p::kad::store::Error> {
        let key_str = std::str::from_utf8(record.key.as_ref())
            .map_err(|_| libp2p::kad::store::Error::MaxRecords)?;

        let new_stored = StoredProvider {
            provider: record.provider.to_string(),
            expires: record
                .expires
                .map(|i| i.elapsed().as_secs() + Self::now_unix()),
            signature: Some(sig.signature),
            timestamp: Some(sig.timestamp),
            salt: Some(sig.salt),
        };

        self.with_write_txn_kad(|write_txn| {
            let mut table = write_txn
                .open_table(PROVIDERS_TABLE)
                .map_err(|_| libp2p::kad::store::Error::MaxRecords)?;

            let mut providers: Vec<StoredProvider> = {
                let existing = table.get(key_str).ok().flatten();
                if let Some(data) = existing {
                    match postcard::from_bytes(data.value()) {
                        Ok(parsed) => parsed,
                        Err(e) => {
                            tracing::warn!(
                                "DHT 提供者记录解析失败 (key={:?}), 将重置为空: {}",
                                key_str,
                                e
                            );
                            Vec::new()
                        }
                    }
                } else {
                    Vec::new()
                }
            };

            if let Some(existing_provider) = providers
                .iter_mut()
                .find(|p| p.provider == new_stored.provider)
            {
                existing_provider.expires = new_stored.expires;
                existing_provider.signature = new_stored.signature;
                existing_provider.timestamp = new_stored.timestamp;
                existing_provider.salt = new_stored.salt;
            } else {
                providers.push(new_stored);
            }

            let serialized = postcard::to_allocvec(&providers)
                .map_err(|_| libp2p::kad::store::Error::MaxRecords)?;
            table
                .insert(key_str, serialized.as_slice())
                .map_err(|_| libp2p::kad::store::Error::MaxRecords)?;
            Ok(())
        })
    }

    /// 获取记录的签名元数据（如果存在）
    pub fn get_record_signature(&self, key: &RecordKey) -> Option<DhtRecordSignature> {
        let key_str = std::str::from_utf8(key.as_ref()).ok()?;
        let read_txn = self.db.as_ref().begin_read().ok()?;
        let table = read_txn.open_table(RECORDS_TABLE).ok()?;
        let data = table.get(key_str).ok()??;
        let stored: StoredRecord = postcard::from_bytes(data.value()).ok()?;
        let signature = stored.signature?;
        let timestamp = stored.timestamp?;
        let salt = stored.salt?;
        Some(DhtRecordSignature {
            timestamp,
            salt,
            signature,
        })
    }

    /// 获取 Provider 记录的签名元数据（如果存在）
    pub fn get_provider_signature(
        &self,
        key: &RecordKey,
        provider: &PeerId,
    ) -> Option<DhtRecordSignature> {
        let key_str = std::str::from_utf8(key.as_ref()).ok()?;
        let provider_str = provider.to_string();
        let read_txn = self.db.as_ref().begin_read().ok()?;
        let table = read_txn.open_table(PROVIDERS_TABLE).ok()?;
        let data = table.get(key_str).ok()??;
        let providers: Vec<StoredProvider> = postcard::from_bytes(data.value()).ok()?;
        providers.into_iter().find_map(|p| {
            if p.provider == provider_str {
                let signature = p.signature?;
                let timestamp = p.timestamp?;
                let salt = p.salt?;
                Some(DhtRecordSignature {
                    timestamp,
                    salt,
                    signature,
                })
            } else {
                None
            }
        })
    }

    // Multiaddr management
    pub fn add_multiaddr(&self, peer_id: &PeerId, multiaddr: &Multiaddr) -> DhtResult<()> {
        let peer_id_str = peer_id.to_string();
        let multiaddr_str = multiaddr.to_string();

        self.with_write_txn(|write_txn| {
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
            Ok(())
        })
    }

    pub fn remove_multiaddr(&self, peer_id: &PeerId, multiaddr: &Multiaddr) -> DhtResult<()> {
        let peer_id_str = peer_id.to_string();
        let multiaddr_str = multiaddr.to_string();

        self.with_write_txn(|write_txn| {
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
            Ok(())
        })
    }

    pub fn get_multiaddrs(&self, peer_id: &PeerId) -> DhtResult<Vec<Multiaddr>> {
        let peer_id_str = peer_id.to_string();
        self.with_read_txn(|read_txn| {
            let table = read_txn.open_table(PEER_MULTIADDRS_TABLE)?;
            if let Some(data) = table.get(peer_id_str.as_str())? {
                let addrs: Vec<String> = postcard::from_bytes(data.value())?;
                Ok(addrs.into_iter().filter_map(|s| s.parse().ok()).collect())
            } else {
                Ok(Vec::new())
            }
        })
    }

    pub fn get_random_multiaddr(&self, peer_id: &PeerId) -> DhtResult<Option<Multiaddr>> {
        let mut addrs = self.get_multiaddrs(peer_id)?;
        if addrs.is_empty() {
            return Ok(None);
        }
        let mut rng = rng();
        addrs.shuffle(&mut rng);
        Ok(Some(addrs[0].clone()))
    }

    // Pubkey to PeerID mapping (temporary, stored in DHT)
    pub fn set_pubkey_peerid(&self, pubkey_hex: &str, peer_id: &PeerId) -> DhtResult<()> {
        let peer_id_str = peer_id.to_string();
        self.with_write_txn(|write_txn| {
            let mut table = write_txn.open_table(PUBKEY_PEERID_TABLE)?;
            table.insert(pubkey_hex, peer_id_str.as_str())?;
            Ok(())
        })
    }

    pub fn get_peerid_by_pubkey(&self, pubkey_hex: &str) -> DhtResult<Option<PeerId>> {
        self.with_read_txn(|read_txn| {
            let table = read_txn.open_table(PUBKEY_PEERID_TABLE)?;
            if let Some(data) = table.get(pubkey_hex)? {
                let peer_id_str: &str = data.value();
                let peer_id = peer_id_str
                    .parse::<libp2p::PeerId>()
                    .map_err(|e| DhtError::PeerIdParseError(e.into()))?;
                Ok(Some(peer_id))
            } else {
                Ok(None)
            }
        })
    }

    /// 通过 PeerID 反向查找对应的 ML-DSA 公钥 hex
    ///
    /// 遍历 PUBKEY_PEERID_TABLE，查找值等于给定 PeerID 的条目。
    /// 由于是反向查找（表结构是 pubkey->peerid），需要遍历所有条目。
    /// 适用于小规模数据（联系人数量通常有限）。
    pub fn get_pubkey_by_peerid(&self, peer_id: &PeerId) -> DhtResult<Option<String>> {
        let peer_id_str = peer_id.to_string();
        self.with_read_txn(|read_txn| {
            let table = read_txn.open_table(PUBKEY_PEERID_TABLE)?;
            for result in table.iter()? {
                let (key, value) = result?;
                if value.value() == peer_id_str {
                    return Ok(Some(key.value().to_string()));
                }
            }
            Ok(None)
        })
    }

    pub fn remove_pubkey_peerid(&self, pubkey_hex: &str) -> DhtResult<()> {
        self.with_write_txn(|write_txn| {
            let mut table = write_txn.open_table(PUBKEY_PEERID_TABLE)?;
            table.remove(pubkey_hex)?;
            Ok(())
        })
    }

    /// 获取所有已注册的 ML-DSA 公钥列表
    ///
    /// 遍历 PUBKEY_PEERID_TABLE 返回所有键（ML-DSA 公钥 hex），
    /// 用于 DHT 注册循环在身份切换后重新读取所有需要注册的身份。
    pub fn get_all_pubkeys(&self) -> DhtResult<Vec<String>> {
        self.with_read_txn(|read_txn| {
            let table = read_txn.open_table(PUBKEY_PEERID_TABLE)?;
            let mut keys = Vec::new();
            for result in table.iter()? {
                let (key, _) = result?;
                keys.push(key.value().to_string());
            }
            Ok(keys)
        })
    }

    /// 存储联系人的 ML-KEM 公钥（临时会话密钥）
    /// key: mldsa_pubkey_hex, value: mlkem_pubkey_hex
    pub fn set_mlkem_pubkey(
        &self,
        mldsa_pubkey_hex: &str,
        mlkem_pubkey_hex: &str,
    ) -> DhtResult<()> {
        self.with_write_txn(|write_txn| {
            let mut table = write_txn.open_table(PUBKEY_MLKEM_TABLE)?;
            table.insert(mldsa_pubkey_hex, mlkem_pubkey_hex)?;
            Ok(())
        })
    }

    /// 查询联系人的 ML-KEM 公钥
    pub fn get_mlkem_pubkey(&self, mldsa_pubkey_hex: &str) -> DhtResult<Option<String>> {
        self.with_read_txn(|read_txn| {
            let table = read_txn.open_table(PUBKEY_MLKEM_TABLE)?;
            if let Some(data) = table.get(mldsa_pubkey_hex)? {
                Ok(Some(data.value().to_string()))
            } else {
                Ok(None)
            }
        })
    }

    /// 删除联系人的 ML-KEM 公钥
    pub fn remove_mlkem_pubkey(&self, mldsa_pubkey_hex: &str) -> DhtResult<()> {
        self.with_write_txn(|write_txn| {
            let mut table = write_txn.open_table(PUBKEY_MLKEM_TABLE)?;
            table.remove(mldsa_pubkey_hex)?;
            Ok(())
        })
    }

    /// 删除过期的 pubkey->PeerID 缓存条目
    ///
    /// 保留自身身份的映射（由 `own_pubkey_hex` 指定），删除所有其他联系人的缓存。
    /// 因为 PeerID 是临时传输层标识（每次启动重新生成），
    /// 本地缓存的他人 PeerID 可能已过期，需要定期清理以触发网络重新查询获取最新值。
    ///
    /// # 参数
    /// - `own_pubkey_hex`: 自身 ML-DSA 公钥 hex，此条目不删除
    pub fn clear_expired_pubkey_peerid_cache(&self, own_pubkey_hex: &str) -> DhtResult<()> {
        self.with_write_txn(|write_txn| {
            let mut table = write_txn.open_table(PUBKEY_PEERID_TABLE)?;
            // 收集需要删除的键（redb 4.0 不支持在迭代时删除，先收集再删除）
            let stale_keys: Vec<String> = table
                .iter()?
                .filter_map(|r| r.ok())
                .map(|(k, _)| k.value().to_string())
                .filter(|k| k != own_pubkey_hex)
                .collect();
            let count = stale_keys.len();
            for key in stale_keys {
                table.remove(key.as_str())?;
            }
            if count > 0 {
                tracing::debug!("Cleared {} stale pubkey->PeerID cache entries", count);
            }
            Ok(())
        })
    }

    /// 删除过期的 ML-KEM 公钥缓存条目
    ///
    /// 保留自身身份的映射（由 `own_pubkey_hex` 指定），删除所有其他联系人的缓存。
    /// ML-KEM 公钥是临时会话密钥，每次启动都会变化，
    /// 本地缓存的他人 ML-KEM 公钥已不可用，需要定期清理。
    ///
    /// # 参数
    /// - `own_pubkey_hex`: 自身 ML-DSA 公钥 hex，此条目不删除
    pub fn clear_expired_mlkem_pubkey_cache(&self, own_pubkey_hex: &str) -> DhtResult<()> {
        self.with_write_txn(|write_txn| {
            let mut table = write_txn.open_table(PUBKEY_MLKEM_TABLE)?;
            let stale_keys: Vec<String> = table
                .iter()?
                .filter_map(|r| r.ok())
                .map(|(k, _)| k.value().to_string())
                .filter(|k| k != own_pubkey_hex)
                .collect();
            let count = stale_keys.len();
            for key in stale_keys {
                table.remove(key.as_str())?;
            }
            if count > 0 {
                tracing::debug!("Cleared {} stale ML-KEM pubkey cache entries", count);
            }
            Ok(())
        })
    }

    /// 验证身份绑定：检查给定的 ML-DSA 公钥 hex 是否与 PeerID 和 ML-KEM 公钥一致
    pub fn verify_identity_binding(
        &self,
        mldsa_pubkey_hex: &str,
        expected_peer_id: Option<&PeerId>,
        expected_mlkem_pubkey_hex: Option<&str>,
    ) -> DhtResult<bool> {
        if let Some(expected_pid) = expected_peer_id {
            match self.get_peerid_by_pubkey(mldsa_pubkey_hex)? {
                Some(stored_pid) => {
                    if &stored_pid != expected_pid {
                        tracing::warn!(
                            "Identity binding mismatch: ML-DSA {} -> PeerID {} (expected {})",
                            &mldsa_pubkey_hex[..16],
                            stored_pid,
                            expected_pid
                        );
                        return Ok(false);
                    }
                }
                None => {
                    tracing::warn!(
                        "Identity binding not found: ML-DSA {} has no PeerID mapping",
                        &mldsa_pubkey_hex[..16]
                    );
                    return Ok(false);
                }
            }
        }

        if let Some(expected_mlkem) = expected_mlkem_pubkey_hex {
            match self.get_mlkem_pubkey(mldsa_pubkey_hex)? {
                Some(stored_mlkem) => {
                    if stored_mlkem != expected_mlkem {
                        tracing::warn!(
                            "ML-KEM binding mismatch for ML-DSA {}: stored {} != expected {}",
                            &mldsa_pubkey_hex[..16],
                            &stored_mlkem[..16],
                            &expected_mlkem[..16]
                        );
                        return Ok(false);
                    }
                }
                None => {
                    tracing::warn!(
                        "ML-KEM binding not found for ML-DSA {}",
                        &mldsa_pubkey_hex[..16]
                    );
                    return Ok(false);
                }
            }
        }

        tracing::info!(
            "Identity binding verified for ML-DSA {}",
            &mldsa_pubkey_hex[..16]
        );
        Ok(true)
    }

    /// 批量清理过期记录
    ///
    /// 此方法会清理所有过期的DHT记录和提供者记录。
    /// 建议定期调用此方法以维护数据库性能。
    ///
    /// # 返回
    /// - `Ok((records_cleaned, providers_cleaned))`: 清理的记录数量
    pub fn cleanup_expired_records(&self) -> DhtResult<(usize, usize)> {
        let mut records_cleaned = 0;
        let mut providers_cleaned = 0;

        self.with_write_txn(|write_txn| {
            // 清理过期记录
            if let Ok(mut records_table) = write_txn.open_table(RECORDS_TABLE) {
                let expired_keys: Vec<String> = records_table
                    .iter()?
                    .filter_map(|r| r.ok())
                    .filter_map(|(key, value)| {
                        let data = value.value();
                        if let Ok(stored) = postcard::from_bytes::<StoredRecord>(data)
                            && Self::is_expired(stored.expires)
                        {
                            Some(key.value().to_string())
                        } else {
                            None
                        }
                    })
                    .collect();

                for key in expired_keys {
                    records_table.remove(key.as_str())?;
                    records_cleaned += 1;
                }
            }

            // 清理过期提供者记录
            if let Ok(mut providers_table) = write_txn.open_table(PROVIDERS_TABLE) {
                let entries: Vec<(String, Vec<u8>)> = providers_table
                    .iter()?
                    .filter_map(|r| r.ok())
                    .map(|(key, value)| (key.value().to_string(), value.value().to_vec()))
                    .collect();

                let mut keys_to_update = Vec::new();

                for (key_str, data) in entries {
                    if let Ok(mut providers) = postcard::from_bytes::<Vec<StoredProvider>>(&data) {
                        let original_len = providers.len();
                        providers.retain(|p| !Self::is_expired(p.expires));

                        if providers.len() < original_len {
                            providers_cleaned += original_len - providers.len();
                            if !providers.is_empty() {
                                keys_to_update.push((key_str.clone(), providers));
                            } else {
                                // 所有提供者都过期了，删除整个键
                                let _ = providers_table.remove(key_str.as_str());
                            }
                        }
                    }
                }

                // 更新剩余的提供者记录
                for (key, providers) in keys_to_update {
                    if let Ok(serialized) = postcard::to_allocvec(&providers) {
                        let _ = providers_table.insert(key.as_str(), serialized.as_slice());
                    }
                }
            }

            Ok(())
        })?;

        if records_cleaned > 0 || providers_cleaned > 0 {
            tracing::info!(
                "清理了 {} 条过期DHT记录和 {} 条过期提供者记录",
                records_cleaned,
                providers_cleaned
            );
        }

        Ok((records_cleaned, providers_cleaned))
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
            signature: None,
            timestamp: None,
            salt: None,
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
            // 优化：预估容量，避免频繁重新分配
            // 假设平均记录大小，预估初始容量
            records.reserve(1000);

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

                        records.push(Cow::Owned(Record {
                            key: RecordKey::from(key_str.as_bytes().to_vec()),
                            value: stored.value,
                            publisher,
                            expires,
                        }));

                        // 性能优化：如果记录数量过多，提前返回以保持O(log n)响应性
                        // 在实际DHT使用中，不应该存储过多记录
                        if records.len() >= 10_000 {
                            tracing::warn!("DHT records() 返回超过10,000条记录，可能影响性能");
                            break;
                        }
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
            signature: None, // 无签名（libp2p 标准 ProviderRecord 不带签名元数据）
            timestamp: None, // 使用 add_signed_provider() 存储带签名的提供者记录
            salt: None,
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
                    match postcard::from_bytes(data.value()) {
                        Ok(parsed) => parsed,
                        Err(e) => {
                            tracing::warn!(
                                "DHT 提供者记录解析失败 (key={:?}), 将重置为空: {}",
                                key_str,
                                e
                            );
                            Vec::new()
                        }
                    }
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
            // 优化：预估容量
            provided_records.reserve(500);

            let iter = table.iter().ok();
            if let Some(mut iter) = iter {
                while let Some(Ok((key, value))) = iter.next() {
                    let key_str = key.value();
                    let data = value.value();
                    if let Ok(providers) = postcard::from_bytes::<Vec<StoredProvider>>(data) {
                        for stored in providers {
                            if !Self::is_expired(stored.expires) {
                                if let Ok(provider) = stored.provider.parse() {
                                    let expires = stored.expires.map(|exp| {
                                        Instant::now()
                                            + Duration::from_secs(
                                                exp.saturating_sub(Self::now_unix()),
                                            )
                                    });
                                    // 从 PEER_MULTIADDRS_TABLE 获取该 provider 的地址
                                    let addresses =
                                        self.get_multiaddrs(&provider).unwrap_or_default();

                                    provided_records.push(Cow::Owned(ProviderRecord {
                                        key: RecordKey::from(key_str.as_bytes().to_vec()),
                                        provider,
                                        expires,
                                        addresses,
                                    }));

                                    // 性能优化：限制返回记录数量
                                    if provided_records.len() >= 5_000 {
                                        tracing::warn!(
                                            "DHT provided() 返回超过5,000条记录，可能影响性能"
                                        );
                                        break;
                                    }
                                }
                            }
                        }
                        // 如果已经达到限制，跳出外层循环
                        if provided_records.len() >= 5_000 {
                            break;
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
