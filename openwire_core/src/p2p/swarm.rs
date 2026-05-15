use libp2p::gossipsub;
use libp2p::kad::{self, Config as KadConfig, Mode};
use libp2p::request_response::{Config as rrconfig, ProtocolSupport, cbor, cbor::codec::Codec};
use libp2p::{PeerId, StreamProtocol, Swarm, dcutr, identify, mdns, ping, relay};
use sha2::Digest;

use redb::Database;
use std::num::NonZero;
use std::ops::Deref;
use std::path::Path;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use super::behaviour::MyBehaviour;
use super::bootstrap;
use super::dht;
use crate::error::{P2pError, P2pResult};
use crate::{ChatMessage, ChatResponse};

use std::sync::LazyLock;

/// Kademlia 协议标识
static PROTOCOL_KAD: LazyLock<StreamProtocol> =
    LazyLock::new(|| StreamProtocol::new("/chat/kad/0.0.1"));
/// 消息请求-响应协议标识
static PROTOCOL_MSGRR: LazyLock<StreamProtocol> =
    LazyLock::new(|| StreamProtocol::new("/chat/rr_msg/0.0.1"));

// ============================================================================
// DDoS 防护配置
// ============================================================================

/// 请求消息最大大小（非文件消息，256KB）
///
/// 普通聊天消息远小于此值，256KB 足以容纳加密后的消息+元数据。
/// 文件传输使用专门的 FileStreamChunk 协议，不经过 request-response。
const REQUEST_SIZE_MAX: usize = 256 * 1024;
/// 响应消息最大大小（256KB）
const RESPONSE_SIZE_MAX: usize = 256 * 1024;

/// Identify 协议缓存大小
const IDENTIFY_CACHE_SIZE: usize = 100;

/// 空闲连接超时（秒）
///
/// 连接空闲（无活跃流、无待处理流、所有 handler 返回 keep_alive=false）后，
/// 等待此时间后关闭连接。防止僵尸连接占用资源。
const IDLE_CONNECTION_TIMEOUT_SECS: u64 = 30;

/// 每个连接最大并发协商入站流数
///
/// 防止攻击者通过大量并发入站流协商请求耗尽连接资源。
const MAX_NEGOTIATING_INBOUND_STREAMS: usize = 5;

/// request-response 协议最大并发流数
///
/// 限制每个连接上同时处理的请求-响应流数量，防止单个连接洪泛。
const RR_MAX_CONCURRENT_STREAMS: usize = 10;

/// 并发拨号因子
///
/// 单个出站连接尝试中并发拨号的地址数。降低此值可减少出站连接洪泛的影响。
const DIAL_CONCURRENCY_FACTOR: u8 = 3;

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
/// Provider 记录 TTL（秒）- 24小时
const KAD_PROVIDER_TTL_SECS: u64 = 24 * 60 * 60;
/// Kademlia 发布间隔（秒）- 1小时
const KAD_PUBLICATION_INTERVAL_SECS: u64 = 60 * 60;

/// 路由表缓存文件名
const ROUTING_TABLE_CACHE_FILE: &str = "routing_table.cache";

/// 初始化 libp2p Swarm
///
/// # 网络栈配置
/// - 补充：QUIC（原生 TLS 1.3，性能更优）
/// - 发现：mDNS 局域网自动发现
/// - 消息：rrmsg
///
/// `dht_db` 是共享的 DHT 数据库连接（Arc<Database>），由调用方统一管理，
/// 避免多个组件各自打开数据库导致文件锁冲突。
pub fn swarm_init(
    data_dir: &Path,
    keypair: libp2p::identity::Keypair,
    dht_db: Arc<Database>,
) -> P2pResult<Swarm<MyBehaviour>> {
    let mut swarm = libp2p::SwarmBuilder::with_existing_identity(keypair)
        .with_tokio()
        // QUIC 传输层：内置 TLS 1.3，0-RTT，更好 NAT 穿透
        .with_quic()
        // DNS 解析（支持 /dns4, /dns6, /dnsaddr）
        .with_dns()
        .map_err(|e| P2pError::SwarmInitFailed(e.into()))?
        .with_behaviour(|key| {
            let peer_id = key.public().to_peer_id();

            let codec = Codec::<ChatMessage, ChatResponse>::default()
                .set_request_size_maximum(REQUEST_SIZE_MAX as u64)
                .set_response_size_maximum(RESPONSE_SIZE_MAX as u64);

            // 配置 request-response 协议，限制并发流数防止洪泛攻击
            let rr_config = rrconfig::default()
                .with_request_timeout(Duration::from_secs(30))
                .with_max_concurrent_streams(RR_MAX_CONCURRENT_STREAMS);

            let rr_msg = cbor::Behaviour::with_codec(
                codec,
                [(PROTOCOL_MSGRR.deref().to_owned(), ProtocolSupport::Full)],
                rr_config,
            );

            // --- mDNS 配置 ---
            let mdns =
                mdns::tokio::Behaviour::new(mdns::Config::default(), key.public().to_peer_id())
                    .map_err(|e| P2pError::SwarmInitFailed(e.into()))?;

            // 创建 Kademlia 行为实例（传入共享的 DHT 数据库连接）
            let kademlia = create_kademlia(peer_id, dht_db.clone(), data_dir)?;
            let identify_config =
                identify::Config::new("/rootcell/identify/1.0.0".to_string(), key.public())
                    .with_agent_version(format!("rootcell/{}", env!("CARGO_PKG_VERSION")))
                    .with_cache_size(IDENTIFY_CACHE_SIZE);
            let identify = identify::Behaviour::new(identify_config);
            let ping = ping::Behaviour::new(ping::Config::new());
            let relay = relay::Behaviour::new(key.public().to_peer_id(), Default::default());
            let dcutr = dcutr::Behaviour::new(key.public().to_peer_id());

            // --- Gossipsub 配置 ---
            // 消息 ID 函数：使用 SHA256 哈希作为消息 ID，避免重复消息
            let message_id_fn = |message: &gossipsub::Message| {
                let mut hasher = sha2::Sha256::new();
                hasher.update(&message.data);
                hasher.update(&message.topic.to_string().into_bytes());
                gossipsub::MessageId::from(hasher.finalize().to_vec())
            };

            // 使用默认配置，但自定义消息 ID 函数
            let gossipsub_config = gossipsub::ConfigBuilder::default()
                .message_id_fn(message_id_fn)
                // 心跳间隔：1 秒，快速传播
                .heartbeat_interval(Duration::from_secs(1))
                // 支持的消息长度上限：256KB
                .max_transmit_size(256 * 1024)
                .build()
                .map_err(|e| P2pError::SwarmInitFailed(e.into()))?;

            let gossipsub = gossipsub::Behaviour::new(
                gossipsub::MessageAuthenticity::Signed(key.clone()),
                gossipsub_config,
            )
            .map_err(|e| P2pError::SwarmInitFailed(e.into()))?;

            Ok(MyBehaviour {
                rr_msg,
                mdns,
                kademlia,
                ping,
                identify,
                relay,
                dcutr,
                gossipsub,
            })
        })
        .map_err(|e| P2pError::SwarmInitFailed(e.into()))?
        // Swarm 级 DDoS 防护配置
        .with_swarm_config(|cfg| {
            cfg.with_idle_connection_timeout(Duration::from_secs(IDLE_CONNECTION_TIMEOUT_SECS))
                .with_max_negotiating_inbound_streams(MAX_NEGOTIATING_INBOUND_STREAMS)
                .with_dial_concurrency_factor(
                    NonZero::new(DIAL_CONCURRENCY_FACTOR).expect("DIAL_CONCURRENCY_FACTOR > 0"),
                )
        })
        .build();

    // 端口 0 表示系统自动分配
    swarm
        .listen_on("/ip4/0.0.0.0/udp/0/quic-v1".parse().map_err(|e| {
            P2pError::SwarmInitFailed(format!("Failed to parse listen addr: {}", e).into())
        })?)
        .map_err(|e| P2pError::SwarmInitFailed(format!("Failed to listen: {}", e).into()))?;
    swarm
        .listen_on("/ip6/::/udp/0/quic-v1".parse().map_err(|e| {
            P2pError::SwarmInitFailed(format!("Failed to parse listen addr: {}", e).into())
        })?)
        .map_err(|e| P2pError::SwarmInitFailed(format!("Failed to listen: {}", e).into()))?;

    Ok(swarm)
}

/// 创建 Kademlia 行为
///
/// 使用调用方传入的共享 `Arc<Database>`，避免重复打开导致文件锁冲突。
fn create_kademlia(
    peer_id: PeerId,
    db: Arc<Database>,
    data_dir: &Path,
) -> P2pResult<kad::Behaviour<dht::RedbRecordStore>> {
    let store = dht::RedbRecordStore::new(db);

    // 配置Kademlia网络参数
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

    let mut kademlia = kad::Behaviour::with_config(peer_id, store, config);
    kademlia.set_mode(Some(Mode::Server));

    // 先加载缓存的路由表（如果存在），减少对 bootstrap 节点的单点依赖
    let cache_path = data_dir.join(ROUTING_TABLE_CACHE_FILE);
    let cached_count = load_routing_table(&mut kademlia, &cache_path);
    if cached_count > 0 {
        tracing::info!(
            "Loaded {} cached peers from routing table cache, skipping bootstrap",
            cached_count
        );
    } else {
        // 无缓存时使用硬编码的 bootstrap 节点
        for (peerid, addr) in bootstrap::BOOTSTRAP {
            let peer_id = PeerId::from_str(peerid).map_err(|e| {
                P2pError::SwarmInitFailed(
                    format!("Failed to parse bootstrap peer ID: {}", e).into(),
                )
            })?;
            let multiaddr = bootstrap::resolve_dnsaddr(addr).map_err(|e| {
                P2pError::SwarmInitFailed(format!("Failed to resolve dnsaddr: {}", e).into())
            })?;
            kademlia.add_address(&peer_id, multiaddr);
        }
    }

    if let Err(e) = kademlia.bootstrap() {
        tracing::trace!("Bootstrap error: {}", e);
    }
    Ok(kademlia)
}

// ============================================================================
// 路由表持久化（PEX）
// ============================================================================

/// 将当前 Kademlia 路由表中的已知 peers 保存到缓存文件。
///
/// 序列化格式：每行一个 peer，格式为 `PeerIdBase58 multiaddr1 multiaddr2 ...`
/// 使用 Base58 编码 PeerId，Multiaddr 使用其标准字符串表示。
///
/// 此函数在以下时机调用：
/// 1. `ChatCore::drop()` — 正常退出时保存
/// 2. 主事件循环中定期调用 — 确保运行期间缓存持续更新
///
/// 缓存文件保留在磁盘上，即使程序崩溃也不会丢失上次保存的路由表。
pub fn save_routing_table(swarm: &mut Swarm<MyBehaviour>, cache_path: &Path) {
    let entries = {
        let kademlia = swarm.behaviour_mut();
        let mut peers: Vec<(PeerId, Vec<String>)> = Vec::new();

        for bucket in kademlia.kademlia.kbuckets() {
            for entry in bucket.iter() {
                let peer_id = entry.node.key.preimage();
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

    match std::fs::File::create(cache_path) {
        Ok(file) => {
            use std::io::Write;
            let mut writer = std::io::BufWriter::new(file);
            for (peer_id, addrs) in &entries {
                let line = format!("{} {}\n", peer_id.to_base58(), addrs.join(" "));
                if let Err(e) = writer.write_all(line.as_bytes()) {
                    tracing::warn!("Failed to write routing table cache entry: {}", e);
                    return;
                }
            }
            if let Err(e) = writer.flush() {
                tracing::warn!("Failed to flush routing table cache: {}", e);
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

/// 从缓存文件加载路由表，将缓存的 peers 添加到 Kademlia 路由表中。
///
/// 返回成功加载的 peer 数量。
/// 如果缓存文件不存在或格式错误，返回 0。
fn load_routing_table(
    kademlia: &mut kad::Behaviour<dht::RedbRecordStore>,
    cache_path: &Path,
) -> usize {
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

    let mut loaded_count = 0;
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        // 格式: "PeerIdBase58 multiaddr1 multiaddr2 ..."
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

    loaded_count
}
