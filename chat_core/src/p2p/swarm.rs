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
use crate::{ChatMessage, ChatResponse};

use std::sync::LazyLock;

static PROTOCOL_KAD: LazyLock<StreamProtocol> =
    LazyLock::new(|| StreamProtocol::new("/chat/kad/0.0.1"));
static PROTOCOL_MSGRR: LazyLock<StreamProtocol> =
    LazyLock::new(|| StreamProtocol::new("/chat/rr_msg/0.0.1"));

/// 初始化 libp2p Swarm
///
/// # 网络栈配置
/// - 补充：QUIC（原生 TLS 1.3，性能更优）
/// - 发现：mDNS 局域网自动发现
/// - 消息：rrmsg
pub fn swarm_init(
    data_dir: &std::path::Path,
    keypair: libp2p::identity::Keypair,
) -> anyhow::Result<Swarm<MyBehaviour>> {
    let mut swarm = libp2p::SwarmBuilder::with_existing_identity(keypair)
        .with_tokio()
        // QUIC 传输层：内置 TLS 1.3，0-RTT，更好 NAT 穿透
        .with_quic()
        // DNS 解析（支持 /dns4, /dns6, /dnsaddr）
        .with_dns()?
        .with_behaviour(|key| {
            let peer_id = key.public().to_peer_id();

            let codec = Codec::<ChatMessage, ChatResponse>::default()
                .set_request_size_maximum(1024 * 1024) // 1MB
                .set_response_size_maximum(64 * 1024); // 64KB

            let rr_msg = cbor::Behaviour::with_codec(
                codec,
                [(PROTOCOL_MSGRR.deref().to_owned(), ProtocolSupport::Full)],
                rrconfig::default(),
            );

            // --- mDNS 配置 ---
            let mdns =
                mdns::tokio::Behaviour::new(mdns::Config::default(), key.public().to_peer_id())?;

            // 创建 Kademlia 行为实例
            let kademlia = create_kademlia(peer_id, data_dir);
            let identify_config =
                identify::Config::new("/rootcell/identify/1.0.0".to_string(), key.public())
                    .with_agent_version(format!("rootcell/{}", env!("CARGO_PKG_VERSION")))
                    .with_cache_size(100);
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

    Ok(swarm)
}

/// 创建 Kademlia 行为
fn create_kademlia(
    peer_id: PeerId,
    data_dir: &std::path::Path,
) -> kad::Behaviour<dht::ResourceLimitedRecordStore> {
    let db_path = data_dir.join("dht.redb");
    let db = Arc::new(Database::create(db_path).expect("Failed to create database"));

    // 配置资源限制
    let limits = dht::ResourceLimits {
        max_records_per_peer: 1000,
        max_total_size: 100 * 1024 * 1024, // 100MB
        enabled: true,
    };

    let store = dht::ResourceLimitedRecordStore::new(db, limits);

    // 配置Kademlia网络参数
    let mut config = KadConfig::new(PROTOCOL_KAD.deref().to_owned());
    let _ = &mut config
        .set_query_timeout(Duration::from_secs(60))
        .set_replication_factor(NonZero::new(20).unwrap())
        .set_parallelism(NonZero::new(3).unwrap())
        .set_periodic_bootstrap_interval(Some(Duration::from_secs(300)))
        .set_provider_record_ttl(Some(Duration::from_secs(24 * 60 * 60)))
        .set_publication_interval(Some(Duration::from_secs(60 * 60)));

    let mut kademlia = kad::Behaviour::with_config(peer_id, store, config);
    kademlia.set_mode(Some(Mode::Server));
    for (peerid, addr) in bootstrap::BOOTSTRAP {
        kademlia.add_address(
            &PeerId::from_str(peerid).unwrap(),
            bootstrap::resolve_dnsaddr(addr),
        );
    }
    if let Err(e) = kademlia.bootstrap() {
        tracing::trace!("Bootstrap error: {}", e);
    }
    kademlia
}
