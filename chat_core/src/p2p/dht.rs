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
        // TODO: Implement full iteration
        Vec::new().into_iter()
    }

    fn add_provider(&mut self, record: ProviderRecord) -> Result<(), libp2p::kad::store::Error> {
        let key_str = std::str::from_utf8(record.key.as_ref())
            .map_err(|_| libp2p::kad::store::Error::MaxRecords)?;
        let stored = StoredProvider {
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
            // For simplicity, store as list, but actually need to handle multiple providers per key
            // In KAD, multiple providers per key
            let mut providers = {
                let existing = table.get(key_str).ok().flatten();
                if let Some(data) = existing {
                    postcard::from_bytes(data.value()).unwrap_or_default()
                } else {
                    Vec::new()
                }
            };
            providers.push(stored);
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
                            Some(ProviderRecord {
                                key: key.clone(),
                                provider,
                                expires,
                                addresses: vec![], // TODO: add addresses if needed
                            })
                        }
                    })
                    .collect();
            }
        }
        Vec::new()
    }

    fn provided(&self) -> Self::ProvidedIter<'_> {
        // TODO: Implement full iteration
        Vec::new().into_iter()
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
