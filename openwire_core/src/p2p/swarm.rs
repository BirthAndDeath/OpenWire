use libp2p::kad::store::MemoryStore;
use libp2p::kad::{self, Config as KadConfig, Mode};
use libp2p::multiaddr::Protocol;
use libp2p::request_response::{Config as rrconfig, ProtocolSupport, cbor, cbor::codec::Codec};
use libp2p::{
    Multiaddr, PeerId, StreamProtocol, Swarm, autonat, connection_limits, dcutr, identify, mdns,
    memory_connection_limits, noise, ping, tcp, yamux,
};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use std::num::NonZero;
use std::ops::Deref;
use std::path::Path;
use std::str::FromStr;

use super::behaviour::MyBehaviour;
use super::netevent::{NetEventRequest, NetEventResponse};
use crate::error::{P2pError, P2pResult};
use crate::{ChatMessage, ChatResponse};

use std::sync::LazyLock;

/// Kademlia 协议标识
static PROTOCOL_KAD: LazyLock<StreamProtocol> =
    LazyLock::new(|| StreamProtocol::new("/chat/kad/0.0.1"));
/// 消息请求-响应协议标识
static PROTOCOL_MSGRR: LazyLock<StreamProtocol> =
    LazyLock::new(|| StreamProtocol::new("/chat/rr_msg/0.0.1"));
/// 网络事件通知协议标识
static PROTOCOL_NETEVENT: LazyLock<StreamProtocol> =
    LazyLock::new(|| StreamProtocol::new("/chat/rr_netevent/0.0.1"));

// ============================================================================
// DDoS 防护配置
// ============================================================================

/// 请求消息最大大小（非文件消息，256KB）
const REQUEST_SIZE_MAX: usize = 256 * 1024;
/// 响应消息最大大小（256KB）
const RESPONSE_SIZE_MAX: usize = 256 * 1024;

/// Identify 协议缓存大小
const IDENTIFY_CACHE_SIZE: usize = 100;

/// 空闲连接超时（秒）—— relay 连接需要更宽容的超时
const IDLE_CONNECTION_TIMEOUT_SECS: u64 = 90;

/// 每个连接最大并发协商入站流数
const MAX_NEGOTIATING_INBOUND_STREAMS: usize = 5;

/// request-response 协议最大并发流数
const RR_MAX_CONCURRENT_STREAMS: usize = 10;

/// 并发拨号因子 —— 加速批量 relay 建立
const DIAL_CONCURRENCY_FACTOR: u8 = 5;

// ============================================================================
// Kademlia 配置
// ============================================================================

/// Kademlia 查询超时（秒）
const KAD_QUERY_TIMEOUT_SECS: u64 = 60;
/// Kademlia 复制因子
const KAD_REPLICATION_FACTOR: usize = 20;
/// Kademlia 并行度
const KAD_PARALLELISM: usize = 3;
/// Kademlia 定期 bootstrap 间隔（秒）
const KAD_BOOTSTRAP_INTERVAL_SECS: u64 = 300;
/// Provider 记录 TTL（秒）- 1小时（中继节点离线后快速淘汰）
const KAD_PROVIDER_TTL_SECS: u64 = 60 * 60;
/// Kademlia 发布间隔（秒）- 1小时
const KAD_PUBLICATION_INTERVAL_SECS: u64 = 60 * 60;

/// 初始化 libp2p Swarm
///
/// # 参数
/// - `data_dir`: 数据目录路径
/// - `keypair`: libp2p 身份密钥对
/// - `relay_nodes`: Relay 中继节点列表 [(PeerId, Multiaddr)]，用于 NAT 穿透
/// - `bootstrap_nodes`: Bootstrap 引导节点列表 [(PeerId, Multiaddr)]，用于 DHT 网络引导
/// - `preferred_ports`: 可选端口偏好（来自 PeerIdConfig），端口被占用时自动回退到 OS 分配
pub fn swarm_init(
    data_dir: &Path,
    keypair: libp2p::identity::Keypair,
    bootstrap_nodes: &[(String, String)],
    peerid_config: Option<&crate::peerid_store::PeerIdConfig>,
) -> P2pResult<Swarm<MyBehaviour>> {
    let peerid_was_rotated = peerid_config.map_or(true, |c| c.was_rotated());
    let mut swarm = {
        let builder = libp2p::SwarmBuilder::with_existing_identity(keypair)
            .with_tokio()
            .with_tcp(
                tcp::Config::default(),
                noise::Config::new,
                yamux::Config::default,
            )
            .map_err(|e| P2pError::SwarmInitFailed(e.into()))?
            .with_quic();

        #[cfg(not(target_os = "android"))]
        let builder = builder
            .with_dns()
            .map_err(|e| P2pError::SwarmInitFailed(e.into()))?;
        #[cfg(not(target_os = "android"))] //似乎会在安卓端出bug
        let builder = futures::executor::block_on(
            builder.with_websocket(noise::Config::new, yamux::Config::default),
        )
        .map_err(|e| P2pError::SwarmInitFailed(e.into()))?;

        builder
            .with_relay_client(noise::Config::new, yamux::Config::default)
            .map_err(|e| P2pError::SwarmInitFailed(e.into()))?
            .with_behaviour(|key, relay_behaviour| {
                let peer_id = key.public().to_peer_id();

                // --- rr_msg: 消息传输 ---
                let msg_codec = Codec::<ChatMessage, ChatResponse>::default()
                    .set_request_size_maximum(REQUEST_SIZE_MAX as u64)
                    .set_response_size_maximum(RESPONSE_SIZE_MAX as u64);
                let rr_config = rrconfig::default()
                    .with_request_timeout(Duration::from_secs(30))
                    .with_max_concurrent_streams(RR_MAX_CONCURRENT_STREAMS);
                let rr_msg = cbor::Behaviour::with_codec(
                    msg_codec,
                    [(PROTOCOL_MSGRR.deref().to_owned(), ProtocolSupport::Full)],
                    rr_config,
                );

                // --- rr_netevent: 网络事件通知 ---
                let netevent_codec = Codec::<NetEventRequest, NetEventResponse>::default()
                    .set_request_size_maximum(64 * 1024) // 64KB 足够
                    .set_response_size_maximum(1024); // 响应很小
                let netevent_rr_config = rrconfig::default()
                    .with_request_timeout(Duration::from_secs(15))
                    .with_max_concurrent_streams(RR_MAX_CONCURRENT_STREAMS);
                let rr_netevent = cbor::Behaviour::with_codec(
                    netevent_codec,
                    [(PROTOCOL_NETEVENT.deref().to_owned(), ProtocolSupport::Full)],
                    netevent_rr_config,
                );

                // --- mDNS 配置 ---
                let mdns =
                    mdns::tokio::Behaviour::new(mdns::Config::default(), key.public().to_peer_id())
                        .map_err(|e| P2pError::SwarmInitFailed(e.into()))?;

                // 创建 Kademlia 行为实例
                let kademlia = create_kademlia(peer_id, data_dir, bootstrap_nodes, peerid_was_rotated)?;
                let identify_config =
                    identify::Config::new("/rootcell/identify/1.0.0".to_string(), key.public())
                        .with_agent_version(format!("rootcell/{}", env!("CARGO_PKG_VERSION")))
                        .with_cache_size(IDENTIFY_CACHE_SIZE);
                let identify = identify::Behaviour::new(identify_config);
                let ping = ping::Behaviour::new(ping::Config::new());
                let dcutr = dcutr::Behaviour::new(key.public().to_peer_id());
                let limits = connection_limits::Behaviour::new(
                    connection_limits::ConnectionLimits::default()
                        .with_max_pending_incoming(Some(50))
                        .with_max_pending_outgoing(Some(50))
                        .with_max_established_incoming(Some(200))
                        .with_max_established_outgoing(Some(50)),
                );
                let memory_limits = memory_connection_limits::Behaviour::with_max_percentage(0.05);
                let autonat =
                    autonat::Behaviour::new(key.public().to_peer_id(), Default::default());
                let upnp = libp2p::upnp::tokio::Behaviour::default();
                #[cfg(feature = "pathranker")]
                let pathranker = libp2p_pathranker::PathRankerBehaviour::new(key.clone());
                Ok(MyBehaviour {
                    autonat,
                    upnp,
                    rr_msg,
                    rr_netevent,
                    mdns,
                    kademlia,
                    ping,
                    identify,
                    relay_client: relay_behaviour,
                    dcutr,
                    limits,
                    memory_limits,
                    #[cfg(feature = "pathranker")]
                    pathranker,
                })
            })
            .map_err(|e| P2pError::SwarmInitFailed(e.into()))?
            .with_swarm_config(|cfg| {
                cfg.with_idle_connection_timeout(Duration::from_secs(IDLE_CONNECTION_TIMEOUT_SECS))
                    .with_max_negotiating_inbound_streams(MAX_NEGOTIATING_INBOUND_STREAMS)
                    .with_dial_concurrency_factor(
                        NonZero::new(DIAL_CONCURRENCY_FACTOR).expect("DIAL_CONCURRENCY_FACTOR > 0"),
                    )
            })
            .build()
    };

    // 端口偏好：优先使用 PeerIdConfig 中存储的端口，被占用则回退到 OS 分配
    let quic_port = peerid_config.map_or(0, |p| p.preferred_quic_port());
    let tcp_port = peerid_config.map_or(0, |p| p.preferred_tcp_port());
    let ws_port = peerid_config.map_or(0, |p| p.preferred_ws_port());

    use std::net::{Ipv4Addr, Ipv6Addr};
    try_listen_or_fallback(
        &mut swarm,
        Multiaddr::empty()
            .with(Protocol::from(Ipv4Addr::UNSPECIFIED))
            .with(Protocol::Udp(quic_port))
            .with(Protocol::QuicV1),
        Multiaddr::empty()
            .with(Protocol::from(Ipv4Addr::UNSPECIFIED))
            .with(Protocol::Udp(0))
            .with(Protocol::QuicV1),
    )?;
    try_listen_or_fallback(
        &mut swarm,
        Multiaddr::empty()
            .with(Protocol::from(Ipv6Addr::UNSPECIFIED))
            .with(Protocol::Udp(quic_port))
            .with(Protocol::QuicV1),
        Multiaddr::empty()
            .with(Protocol::from(Ipv6Addr::UNSPECIFIED))
            .with(Protocol::Udp(0))
            .with(Protocol::QuicV1),
    )?;
    try_listen_or_fallback(
        &mut swarm,
        Multiaddr::empty()
            .with(Protocol::from(Ipv4Addr::UNSPECIFIED))
            .with(Protocol::Tcp(tcp_port)),
        Multiaddr::empty()
            .with(Protocol::from(Ipv4Addr::UNSPECIFIED))
            .with(Protocol::Tcp(0)),
    )?;
    try_listen_or_fallback(
        &mut swarm,
        Multiaddr::empty()
            .with(Protocol::from(Ipv6Addr::UNSPECIFIED))
            .with(Protocol::Tcp(tcp_port)),
        Multiaddr::empty()
            .with(Protocol::from(Ipv6Addr::UNSPECIFIED))
            .with(Protocol::Tcp(0)),
    )?;
    #[cfg(not(target_os = "android"))]
    {
        try_listen_or_fallback(
            &mut swarm,
            Multiaddr::empty()
                .with(Protocol::from(Ipv4Addr::UNSPECIFIED))
                .with(Protocol::Tcp(ws_port))
                .with(Protocol::Ws(std::borrow::Cow::Borrowed(""))),
            Multiaddr::empty()
                .with(Protocol::from(Ipv4Addr::UNSPECIFIED))
                .with(Protocol::Tcp(0))
                .with(Protocol::Ws(std::borrow::Cow::Borrowed(""))),
        )?;
        try_listen_or_fallback(
            &mut swarm,
            Multiaddr::empty()
                .with(Protocol::from(Ipv6Addr::UNSPECIFIED))
                .with(Protocol::Tcp(ws_port))
                .with(Protocol::Ws(std::borrow::Cow::Borrowed(""))),
            Multiaddr::empty()
                .with(Protocol::from(Ipv6Addr::UNSPECIFIED))
                .with(Protocol::Tcp(0))
                .with(Protocol::Ws(std::borrow::Cow::Borrowed(""))),
        )?;
    } // relay 节点拨号由 P2pActor 在 AutoNAT 确定 NAT 状态后按需进行

    Ok(swarm)
}

/// 创建 Kademlia 行为
fn create_kademlia(
    peer_id: PeerId,
    data_dir: &Path,
    bootstrap_nodes: &[(String, String)],
    peerid_was_rotated: bool,
) -> P2pResult<kad::Behaviour<MemoryStore>> {
    let mut config = KadConfig::new(PROTOCOL_KAD.deref().to_owned());
    let replication_factor = NonZero::new(KAD_REPLICATION_FACTOR).ok_or_else(|| {
        P2pError::SwarmInitFailed("KAD_REPLICATION_FACTOR must be non-zero".into())
    })?;
    let parallelism = NonZero::new(KAD_PARALLELISM)
        .ok_or_else(|| P2pError::SwarmInitFailed("KAD_PARALLELISM must be non-zero".into()))?;
    let _ = &mut config
        .set_query_timeout(Duration::from_secs(KAD_QUERY_TIMEOUT_SECS))
        .set_replication_factor(replication_factor)
        .set_parallelism(parallelism)
        .set_periodic_bootstrap_interval(Some(Duration::from_secs(KAD_BOOTSTRAP_INTERVAL_SECS)))
        .set_provider_record_ttl(Some(Duration::from_secs(KAD_PROVIDER_TTL_SECS)))
        .set_publication_interval(Some(Duration::from_secs(KAD_PUBLICATION_INTERVAL_SECS)));

    let mut kademlia = kad::Behaviour::with_config(peer_id, MemoryStore::new(peer_id), config);
    kademlia.set_mode(Some(Mode::Client));

    // PeerID 未轮换时加载路由表缓存，加速节点发现
    if !peerid_was_rotated {
        let cache_path = data_dir.join("routing_table.cache");
        let loaded = load_routing_table(&mut kademlia, &cache_path);
        if loaded > 0 {
            tracing::info!("从路由表缓存加载了 {} 个节点", loaded);
        }
    }

    // 始终使用 bootstrap 节点作为基础路由
    if !bootstrap_nodes.is_empty() {
        for (peerid, addr) in bootstrap_nodes {
            let peer_id = PeerId::from_str(peerid).map_err(|e| {
                P2pError::SwarmInitFailed(
                    format!("Failed to parse bootstrap peer ID: {}", e).into(),
                )
            })?;
            let multiaddr = addr.parse::<libp2p::Multiaddr>().map_err(|e| {
                P2pError::SwarmInitFailed(format!("Failed to parse bootstrap addr: {}", e).into())
            })?;
            kademlia.add_address(&peer_id, multiaddr);
        }
    } else {
        tracing::warn!("没有配置 bootstrap 节点，Kademlia 将无法引导");
    }

    if let Err(e) = kademlia.bootstrap() {
        tracing::trace!("Bootstrap error: {}", e);
    }
    Ok(kademlia)
}

// ============================================================================
// 路由表持久化（PEX）
// ============================================================================

const ROUTING_CACHE_MAX_AGE_SECS: u64 = 86400; // 24h
const ROUTING_CACHE_MAX_LOAD: usize = 100;

/// 将当前 Kademlia 路由表中的已知 peers 保存到缓存文件。
///
/// 仅保存 `connected_peers` 集合中的 peer（跳过已断开连接的陈旧条目），
/// 避免持久化过期节点、防止缓存无限累积。
///
/// 文件格式:
/// ```text
/// #ts={unix_timestamp}
/// {base58_peerid} {multiaddr1} {multiaddr2} ...
/// ```
pub async fn save_routing_table(swarm: &mut Swarm<MyBehaviour>, cache_path: &Path, connected_peers: &std::collections::HashMap<PeerId, usize>) {
    let entries = {
        let kademlia = swarm.behaviour_mut();
        let mut peers: Vec<(PeerId, Vec<String>)> = Vec::new();

        for bucket in kademlia.kademlia.kbuckets() {
            for entry in bucket.iter() {
                let peer_id = entry.node.key.preimage();
                if !connected_peers.contains_key(peer_id) {
                    continue;
                }
                let addrs: Vec<String> = entry.node.value.iter().map(|a| a.to_string()).collect();
                if !addrs.is_empty() {
                    peers.push((*peer_id, addrs));
                }
            }
        }
        peers
    };

    if entries.is_empty() {
        tracing::debug!("Routing table is empty, skipping cache save");
        return;
    }

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let mut content_lines = Vec::with_capacity(entries.len() + 1);
    content_lines.push(format!("#ts={}", now));
    for (peer_id, addrs) in &entries {
        content_lines.push(format!("{} {}", peer_id.to_base58(), addrs.join(" ")));
    }

    let content = content_lines.join("\n");

    match tokio::fs::write(cache_path, content.as_bytes()).await {
        Ok(()) => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(cache_path, std::fs::Permissions::from_mode(0o600));
            }
            tracing::info!(
                "Saved {} peers to routing table cache: {:?}",
                entries.len(),
                cache_path
            );
        }
        Err(e) => {
            tracing::warn!(
                "Failed to create routing table cache file {:?}: {}",
                cache_path,
                e
            );
        }
    }
}

/// 从缓存文件加载路由表，验证完整性，过滤过期条目。
///
/// 仅在 PeerID 未轮换时调用，否则缓存内容不可复用。
fn load_routing_table(kademlia: &mut kad::Behaviour<MemoryStore>, cache_path: &Path) -> usize {
    if !cache_path.exists() {
        return 0;
    }

    let content = match std::fs::read_to_string(cache_path) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("Failed to read routing table cache {:?}: {}", cache_path, e);
            return 0;
        }
    };

    let lines: Vec<&str> = content.lines().collect();
    if lines.len() < 2 {
        tracing::warn!("Routing table cache too short ({} lines), ignoring", lines.len());
        return 0;
    }

    // 解析时间戳
    let ts_line = lines[0].trim();
    let cache_ts: u64 = match ts_line.strip_prefix("#ts=").and_then(|s| s.parse().ok()) {
        Some(ts) => ts,
        None => {
            tracing::warn!("Routing table cache missing timestamp header, ignoring");
            return 0;
        }
    };

    // 检查过期
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    if now.saturating_sub(cache_ts) > ROUTING_CACHE_MAX_AGE_SECS {
        tracing::info!("Routing table cache expired ({}s old), ignoring", now.saturating_sub(cache_ts));
        let _ = std::fs::remove_file(cache_path);
        return 0;
    }

    let mut loaded_count = 0;
    for line in &lines[1..] {
        if loaded_count >= ROUTING_CACHE_MAX_LOAD {
            tracing::info!("Reached max load limit ({})", ROUTING_CACHE_MAX_LOAD);
            break;
        }

        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        let mut parts = line.splitn(2, ' ');
        let peer_id_str = match parts.next() {
            Some(s) => s,
            None => continue,
        };
        let addrs_str = match parts.next() {
            Some(s) => s,
            None => continue,
        };

        let peer_id = match PeerId::from_str(peer_id_str) {
            Ok(pid) => pid,
            Err(e) => {
                tracing::trace!("Skipping invalid PeerId in routing table cache: {}", e);
                continue;
            }
        };

        for addr_str in addrs_str.split(' ') {
            let addr_str = addr_str.trim();
            if addr_str.is_empty() {
                continue;
            }
            match addr_str.parse::<libp2p::Multiaddr>() {
                Ok(addr) => {
                    kademlia.add_address(&peer_id, addr);
                }
                Err(e) => {
                    tracing::trace!(
                        "Skipping invalid Multiaddr '{}' in routing table cache: {}",
                        addr_str,
                        e
                    );
                }
            }
        }
        loaded_count += 1;
    }

    if loaded_count > 0 {
        tracing::info!("Loaded {} peers from routing table cache", loaded_count);
    }
    loaded_count
}

/// 尝试用偏好地址监听，失败则回退到备用地址（OS 分配端口）
///
/// 偏好地址中的端口可能被占用，此时静默回退到端口 0 让 OS 分配。
/// 这种"尽力而为"策略确保端口稳定时网络拓扑不变，端口冲突时不影响连接。
fn try_listen_or_fallback(
    swarm: &mut Swarm<MyBehaviour>,
    preferred: Multiaddr,
    fallback: Multiaddr,
) -> P2pResult<()> {
    let preferred_port = preferred
        .iter()
        .find_map(|p| match p {
            Protocol::Tcp(p) | Protocol::Udp(p) => Some(p),
            _ => None,
        })
        .unwrap_or(0);

    if preferred_port > 0 {
        match swarm.listen_on(preferred.clone()) {
            Ok(_listener_id) => {
                tracing::debug!("Listened on preferred port {}", preferred_port);
                return Ok(());
            }
            Err(e) => {
                tracing::warn!(
                    "Preferred port {} unavailable ({}), falling back to OS-assigned",
                    preferred_port,
                    e
                );
            }
        }
    }

    swarm
        .listen_on(fallback)
        .map_err(|e| P2pError::SwarmInitFailed(format!("Failed to listen: {}", e).into()))?;
    Ok(())
}
