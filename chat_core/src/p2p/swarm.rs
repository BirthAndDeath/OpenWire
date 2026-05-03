use anyhow::Context;
use libp2p::kad::{self, Config as KadConfig, Mode};
use libp2p::request_response::{Config as rrconfig, ProtocolSupport, cbor, cbor::codec::Codec};
use libp2p::{PeerId, StreamProtocol, Swarm, dcutr, identify, mdns, ping, relay};

use redb::Database;
use std::num::NonZero;
use std::ops::Deref;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use super::behaviour::MyBehaviour;
use super::bootstrap;
use super::dht;
use super::validator::{RecordValidator, RecordValidatorConfig};
use crate::{ChatMessage, ChatResponse};

use std::sync::LazyLock;

/// Kademlia 协议标识
static PROTOCOL_KAD: LazyLock<StreamProtocol> =
    LazyLock::new(|| StreamProtocol::new("/chat/kad/0.0.1"));
/// 消息请求-响应协议标识
static PROTOCOL_MSGRR: LazyLock<StreamProtocol> =
    LazyLock::new(|| StreamProtocol::new("/chat/rr_msg/0.0.1"));

/// 请求消息最大大小（1MB）
const REQUEST_SIZE_MAX: usize = 1024 * 1024;
/// 响应消息最大大小（1MB）
///
/// 虽然当前 ChatResponse 仅包含 timestamp（8 字节），但将响应大小限制提升至与请求一致，
/// 避免未来扩展时（如文件下载 chunk 响应）因大小限制不足导致通信失败。
const RESPONSE_SIZE_MAX: usize = 1024 * 1024;
/// Identify 缓存大小
const IDENTIFY_CACHE_SIZE: usize = 100;
/// Kademlia 查询超时（秒）
const KAD_QUERY_TIMEOUT_SECS: u64 = 60;
/// Kademlia 复制因子
const KAD_REPLICATION_FACTOR: usize = 20;
/// Kademlia 并行度
const KAD_PARALLELISM: usize = 3;
/// Kademlia 定期 bootstrap 间隔（秒）
const KAD_BOOTSTRAP_INTERVAL_SECS: u64 = 300;
/// Provider 记录 TTL（秒）- 24小时
const KAD_PROVIDER_TTL_SECS: u64 = 24 * 60 * 60;
/// Kademlia 发布间隔（秒）- 1小时
const KAD_PUBLICATION_INTERVAL_SECS: u64 = 60 * 60;
/// 每个节点最大记录数
const MAX_RECORDS_PER_PEER: usize = 1000;
/// 签名最大允许年龄（毫秒）
const MAX_SIGNATURE_AGE_MS: u64 = 60000;

/// Swarm 初始化结果，包含 swarm 和 validator
pub struct SwarmWithValidator {
    pub swarm: Swarm<MyBehaviour>,
    pub validator: Arc<std::sync::RwLock<RecordValidator>>,
}

/// 初始化 libp2p Swarm
///
/// # 网络栈配置
/// - 补充：QUIC（原生 TLS 1.3，性能更优）
/// - 发现：mDNS 局域网自动发现
/// - 消息：rrmsg
pub fn swarm_init(
    data_dir: &std::path::Path,
    keypair: libp2p::identity::Keypair,
) -> anyhow::Result<SwarmWithValidator> {
    // 创建记录验证器（基于签名验证）
    let validator_config = RecordValidatorConfig {
        max_records_per_peer: MAX_RECORDS_PER_PEER,
        strict_validation: true,
        max_signature_age_ms: MAX_SIGNATURE_AGE_MS,
    };
    let validator = Arc::new(std::sync::RwLock::new(RecordValidator::new(
        validator_config,
    )));

    let mut swarm = libp2p::SwarmBuilder::with_existing_identity(keypair)
        .with_tokio()
        // QUIC 传输层：内置 TLS 1.3，0-RTT，更好 NAT 穿透
        .with_quic()
        // DNS 解析（支持 /dns4, /dns6, /dnsaddr）
        .with_dns()?
        .with_behaviour(|key| {
            let peer_id = key.public().to_peer_id();

            let codec = Codec::<ChatMessage, ChatResponse>::default()
                .set_request_size_maximum(REQUEST_SIZE_MAX as u64)
                .set_response_size_maximum(RESPONSE_SIZE_MAX as u64);

            let rr_msg = cbor::Behaviour::with_codec(
                codec,
                [(PROTOCOL_MSGRR.deref().to_owned(), ProtocolSupport::Full)],
                rrconfig::default(),
            );

            // --- mDNS 配置 ---
            let mdns =
                mdns::tokio::Behaviour::new(mdns::Config::default(), key.public().to_peer_id())?;

            // 创建 Kademlia 行为实例（传入 validator 引用，用于 put() 签名验证）
            let kademlia = create_kademlia_with_validator(peer_id, data_dir, validator.clone())?;
            let identify_config =
                identify::Config::new("/rootcell/identify/1.0.0".to_string(), key.public())
                    .with_agent_version(format!("rootcell/{}", env!("CARGO_PKG_VERSION")))
                    .with_cache_size(IDENTIFY_CACHE_SIZE);
            let identify = identify::Behaviour::new(identify_config);
            let ping = ping::Behaviour::new(ping::Config::new());
            let relay = relay::Behaviour::new(key.public().to_peer_id(), Default::default());
            let dcutr = dcutr::Behaviour::new(key.public().to_peer_id());

            Ok(MyBehaviour {
                rr_msg,
                mdns,
                kademlia,
                ping,
                identify,
                relay,
                dcutr,
            })
        })?
        .build();

    // 端口 0 表示系统自动分配
    swarm.listen_on("/ip4/0.0.0.0/udp/0/quic-v1".parse()?)?;
    swarm.listen_on("/ip6/::/udp/0/quic-v1".parse()?)?;

    Ok(SwarmWithValidator { swarm, validator })
}

/// 创建 Kademlia 行为（带签名验证器）
fn create_kademlia_with_validator(
    peer_id: PeerId,
    data_dir: &std::path::Path,
    validator: Arc<std::sync::RwLock<RecordValidator>>,
) -> anyhow::Result<kad::Behaviour<dht::ResourceLimitedRecordStore>> {
    let db_path = data_dir.join("dht.redb");
    // 先删除旧数据库文件，再创建新的（切换身份时 DHT 存储需要重新初始化）
    let _ = std::fs::remove_file(&db_path);
    let db = Arc::new(Database::create(&db_path).context("Failed to create DHT database")?);

    // 配置资源限制
    let limits = dht::ResourceLimits {
        max_records_per_peer: MAX_RECORDS_PER_PEER,
        max_total_size: dht::DEFAULT_MAX_TOTAL_SIZE,
        enabled: true,
    };

    let mut store = dht::ResourceLimitedRecordStore::new(db, limits);
    // 将验证器注入存储，使 put() 能验证签名
    store.set_validator(validator);

    // 配置Kademlia网络参数
    let mut config = KadConfig::new(PROTOCOL_KAD.deref().to_owned());
    let replication_factor =
        NonZero::new(KAD_REPLICATION_FACTOR).context("KAD_REPLICATION_FACTOR must be non-zero")?;
    let parallelism = NonZero::new(KAD_PARALLELISM).context("KAD_PARALLELISM must be non-zero")?;
    let _ = &mut config
        .set_query_timeout(Duration::from_secs(KAD_QUERY_TIMEOUT_SECS))
        .set_replication_factor(replication_factor)
        .set_parallelism(parallelism)
        .set_periodic_bootstrap_interval(Some(Duration::from_secs(KAD_BOOTSTRAP_INTERVAL_SECS)))
        .set_provider_record_ttl(Some(Duration::from_secs(KAD_PROVIDER_TTL_SECS)))
        .set_publication_interval(Some(Duration::from_secs(KAD_PUBLICATION_INTERVAL_SECS)));

    let mut kademlia = kad::Behaviour::with_config(peer_id, store, config);
    kademlia.set_mode(Some(Mode::Server));
    for (peerid, addr) in bootstrap::BOOTSTRAP {
        let peer_id = PeerId::from_str(peerid).context("Failed to parse bootstrap peer ID")?;
        let multiaddr = bootstrap::resolve_dnsaddr(addr)?;
        kademlia.add_address(&peer_id, multiaddr);
    }
    if let Err(e) = kademlia.bootstrap() {
        tracing::trace!("Bootstrap error: {}", e);
    }
    Ok(kademlia)
}
