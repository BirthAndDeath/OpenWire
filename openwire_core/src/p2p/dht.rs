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

const RECORDS_TABLE: TableDefinition<&str, &[u8]> = TableDefinition::new("records");
const PROVIDERS_TABLE: TableDefinition<&str, &[u8]> = TableDefinition::new("providers");
const PEER_MULTIADDRS_TABLE: TableDefinition<&str, &[u8]> = TableDefinition::new("peer_multiaddrs");
const PUBKEY_PEERID_TABLE: TableDefinition<&str, &str> = TableDefinition::new("pubkey_peerid");
const PUBKEY_MLKEM_TABLE: TableDefinition<&str, &str> = TableDefinition::new("pubkey_mlkem");

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
    #[allow(dead_code)]
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

                                // 性能优化：限制返回记录数量
                                if provided_records.len() >= 5_000 {
                                    tracing::warn!(
                                        "DHT provided() 返回超过5,000条记录，可能影响性能"
                                    );
                                    break;
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
