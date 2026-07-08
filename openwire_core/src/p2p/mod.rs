/// 网络行为组合（libp2p NetworkBehaviour）
pub mod behaviour;
/// DHT 内存缓存（替代旧的 redb 持久化存储）
pub mod dht_cache;
/// DHT 本地缓存查询
pub mod dht;
mod events;
pub mod netevent;
pub mod nodes;
mod swarm;

pub use behaviour::MyBehaviour;
pub use events::handle_incoming_request;
pub use swarm::{save_routing_table, swarm_init};

use libp2p::PeerId;
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

/// GetProviders 查询回调注册表
pub(crate) static DHT_PROVIDER_CALLBACKS: LazyLock<
    Mutex<HashMap<String, tokio::sync::oneshot::Sender<PeerId>>>,
> = LazyLock::new(|| Mutex::new(HashMap::new()));
