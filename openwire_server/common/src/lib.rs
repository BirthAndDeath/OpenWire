use std::collections::HashMap;
use std::num::NonZero;
use std::path::Path;
use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use libp2p::futures::StreamExt;
use libp2p::kad::{self, Config as KadConfig};
use libp2p::request_response::{Config as RrConfig, ProtocolSupport, cbor, cbor::codec::Codec};
use libp2p::swarm::{NetworkBehaviour, SwarmEvent};
use libp2p::multiaddr::Protocol;
use libp2p::{
    Multiaddr, PeerId, StreamProtocol, SwarmBuilder, connection_limits, identify, identity,
    memory_connection_limits, noise, ping, relay, tcp, yamux,
};

use openwire_core::p2p::dht_cache::DhtCache;
use openwire_core::p2p::netevent::{NackReason, NetEventRequest, NetEventResponse};
use openwire_core::server_redb_store::RedbRecordStore;

const DHT_RELAY_INDEX_KEY: &str = "relay_nodes_public";
const PROTOCOL_KAD: StreamProtocol = StreamProtocol::new("/chat/kad/0.0.1");
const PROTOCOL_NETEVENT: StreamProtocol = StreamProtocol::new("/chat/rr_netevent/0.0.1");

#[derive(NetworkBehaviour)]
struct RelayBehaviour {
    relay: relay::Behaviour,
    kademlia: kad::Behaviour<RedbRecordStore>,
    identify: identify::Behaviour,
    ping: ping::Behaviour,
    rr_netevent: cbor::Behaviour<NetEventRequest, NetEventResponse>,
    limits: connection_limits::Behaviour,
    memory_limits: memory_connection_limits::Behaviour,
}

fn load_keypair(path: &Path) -> anyhow::Result<identity::Keypair> {
    if path.exists() {
        return Ok(identity::Keypair::from_protobuf_encoding(&std::fs::read(
            path,
        )?)?);
    }
    let kp = identity::Keypair::generate_ed25519();
    std::fs::create_dir_all(path.parent().unwrap())?;
    let encoded = kp.to_protobuf_encoding()?;
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).mode(0o600);
        let mut f = opts.open(path)?;
        f.write_all(&encoded)?;
    }
    #[cfg(not(unix))]
    std::fs::write(path, &encoded)?;
    Ok(kp)
}

pub async fn relay(dir: Option<&Path>, port: u16) -> anyhow::Result<()> {
    let dir = dir.unwrap_or_else(|| Path::new(".openwire-relay"));
    std::fs::create_dir_all(dir)?;

    let kp = load_keypair(&dir.join("ed25519.bin"))?;
    let peer_id = kp.public().to_peer_id();
    println!("中继公钥: PeerId={peer_id}");

    let nodes_cfg = openwire_core::p2p::nodes::NodesConfig::load(dir);
    let bootstrap_nodes = nodes_cfg.bootstrap_nodes;

    let mut kad_config = KadConfig::new(PROTOCOL_KAD);
    kad_config
        .set_query_timeout(Duration::from_secs(60))
        .set_replication_factor(NonZero::new(20).unwrap())
        .set_parallelism(NonZero::new(3).unwrap())
        .set_periodic_bootstrap_interval(Some(Duration::from_secs(300)))
        .set_provider_record_ttl(Some(Duration::from_secs(3600)))
        .set_publication_interval(Some(Duration::from_secs(3600)));

    let dht_cache = DhtCache::new();

    // 打开持久化 Kademlia 存储，服务器重启后路由表不丢失
    // 先以 0600 权限创建文件，再交由 redb 打开，避免路由表文件对其他用户可读
    let db_path = dir.join("dht.redb");
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .mode(0o600)
            .open(&db_path)
            .map_err(|e| anyhow::anyhow!("创建 DHT 数据库失败 {}: {e}", db_path.display()))?;
        drop(file);
    }
    let db = Arc::new(
        redb::Database::create(&db_path)
            .map_err(|e| anyhow::anyhow!("创建 DHT 数据库失败 {}: {e}", db_path.display()))?,
    );
    let mut swarm = SwarmBuilder::with_existing_identity(kp)
        .with_tokio()
        .with_tcp(
            tcp::Config::default(),
            noise::Config::new,
            yamux::Config::default,
        )
        .map_err(|e| anyhow::anyhow!("tcp: {e}"))?
        .with_quic()
        .with_dns()
        .map_err(|e| anyhow::anyhow!("dns: {e}"))?
        .with_behaviour(|key| {
            let relay_cfg = relay::Config {
                max_circuits: 50,
                max_circuits_per_peer: 5,
                max_reservations: 50,
                max_reservations_per_peer: 5,
                reservation_duration: Duration::from_secs(7200),
                max_circuit_duration: Duration::from_secs(3600),
                max_circuit_bytes: 100 << 20,
                ..Default::default()
            };
            let relay = relay::Behaviour::new(key.public().to_peer_id(), relay_cfg);

            let pid = key.public().to_peer_id();
            let mut kademlia = kad::Behaviour::with_config(
                pid,
                RedbRecordStore::new(db.clone()),
                kad_config.clone(),
            );

            // 向 bootstrap 节点注册，但排除自身（避免自引用）
            // 如果所有 bootstrap 节点都是自身，则跳过 bootstrap 注册，
            // 依靠客户端连接后通过 Identify 填充路由表。
            let mut has_remote = false;
            for node in &bootstrap_nodes {
                if let (Ok(boot_pid), Ok(addr)) = (PeerId::from_str(&node[0]), node[1].parse()) {
                    if boot_pid != pid {
                        kademlia.add_address(&boot_pid, addr);
                        has_remote = true;
                    }
                }
            }
            if has_remote {
                let _ = kademlia.bootstrap();
            } else {
                tracing::info!("无远程 bootstrap 节点，跳过 DHT bootstrap（依赖客户端 Identify 填充路由表）");
            }

            let identify = identify::Behaviour::new(
                identify::Config::new("/rootcell/identify/1.0.0".to_string(), key.public())
                    .with_agent_version("openwire-relay/0.1.0".to_string()),
            );
            let ping = ping::Behaviour::new(ping::Config::new());

            let netevent_codec = Codec::<NetEventRequest, NetEventResponse>::default()
                .set_request_size_maximum(65536)
                .set_response_size_maximum(1024);
            let netevent_rr_config = RrConfig::default()
                .with_request_timeout(Duration::from_secs(15))
                .with_max_concurrent_streams(10);
            let rr_netevent = cbor::Behaviour::with_codec(
                netevent_codec,
                [(PROTOCOL_NETEVENT, ProtocolSupport::Full)],
                netevent_rr_config,
            );

            let limits = connection_limits::Behaviour::new(
                connection_limits::ConnectionLimits::default()
                    .with_max_pending_incoming(Some(100))
                    .with_max_pending_outgoing(Some(50))
                    .with_max_established_incoming(Some(500))
                    .with_max_established_outgoing(Some(50)),
            );
            let memory_limits = memory_connection_limits::Behaviour::with_max_percentage(0.05);

            Ok(RelayBehaviour {
                relay,
                kademlia,
                identify,
                ping,
                rr_netevent,
                limits,
                memory_limits,
            })
        })
        .map_err(|e| anyhow::anyhow!("behaviour: {e}"))?
        .build();

    swarm.listen_on(
        Multiaddr::empty()
            .with(Protocol::from(std::net::Ipv4Addr::UNSPECIFIED))
            .with(Protocol::Tcp(port)),
    )?;
    swarm.listen_on(
        Multiaddr::empty()
            .with(Protocol::from(std::net::Ipv6Addr::UNSPECIFIED))
            .with(Protocol::Tcp(port)),
    )?;
    swarm.listen_on(
        Multiaddr::empty()
            .with(Protocol::from(std::net::Ipv4Addr::UNSPECIFIED))
            .with(Protocol::Udp(port))
            .with(Protocol::QuicV1),
    )?;
    swarm.listen_on(
        Multiaddr::empty()
            .with(Protocol::from(std::net::Ipv6Addr::UNSPECIFIED))
            .with(Protocol::Udp(port))
            .with(Protocol::QuicV1),
    )?;

    let relay_key = libp2p::kad::RecordKey::new(&DHT_RELAY_INDEX_KEY.as_bytes().to_vec());
    swarm.behaviour_mut().kademlia.start_providing(relay_key)?;

    let mut friend_online_rate: HashMap<PeerId, Instant> = HashMap::new();
    const FRIEND_ONLINE_COOLDOWN: Duration = Duration::from_secs(5);
    const FRIEND_ONLINE_RATE_MAX: usize = 10_000;

    loop {
        match swarm.select_next_some().await {
            SwarmEvent::NewListenAddr { address, .. } => {
                println!("Listening on {address}");
            }
            SwarmEvent::Behaviour(RelayBehaviourEvent::Relay(
                relay::Event::ReservationReqAccepted { src_peer_id, .. },
            )) => {
                tracing::info!("relay reservation from {src_peer_id}");
            }
            // 处理 FriendOnline 通知：缓存 (pubkey → PeerID) 映射
            SwarmEvent::Behaviour(RelayBehaviourEvent::RrNetevent(
                libp2p::request_response::Event::Message {
                    peer,
                    message:
                        libp2p::request_response::Message::Request {
                            request, channel, ..
                        },
                    ..
                },
            )) => {
                match &request {
                    NetEventRequest::FriendOnline {
                        mldsa_pubkey_hex,
                        peer_id,
                        listen_addrs,
                        mlkem_pubkey_hex,
                        signature,
                        ..
                    } => {
                        // 校验 PeerID 与签名：无效请求返回 Nack，不缓存
                        let mut response = NetEventResponse::Ack;
                        if *peer_id != peer.to_string() {
                            tracing::warn!(
                                "relay FriendOnline PeerID mismatch: claimed={}, actual={}",
                                peer_id,
                                peer
                            );
                            response = NetEventResponse::Nack {
                                reason: NackReason::PeerIdMismatch,
                            };
                        } else if let Some(sig) = signature {
                            if !openwire_core::p2p::netevent::verify_friend_online_signature(
                                mldsa_pubkey_hex,
                                peer_id,
                                listen_addrs,
                                mlkem_pubkey_hex,
                                sig,
                            ) {
                                tracing::warn!(
                                    "relay FriendOnline signature verification failed for {}..",
                                    &mldsa_pubkey_hex[..16.min(mldsa_pubkey_hex.len())]
                                );
                                response = NetEventResponse::Nack {
                                    reason: NackReason::SignatureVerificationFailed,
                                };
                            }
                        } else {
                            // 开发版本不做旧版本兼容：无签名请求直接拒绝，防止伪造 pubkey 毒化缓存
                            tracing::warn!(
                                "relay FriendOnline without signature rejected for {}..",
                                &mldsa_pubkey_hex[..16.min(mldsa_pubkey_hex.len())]
                            );
                            response = NetEventResponse::Nack {
                                reason: NackReason::SignatureVerificationFailed,
                            };
                        }
                        // 请求有效且通过校验后，受 5s 限流保护地写入缓存
                        if matches!(response, NetEventResponse::Ack) {
                            if friend_online_rate
                                .get(&peer)
                                .map_or(true, |last| last.elapsed() >= FRIEND_ONLINE_COOLDOWN)
                            {
                                let _ = dht_cache.set_pubkey_peerid(mldsa_pubkey_hex, &peer);
                                // 限流 map 达到上限时淘汰任意条目，防止恶意 PeerID 无限占满内存
                                if friend_online_rate.len() >= FRIEND_ONLINE_RATE_MAX
                                    && !friend_online_rate.contains_key(&peer)
                                    && let Some(evict) = friend_online_rate.keys().next().copied()
                                {
                                    friend_online_rate.remove(&evict);
                                    tracing::debug!("FriendOnline rate map at capacity, evicted {evict}");
                                }
                                friend_online_rate.insert(peer, Instant::now());
                                tracing::info!(
                                    "=== RELAY CACHED FriendOnline: {}.. -> {} ===",
                                    &mldsa_pubkey_hex[..16.min(mldsa_pubkey_hex.len())],
                                    peer
                                );
                            } else {
                                tracing::debug!("FriendOnline rate limited for {}", peer);
                            }
                        }
                        let _ = swarm
                            .behaviour_mut()
                            .rr_netevent
                            .send_response(channel, response);
                    }
                    NetEventRequest::DiscoverPeer { mldsa_pubkey_hex } => {
                        let peer_id = dht_cache
                            .get_peerid_by_pubkey(mldsa_pubkey_hex)
                            .ok()
                            .flatten();
                        // ML-KEM 公钥由 FriendOnline 直接携带，DHT 不缓存
                        match peer_id {
                        Some(pid) => {
                            tracing::info!(
                                "=== RELAY DISCOVER HIT: {}.. -> {} ===",
                                &mldsa_pubkey_hex[..16.min(mldsa_pubkey_hex.len())],
                                pid
                            );
                                let _ = swarm.behaviour_mut().rr_netevent.send_response(
                                    channel,
                                    NetEventResponse::PeerInfo {
                                        mldsa_pubkey_hex: mldsa_pubkey_hex.clone(),
                                        peer_id: pid.to_string(),
                                        mlkem_pubkey_hex: String::new(),
                                    },
                                );
                            }
                            None => {
                                tracing::info!(
                                    "=== RELAY DISCOVER MISS: {}.. (peer not found) ===",
                                    &mldsa_pubkey_hex[..16.min(mldsa_pubkey_hex.len())]
                                );
                                let _ = swarm.behaviour_mut().rr_netevent.send_response(
                                    channel,
                                    NetEventResponse::PeerNotFound {
                                        mldsa_pubkey_hex: mldsa_pubkey_hex.clone(),
                                    },
                                );
                            }
                        }
                    }
                    // 预留：当 NetEventRequest 新增变体时，直接忽略
                    _ => {
                        tracing::trace!("relay ignored NetEventRequest: {:?}", request);
                    }
                }
            }
            // 忽略其他事件
            _ => {}
        }
    }
}
