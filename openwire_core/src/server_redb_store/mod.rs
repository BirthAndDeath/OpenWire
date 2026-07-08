//! RedbRecordStore -- 基于 Redb 的持久化 Kademlia 记录存储。
//!
//! 当前客户端 Kademlia 使用 MemoryStore（libp2p 原生内存存储），
//! 此模块保留完整的 RecordStore trait 实现，
//! 可供未来服务器节点或需要持久化 Kademlia 记录的场景使用。
//!
//! 同时提供本地持久化缓存功能（pubkey-peerid、mlkem 公钥映射），
//! 客户端通过 get_dht_store() / lookup_peerid_by_pubkey() 访问。

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
    expires: Option<u64>,
}

#[derive(Serialize, Deserialize)]
struct StoredProvider {
    provider: String,
    expires: Option<u64>,
}

/// 基于 Redb 的持久化记录存储（完整 Kademlia RecordStore 实现 + 本地缓存）
pub struct RedbRecordStore {
    db: Arc<Database>,
}

impl RedbRecordStore {
    /// 创建新的 RedbRecordStore，自动创建所需表
    pub fn new(db: Arc<Database>) -> Self {
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

    fn with_write_txn<T>(
        &self,
        f: impl FnOnce(&redb::WriteTransaction) -> DhtResult<T>,
    ) -> DhtResult<T> {
        let write_txn = self.db.as_ref().begin_write()?;
        let result = f(&write_txn)?;
        write_txn.commit()?;
        Ok(result)
    }

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

    pub fn remove_mlkem_pubkey(&self, mldsa_pubkey_hex: &str) -> DhtResult<()> {
        self.with_write_txn(|write_txn| {
            let mut table = write_txn.open_table(PUBKEY_MLKEM_TABLE)?;
            table.remove(mldsa_pubkey_hex)?;
            Ok(())
        })
    }

    pub fn cleanup_expired_records(&self) -> DhtResult<(usize, usize)> {
        let mut records_cleaned = 0;
        let mut providers_cleaned = 0;

        self.with_write_txn(|write_txn| {
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
                                let _ = providers_table.remove(key_str.as_str());
                            }
                        }
                    }
                }

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
                "cleaned {} expired DHT records and {} expired provider records",
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

                        if records.len() >= 10_000 {
                            tracing::warn!("DHT records() returned more than 10,000 records, may affect performance");
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
                                "DHT provider record parse failed (key={:?}), resetting to empty: {}",
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
                                let addresses = self.get_multiaddrs(&provider).unwrap_or_default();

                                provided_records.push(Cow::Owned(ProviderRecord {
                                    key: RecordKey::from(key_str.as_bytes().to_vec()),
                                    provider,
                                    expires,
                                    addresses,
                                }));

                                if provided_records.len() >= 5_000 {
                                    tracing::warn!("DHT provided() returned more than 5,000 records, may affect performance");
                                    break;
                                }
                            }
                        }
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