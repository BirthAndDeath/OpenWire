use std::collections::{HashMap, HashSet};
use std::net::{Ipv4Addr, Ipv6Addr};
use std::num::NonZero;
use std::path::Path;
use std::path::PathBuf;
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

/// FriendOnline 限流冷却与表容量（防签名验证 CPU 冲刷）
const FRIEND_ONLINE_COOLDOWN: Duration = Duration::from_secs(5);
const FRIEND_ONLINE_RATE_MAX: usize = 10_000;
/// 限流表整体清理周期：剔除冷却期已过的条目，防止长期运行被递增 PeerID 占满常驻内存
const RATE_CLEANUP_INTERVAL: Duration = Duration::from_secs(300);

/// 中继信息输出文件：PeerId 与监听地址，供运维复制到客户端 nodes.json
const RELAY_INFO_FILE: &str = "relay-info.json";
const PORT_PREFERENCE_FILE: &str = "port-preference.txt";

/// 端口偏好持久化：保存上次使用的端口，重启后优先使用。
/// CLI 参数 > 持久化端口 > 默认 44909。
fn load_port_preference(dir: &Path) -> Option<u16> {
    let path = dir.join(PORT_PREFERENCE_FILE);
    std::fs::read_to_string(&path).ok()?.trim().parse().ok()
}

fn save_port_preference(dir: &Path, port: u16) {
    let path = dir.join(PORT_PREFERENCE_FILE);
    if let Err(e) = std::fs::write(&path, port.to_string()) {
        tracing::warn!("Failed to save port preference: {e}");
    }
}

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
    let parent = path.parent().ok_or_else(|| anyhow::anyhow!("密钥路径 {} 没有父目录", path.display()))?;
    std::fs::create_dir_all(parent)?;
    let encoded = kp.to_protobuf_encoding()?;
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true).mode(0o600);
        let mut f = opts.open(path)?;
        f.write_all(&encoded)?;
    }
    #[cfg(windows)]
    {
        use std::io::Write;
        use std::os::windows::fs::OpenOptionsExt;
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).share_mode(0);
        let mut f = opts.open(path)?;
        f.write_all(&encoded)?;
    }
    #[cfg(not(any(unix, windows)))]
    std::fs::write(path, &encoded)?;
    Ok(kp)
}

pub async fn relay(dir: Option<&Path>, port: Option<u16>) -> anyhow::Result<()> {
    let dir: PathBuf = dir.unwrap_or_else(|| Path::new(".openwire-relay")).to_path_buf();
    std::fs::create_dir_all(&dir)?;

    // 端口优先级：CLI 参数 > 持久化端口 > 默认 44909（兼容既有安装/防火墙配置）
    let port = match port {
        Some(p) => p,
        None => load_port_preference(&dir).unwrap_or(44909),
    };
    if port == 0 {
        tracing::info!("端口由 OS 分配 / port assigned by OS");
    } else {
        tracing::info!("使用端口 / using port: {port}");
    }

    let kp = load_keypair(&dir.join("ed25519.bin"))?;
    let peer_id = kp.public().to_peer_id();
    println!("中继公钥: PeerId={peer_id}");

    let nodes_cfg = openwire_core::p2p::nodes::NodesConfig::load(&dir);
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
            // === 中继资源配额（可由环境变量收紧；默认值即此前硬编码阈值）===
            // 开放中继可被任意 libp2p 节点用于匿名流量中转（带宽吸血），
            // 运行方应据部署环境收紧如下配额 / 或按 README 用 OS 防火墙做 Peer 白名单。
            let env_usize = |name: &str, dft: usize| -> usize {
                std::env::var(name)
                    .ok()
                    .and_then(|v| v.trim().parse().ok())
                    .unwrap_or(dft)
            };
            let env_u64 = |name: &str, dft: u64| -> u64 {
                std::env::var(name)
                    .ok()
                    .and_then(|v| v.trim().parse().ok())
                    .unwrap_or(dft)
            };
            let relay_cfg = relay::Config {
                max_circuits: env_usize("OPENWIRE_RELAY_MAX_CIRCUITS", 50),
                max_circuits_per_peer: env_usize("OPENWIRE_RELAY_MAX_CIRCUITS_PER_PEER", 5),
                max_reservations: env_usize("OPENWIRE_RELAY_MAX_RESERVATIONS", 50),
                max_reservations_per_peer: env_usize("OPENWIRE_RELAY_MAX_RESERVATIONS_PER_PEER", 5),
                reservation_duration: Duration::from_secs(env_u64(
                    "OPENWIRE_RELAY_RESERVATION_DURATION_SECS",
                    7200,
                )),
                max_circuit_duration: Duration::from_secs(env_u64(
                    "OPENWIRE_RELAY_MAX_CIRCUIT_DURATION_SECS",
                    3600,
                )),
                max_circuit_bytes: env_u64("OPENWIRE_RELAY_MAX_CIRCUIT_BYTES", 100 << 20),
                ..Default::default()
            };
            let relay = relay::Behaviour::new(key.public().to_peer_id(), relay_cfg);

            let pid = key.public().to_peer_id();
            let mut kademlia = kad::Behaviour::with_config(
                pid,
                RedbRecordStore::new(db.clone()),
                kad_config.clone(),
            );
            // 专用服务器：固定服务入站 DHT 查询。
            // kad 默认 Client 模式会拒绝全部入站 Kademlia 子流（DeniedUpgrade），
            // 且本 server 无 autonat/upnp，auto_mode 在无外部地址确认时永远不会升 Server，
            // 导致 provider 记录无法被存储/响应、客户端 bootstrap 查询全部落空。
            kademlia.set_mode(Some(kad::Mode::Server));

            // 向 bootstrap 节点注册，但排除自身（避免自引用）
            // 如果所有 bootstrap 节点都是自身，则跳过 bootstrap 注册，
            // 依靠客户端连接后通过 Identify 填充路由表。
            let mut has_remote = false;
            for node in &bootstrap_nodes {
                if let (Ok(boot_pid), Ok(addr)) = (PeerId::from_str(&node[0]), node[1].parse())
                    && boot_pid != pid {
                        kademlia.add_address(&boot_pid, addr);
                        has_remote = true;
                    }
            }
            if has_remote {
                let _ = kademlia.bootstrap();
            } else {
                tracing::info!("无远程 bootstrap 节点，跳过 DHT bootstrap（依赖客户端 Identify 填充路由表）");
            }

            let identify = identify::Behaviour::new(
                identify::Config::new("/rootcell/identify/1.0.0".to_string(), key.public())
                    .with_agent_version("openwire-relay/0.2.0".to_string()),
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

    // 各地址族独立尝试监听：端口被占用时回退到 OS 分配端口，仅记录警告不中断启动。
    try_listen_or_fallback(
        &mut swarm,
        Multiaddr::empty()
            .with(Protocol::from(Ipv4Addr::UNSPECIFIED))
            .with(Protocol::Tcp(port)),
        Multiaddr::empty()
            .with(Protocol::from(Ipv4Addr::UNSPECIFIED))
            .with(Protocol::Tcp(0)),
    );
    try_listen_or_fallback(
        &mut swarm,
        Multiaddr::empty()
            .with(Protocol::from(Ipv6Addr::UNSPECIFIED))
            .with(Protocol::Tcp(port)),
        Multiaddr::empty()
            .with(Protocol::from(Ipv6Addr::UNSPECIFIED))
            .with(Protocol::Tcp(0)),
    );
    try_listen_or_fallback(
        &mut swarm,
        Multiaddr::empty()
            .with(Protocol::from(Ipv4Addr::UNSPECIFIED))
            .with(Protocol::Udp(port))
            .with(Protocol::QuicV1),
        Multiaddr::empty()
            .with(Protocol::from(Ipv4Addr::UNSPECIFIED))
            .with(Protocol::Udp(0))
            .with(Protocol::QuicV1),
    );
    try_listen_or_fallback(
        &mut swarm,
        Multiaddr::empty()
            .with(Protocol::from(Ipv6Addr::UNSPECIFIED))
            .with(Protocol::Udp(port))
            .with(Protocol::QuicV1),
        Multiaddr::empty()
            .with(Protocol::from(Ipv6Addr::UNSPECIFIED))
            .with(Protocol::Udp(0))
            .with(Protocol::QuicV1),
    );

    let relay_key = libp2p::kad::RecordKey::new(&DHT_RELAY_INDEX_KEY.as_bytes().to_vec());
    swarm.behaviour_mut().kademlia.start_providing(relay_key)?;

    // 定时清理已过期的 DHT 记录与 provider 记录，防止 redb 数据库无限增长。
    // 清理在独立任务中执行，避免阻塞 swarm 主循环。
    {
        let cleanup_db = db.clone();
        tokio::spawn(async move {
            let store = RedbRecordStore::new(cleanup_db);
            let mut interval = tokio::time::interval(Duration::from_secs(3600));
            interval.tick().await; // 跳过首次立即触发
            loop {
                interval.tick().await;
                match store.cleanup_expired_records() {
                    Ok(_) => {}
                    Err(e) => tracing::warn!("DHT expired record cleanup failed: {e}"),
                }
            }
        });
    }

    let mut friend_online_rate: HashMap<PeerId, Instant> = HashMap::new();
    let mut rate_last_cleanup: Instant = Instant::now();

    // 监听地址集合：NewListenAddr 事件到达后重写 relay-info.json
    let mut listened: HashSet<Multiaddr> = HashSet::new();
    write_relay_info(&dir, &peer_id, &listened);
    println!(
        "中继信息已写入 / Relay info written to: {:?}",
        dir.join(RELAY_INFO_FILE)
    );

    loop {
        match swarm.select_next_some().await {
            SwarmEvent::NewListenAddr { address, .. } => {
                println!("Listening on {address}");
                // 仅由 TCP 地址更新端口偏好，避免 QUIC/UDP 事件覆盖 TCP 端口
                let port = address.iter().find_map(|p| match p {
                    Protocol::Tcp(p) => Some(p),
                    _ => None,
                });
                if let Some(p) = port {
                    save_port_preference(&dir, p);
                }
                listened.insert(address);
                write_relay_info(&dir, &peer_id, &listened);
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
                        // 防御上限：超大公钥 hex 会让签名验证/hex::decode 产生巨额
                        // 分配与 CPU 开销（公网攻击面），先于限流与验证拒绝。
                        if mldsa_pubkey_hex.len()
                            > openwire_core::p2p::netevent::MAX_MLDSA_PUBKEY_HEX_LEN
                        {
                            tracing::warn!(
                                "relay FriendOnline 公钥长度异常 ({}), 拒绝",
                                mldsa_pubkey_hex.len()
                            );
                            let _ = swarm
                                .behaviour_mut()
                                .rr_netevent
                                .send_response(
                                    channel,
                                    NetEventResponse::Nack {
                                        reason: NackReason::Other {
                                            description: "Invalid mldsa_pubkey_hex length"
                                                .to_string(),
                                        },
                                    },
                                );
                            continue;
                        }
                        // 过期条目按需剔除：冷却期已过则不再限流
                        if friend_online_rate
                            .get(&peer)
                            .is_some_and(|last| last.elapsed() >= FRIEND_ONLINE_COOLDOWN)
                        {
                            friend_online_rate.remove(&peer);
                        }
                        // 周期性整体清理：防止长期运行被递增 PeerID 占满常驻内存
                        if rate_last_cleanup.elapsed() >= RATE_CLEANUP_INTERVAL {
                            friend_online_rate
                                .retain(|_, last| last.elapsed() < FRIEND_ONLINE_COOLDOWN);
                            rate_last_cleanup = Instant::now();
                        }

                        // 限流检查前置：冷确期内直接 Ack 跳过昂贵的 ML-DSA 签名验证，
                        // 防止攻击者用大量连接强制持续的签名验证 CPU 消耗。
                        if friend_online_rate
                            .get(&peer)
                            .is_some_and(|last| last.elapsed() < FRIEND_ONLINE_COOLDOWN)
                        {
                            tracing::debug!("FriendOnline rate limited for {}", peer);
                            let _ = swarm
                                .behaviour_mut()
                                .rr_netevent
                                .send_response(channel, NetEventResponse::Ack);
                            // 预防性：不清空 map 条目，让重放攻击者逐渐填满自己的条目
                            continue;
                        }

                        // 校验 PeerID 与签名：无效请求返回 Nack，不缓存
                        let mut response = NetEventResponse::Ack;
                        let claimed_peer: Option<PeerId> = peer_id.parse().ok();
                        if claimed_peer != Some(peer) {
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
                        // 请求有效且通过校验后，写入缓存并记录限流时间
                        if matches!(response, NetEventResponse::Ack) {
                            dht_cache.set_pubkey_peerid(mldsa_pubkey_hex, &peer);
                            // 同步缓存摘要→PeerID：扫码方仅持 SHA256(公钥) 摘要，
                            // 凭摘要即可反查对端当前 PeerID，解决扫码暂存长期卡等。
                            dht_cache.set_digest_peerid(
                                &openwire_core::actor::p2p::swarm_ops::dht_key(mldsa_pubkey_hex),
                                &peer,
                            );
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
                        }
                        let _ = swarm
                            .behaviour_mut()
                            .rr_netevent
                            .send_response(channel, response);
                    }
                    NetEventRequest::DiscoverPeer { mldsa_pubkey_hex } => {
                        // 查询载荷可为完整 ML-DSA 公钥（3904 hex）或 SHA256(公钥) 摘要
                        // （64 hex，扫码暂存方只持摘要）。ML-KEM 公钥由 FriendOnline
                        // 直接携带，DHT 不缓存。
                        let peer_id = dht_cache
                            .get_peerid_by_pubkey(mldsa_pubkey_hex)
                            .or_else(|| dht_cache.get_peerid_by_digest(mldsa_pubkey_hex));
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
            // 客户端连接后通过 Identify 回填路由表。
            // 与 bootstrap 自排除注释一致：无远程 bootstrap 时依赖客户端填充，
            // 否则本 server 作为 Listener 侧不记录对端地址，路由表会一直为空。
            SwarmEvent::Behaviour(RelayBehaviourEvent::Identify(
                identify::Event::Received { peer_id, info, .. },
            )) => {
                for multiaddr in info.listen_addrs {
                    swarm.behaviour_mut().kademlia.add_address(&peer_id, multiaddr);
                }
                tracing::debug!(
                    "identified {peer_id} with {} protocols",
                    info.protocols.len()
                );
            }
            // 忽略其他事件
            _ => {}
        }
    }
}

/// 尝试用偏好端口监听，失败则回退到 OS 分配端口（port 0）。
///
/// 中继服务器的端口常被客户端硬编码，但端口被占用时不应让整个服务器崩溃；
/// 记录警告并继续运行，监听地址通过 `NewListenAddr` 事件输出。
fn try_listen_or_fallback(swarm: &mut libp2p::swarm::Swarm<RelayBehaviour>, preferred: Multiaddr, fallback: Multiaddr) {
    match swarm.listen_on(preferred.clone()) {
        Ok(_) => tracing::debug!("Listening on preferred {}", preferred),
        Err(e) => {
            tracing::warn!("Preferred {} unavailable ({e}), falling back to OS-assigned port", preferred);
            tracing::debug!("Attempting fallback listen on {fallback}");
            if let Err(fe) = swarm.listen_on(fallback.clone()) {
                tracing::error!("Listen fallback {fallback} failed: {fe}");
            }
        }
    }
}

/// 将中继服务器的 PeerId 与监听地址写入数据目录 `relay-info.json`，
///
/// 地址附加 `/p2p/<peer_id>` 后缀，格式与客户端配置一致。
/// 端口从实际监听地址解析（优先 TCP），避免使用偏好端口掩盖回退分配。
fn write_relay_info(dir: &Path, peer_id: &PeerId, addresses: &HashSet<Multiaddr>) {
    if addresses.is_empty() {
        tracing::error!("No listen addresses available yet, relay-info.json will have port=0");
    }
    let port = {
        let mut tcp_port = 0u16;
        let mut udp_port = 0u16;
        for addr in addresses {
            for p in addr.iter() {
                match p {
                    Protocol::Tcp(p) => tcp_port = p,
                    Protocol::Udp(p) => udp_port = p,
                    _ => {}
                }
            }
        }
        // 优先 TCP，其次 UDP
        if tcp_port != 0 { tcp_port } else { udp_port }
    };
    let list: Vec<String> = addresses
        .iter()
        .filter(|a| {
            // 过滤掉 0.0.0.0 和 [::] 等未指定地址，避免运维误用
            !a.iter().any(|p| matches!(p, Protocol::Ip4(ip) if ip.is_unspecified()))
                && !a.iter().any(|p| matches!(p, Protocol::Ip6(ip) if ip.is_unspecified()))
        })
        .map(|a| format!("{a}/p2p/{peer_id}"))
        .collect();
    let mut sorted = list;
    sorted.sort();
    let info = serde_json::json!({
        "peer_id": peer_id.to_string(),
        "port": port,
        "addresses": sorted,
        "note": "Actual listening port & addresses. Replace /ip4/0.0.0.0 with the server's public IP. Add an entry to client nodes.json relay_nodes. WARNING: this relay is OPEN by default (any libp2p peer can use it) and may be abused for anonymous traffic relay; enforce PeerID allowlisting via OS firewall and tighten OPENWIRE_RELAY_MAX_* quotas for public deployments — see openwire_server/README.md"
    });
    let path = dir.join(RELAY_INFO_FILE);
    match serde_json::to_string_pretty(&info) {
        Ok(s) => {
            if let Err(e) = std::fs::write(&path, format!("{s}\n")) {
                tracing::warn!("Failed to write relay info file {:?}: {e}", path);
            }
        }
        Err(e) => tracing::warn!("Failed to serialize relay info: {e}"),
    }
}
