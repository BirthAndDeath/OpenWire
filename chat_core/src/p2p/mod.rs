mod behaviour;
mod bootstrap;
pub mod dht;
mod events;
mod swarm;
mod validator;

pub use behaviour::MyBehaviour;
pub use events::swarm_event;
pub use swarm::swarm_init;
pub use validator::{ChallengeValidator, ChallengeValidatorConfig};

use libp2p::PeerId;
use std::path::Path;

/// 通过 ML-KEM 公钥从 DHT 查询对应的 PeerID
///
/// # 参数
/// - `data_dir`: 数据目录路径
/// - `pubkey_hex`: ML-KEM 公钥的 hex 编码
///
/// # 返回
/// - `Ok(Some(peer_id))`: 找到对应的 PeerID
/// - `Ok(None)`: 未找到记录
/// - `Err(e)`: 数据库错误
pub fn lookup_peerid_by_pubkey(
    data_dir: &Path,
    pubkey_hex: &str,
) -> anyhow::Result<Option<PeerId>> {
    let dht_path = data_dir.join("dht.redb");

    if !dht_path.exists() {
        return Ok(None);
    }

    let db = redb::Database::open(&dht_path)?;
    let store = dht::RedbRecordStore::new(std::sync::Arc::new(db));
    store.get_peerid_by_pubkey(pubkey_hex)
}
