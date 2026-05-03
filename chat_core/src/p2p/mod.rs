mod behaviour;
mod bootstrap;
pub mod dht;
mod events;
mod swarm;
pub mod validator;

pub use behaviour::MyBehaviour;
pub use events::swarm_event;
pub use swarm::{SwarmWithValidator, swarm_init};
pub use validator::{RecordValidator, RecordValidatorConfig};

use libp2p::PeerId;
use std::path::Path;
use std::sync::OnceLock;

/// 全局 DHT 查询回调注册表
///
/// 用于在 Kademlia get_record 网络查询完成时，将结果回传给等待的调用方。
/// key: 查询 ID (String), value: oneshot Sender
static DHT_QUERY_CALLBACKS: OnceLock<
    std::sync::Mutex<
        std::collections::HashMap<String, tokio::sync::oneshot::Sender<Option<PeerId>>>,
    >,
> = OnceLock::new();

/// ML-KEM 查询回调：key: 查询 ID (String), value: oneshot Sender<Option<String>> (ML-KEM hex)
static MLKEM_QUERY_CALLBACKS: OnceLock<
    std::sync::Mutex<
        std::collections::HashMap<String, tokio::sync::oneshot::Sender<Option<String>>>,
    >,
> = OnceLock::new();

pub fn dht_query_callbacks() -> &'static std::sync::Mutex<
    std::collections::HashMap<String, tokio::sync::oneshot::Sender<Option<PeerId>>>,
> {
    DHT_QUERY_CALLBACKS.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

pub fn mlkem_query_callbacks() -> &'static std::sync::Mutex<
    std::collections::HashMap<String, tokio::sync::oneshot::Sender<Option<String>>>,
> {
    MLKEM_QUERY_CALLBACKS.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

/// 注册一个 DHT 查询回调，返回一个 Receiver 用于等待结果
///
/// # 参数
/// - `query_id`: 查询唯一标识（使用 pubkey_hex）
///
/// # 返回
/// - `Receiver` 用于异步等待查询结果
pub fn register_dht_query_callback(
    query_id: String,
) -> tokio::sync::oneshot::Receiver<Option<PeerId>> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    dht_query_callbacks().lock().unwrap().insert(query_id, tx);
    rx
}

/// 注册一个 ML-KEM 查询回调，返回一个 Receiver 用于等待结果
///
/// # 参数
/// - `query_id`: 查询唯一标识（使用 pubkey_hex）
///
/// # 返回
/// - `Receiver` 用于异步等待 ML-KEM 公钥 hex 结果
pub fn register_mlkem_query_callback(
    query_id: String,
) -> tokio::sync::oneshot::Receiver<Option<String>> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    mlkem_query_callbacks().lock().unwrap().insert(query_id, tx);
    rx
}

/// 完成一个 DHT 查询，将结果发送给等待的调用方
///
/// 由 events.rs 中的 handle_kademlia_event 在收到 GetRecord 结果时调用。
///
/// # 参数
/// - `query_id`: 查询唯一标识（使用 pubkey_hex）
/// - `result`: 查询结果（Some(PeerId) 表示找到，None 表示未找到）
pub fn complete_dht_query(query_id: &str, result: Option<PeerId>) {
    if let Some(tx) = dht_query_callbacks().lock().unwrap().remove(query_id) {
        let _ = tx.send(result);
    }
}

/// 从本地 DHT 数据库查询 ML-KEM 公钥对应的 PeerID
///
/// # 注意
/// 此函数仅查询本地持久化的 DHT 数据库，不发起网络 DHT 查询。
/// 如需网络查询，请使用 [`lookup_peerid_by_pubkey_network`]。
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

/// 通过 Kademlia 网络 DHT 查询 ML-DSA 公钥对应的 PeerID
///
/// 此函数先查询本地数据库，如果未找到则发起网络 DHT 查询（Kademlia get_record）。
/// 网络查询是异步的，通过 oneshot channel 等待结果，超时时间为 30 秒。
///
/// # 参数
/// - `swarm`: libp2p Swarm 的可变引用
/// - `data_dir`: 数据目录路径（用于本地查询）
/// - `pubkey_hex`: ML-DSA 公钥的 hex 编码（也是 DHT 记录键）
///
/// # 返回
/// - `Ok(Some(peer_id))`: 找到对应的 PeerID
/// - `Ok(None)`: 未找到记录
/// - `Err(e)`: 查询失败
pub async fn lookup_peerid_by_pubkey_network(
    swarm: &mut libp2p::Swarm<MyBehaviour>,
    data_dir: &Path,
    pubkey_hex: &str,
) -> anyhow::Result<Option<PeerId>> {
    // 1. 先查本地数据库
    if let Some(peer_id) = lookup_peerid_by_pubkey(data_dir, pubkey_hex)? {
        tracing::debug!(
            "DHT network lookup: found {} in local database",
            &pubkey_hex[..16]
        );
        return Ok(Some(peer_id));
    }

    // 2. 本地未找到，发起网络 Kademlia get_record 查询
    // DHT 记录键使用 "peerid:{pubkey_hex}" 格式
    let record_key = format!("peerid:{}", pubkey_hex);
    let key = libp2p::kad::RecordKey::new(&record_key);

    tracing::info!(
        "DHT network lookup: querying network for {} (key: {})",
        &pubkey_hex[..16],
        &record_key[..32]
    );

    // 注册回调，等待网络查询结果
    let rx = register_dht_query_callback(pubkey_hex.to_string());

    // 发起 Kademlia get_record 查询
    let query_id = swarm.behaviour_mut().kademlia.get_record(key);
    tracing::debug!("DHT get_record query started: {:?}", query_id);

    // 3. 等待结果（超时 30 秒）
    match tokio::time::timeout(std::time::Duration::from_secs(30), rx).await {
        Ok(Ok(Some(peer_id))) => {
            tracing::info!(
                "DHT network lookup: found PeerID {} for pubkey {}",
                peer_id,
                &pubkey_hex[..16]
            );
            Ok(Some(peer_id))
        }
        Ok(Ok(None)) => {
            tracing::info!(
                "DHT network lookup: no record found for pubkey {}",
                &pubkey_hex[..16]
            );
            Ok(None)
        }
        Ok(Err(_)) => {
            // oneshot channel 被关闭（发送端丢弃）
            tracing::warn!(
                "DHT network lookup: query cancelled for pubkey {}",
                &pubkey_hex[..16]
            );
            Ok(None)
        }
        Err(_) => {
            // 超时
            tracing::warn!(
                "DHT network lookup: timeout for pubkey {}",
                &pubkey_hex[..16]
            );
            // 清理回调
            dht_query_callbacks().lock().unwrap().remove(pubkey_hex);
            Ok(None)
        }
    }
}

/// 验证身份绑定：检查给定的 ML-DSA 公钥 hex 是否与 PeerID 和 ML-KEM 公钥一致
///
/// 此函数查询本地 DHT 数据库，验证身份绑定关系的完整性。
///
/// # 参数
/// - `data_dir`: 数据目录路径
/// - `mldsa_pubkey_hex`: 声称的 ML-DSA 公钥 hex
/// - `expected_peer_id`: 期望的 PeerID（如果为 None 则跳过检查）
/// - `expected_mlkem_pubkey_hex`: 期望的 ML-KEM 公钥 hex（如果为 None 则跳过检查）
///
/// # 返回
/// - `Ok(true)`: 绑定验证通过
/// - `Ok(false)`: 绑定验证失败
/// - `Err(e)`: 数据库错误
pub fn verify_identity_binding(
    data_dir: &Path,
    mldsa_pubkey_hex: &str,
    expected_peer_id: Option<&PeerId>,
    expected_mlkem_pubkey_hex: Option<&str>,
) -> anyhow::Result<bool> {
    let dht_path = data_dir.join("dht.redb");

    if !dht_path.exists() {
        return Ok(false);
    }

    let db = redb::Database::open(&dht_path)?;
    let store = dht::RedbRecordStore::new(std::sync::Arc::new(db));
    store.verify_identity_binding(
        mldsa_pubkey_hex,
        expected_peer_id,
        expected_mlkem_pubkey_hex,
    )
}
