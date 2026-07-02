/// 网络行为组合（libp2p NetworkBehaviour）
pub mod behaviour;
/// DHT 记录存储（Redb 后端，目前停用，未来可能考虑用于服务器节点）
pub mod dht;
mod events;
pub mod netevent;
pub mod nodes;
mod swarm;

pub use behaviour::MyBehaviour;
pub use events::handle_incoming_request;
pub use swarm::{save_routing_table, swarm_init};

use libp2p::PeerId;
use redb::Database;
use std::path::Path;
use std::sync::{Arc, LazyLock, Mutex};

use crate::error::{DhtError, DhtResult};

/// GetProviders 查询回调注册表
///
/// 当 `dht_lookup_peerid` 发起 GetProviders 查询时，注册一个 oneshot sender。
/// `actor/p2p/mod.rs` 中的 GetProvidersOk::FoundProviders 处理器会通过 key 查找对应的 sender
/// 并发送找到的 PeerID。
///
/// 使用 LazyLock 确保全局唯一实例，避免 static 初始化顺序问题。
pub(crate) static DHT_PROVIDER_CALLBACKS: LazyLock<
    Mutex<HashMap<String, tokio::sync::oneshot::Sender<PeerId>>>,
> = LazyLock::new(|| Mutex::new(HashMap::new()));

use std::collections::HashMap;

/// 从本地 DHT 数据库查询 ML-DSA 公钥对应的 PeerID
///
/// 此函数仅查询本地持久化的 DHT 数据库，不发起网络 DHT 查询。
///
/// # 参数
/// - `data_dir`: 数据目录路径
/// - `pubkey_hex`: ML-DSA 公钥的 hex 编码
/// - `dht_db`: 可选的共享 DHT 数据库连接。如果提供，优先使用该连接避免文件锁冲突。
///   如果为 None，则打开文件（适用于无 ChatCore 实例的场景）。
///
/// # 返回
/// - `Ok(Some(peer_id))`: 找到对应的 PeerID
/// - `Ok(None)`: 未找到记录
/// - `Err(e)`: 数据库错误
pub fn lookup_peerid_by_pubkey(
    data_dir: &Path,
    pubkey_hex: &str,
    dht_db: Option<Arc<Database>>,
) -> DhtResult<Option<PeerId>> {
    let store = if let Some(db) = dht_db {
        dht::RedbRecordStore::new(db)
    } else {
        let dht_path = data_dir.join("dht.redb");
        if !dht_path.exists() {
            return Ok(None);
        }
        let db = redb::Database::open(&dht_path).map_err(DhtError::CreateDatabaseFailed)?;
        dht::RedbRecordStore::new(std::sync::Arc::new(db))
    };
    store.get_peerid_by_pubkey(pubkey_hex)
}
