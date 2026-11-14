//! Swarm 操作方法
//!
//! 封装对 libp2p Swarm 的常用操作，供 P2pActor 使用。
//! 包括：发送消息、发送 NetEvent、DHT 操作、连接管理等。

use libp2p::request_response::OutboundRequestId;
use libp2p::{PeerId, Swarm};

use crate::log::truncate_str;
use crate::p2p::behaviour::MyBehaviour;
use crate::p2p::netevent::{NetEventRequest, NetEventResponse};
use crate::{ChatMessage, ChatResponse};

// ============================================================================
// 消息发送
// ============================================================================

/// 通过 rr_msg 协议发送消息，返回 OutboundRequestId 供追踪结果
pub fn send_message(
    swarm: &mut Swarm<MyBehaviour>,
    peer_id: &PeerId,
    message: ChatMessage,
) -> OutboundRequestId {
    swarm.behaviour_mut().rr_msg.send_request(peer_id, message)
}

/// 通过 rr_netevent 协议发送 NetEvent 请求
pub fn send_netevent_request(
    swarm: &mut Swarm<MyBehaviour>,
    peer_id: &PeerId,
    request: NetEventRequest,
) {
    swarm
        .behaviour_mut()
        .rr_netevent
        .send_request(peer_id, request);
}

/// 发送 NetEvent 响应
pub fn send_netevent_response(
    swarm: &mut Swarm<MyBehaviour>,
    channel: libp2p::request_response::ResponseChannel<NetEventResponse>,
    response: NetEventResponse,
) {
    if let Err(e) = swarm
        .behaviour_mut()
        .rr_netevent
        .send_response(channel, response)
    {
        tracing::error!("发送 NetEvent 响应失败: {:?}", e);
    }
}

/// 发送 rr_msg 响应确认
pub fn send_response(
    swarm: &mut Swarm<MyBehaviour>,
    channel: libp2p::request_response::ResponseChannel<ChatResponse>,
    response: ChatResponse,
) {
    if let Err(e) = swarm
        .behaviour_mut()
        .rr_msg
        .send_response(channel, response)
    {
        tracing::error!("发送响应失败: {:?}", e);
    }
}

// ============================================================================
// DHT 操作
// ============================================================================

/// 将 ML-DSA 公钥 hex 哈希为 DHT 查询键，隐藏原始公钥
pub fn dht_key(mldsa_pubkey_hex: &str) -> String {
    use sha2::Digest;
    hex::encode(sha2::Sha256::digest(mldsa_pubkey_hex.as_bytes()))
}

/// 发布身份到 DHT（ML-KEM 不再存入 DHT）
///
/// 使用 SHA256(ML-DSA 公钥) 作为 provider key，隐藏原始公钥。
pub fn publish_identity_to_dht(swarm: &mut Swarm<MyBehaviour>, mldsa_pubkey_hex: &str) {
    let key = libp2p::kad::RecordKey::new(&dht_key(mldsa_pubkey_hex));
    match swarm.behaviour_mut().kademlia.start_providing(key) {
        Ok(query_id) => {
            tracing::debug!(
                "Started providing PeerID for ML-DSA {} (query_id: {:?})",
                truncate_str(mldsa_pubkey_hex, 16),
                query_id,
            );
        }
        Err(e) => {
            tracing::warn!(
                "Failed to start providing PeerID for ML-DSA {}: {:?}",
                truncate_str(mldsa_pubkey_hex, 16),
                e
            );
        }
    }
}

/// 发起 GetProviders 查询（key 由调用方用 dht_key 哈希），返回查询 ID 供许可回收
pub fn get_providers(swarm: &mut Swarm<MyBehaviour>, key: &str) -> libp2p::kad::QueryId {
    let record_key = libp2p::kad::RecordKey::new(&key);
    swarm.behaviour_mut().kademlia.get_providers(record_key)
}

/// 停止在 DHT 提供身份（删除 identity 时调用，撤销 DHT 上的提供记录）
pub fn stop_providing_to_dht(swarm: &mut Swarm<MyBehaviour>, mldsa_pubkey_hex: &str) {
    let key = libp2p::kad::RecordKey::new(&dht_key(mldsa_pubkey_hex));
    swarm.behaviour_mut().kademlia.stop_providing(&key);
    tracing::debug!(
        "Stopped providing PeerID for ML-DSA {}",
        truncate_str(mldsa_pubkey_hex, 16),
    );
}

/// 判断 Multiaddr 是否为公网可达地址（非环回/私有/链路本地/ULA/组播/未指定）。
///
/// 等价于 Go libp2p 的 `Addresses.NoAnnounce`：任何向外部宣告/写入推荐信息的
/// 地址（身份记录、二维码 addr、节点导出）都应先过此过滤，避免内网网段泄漏。
pub fn is_public_internet_addr(maddr: &libp2p::Multiaddr) -> bool {
    use libp2p::multiaddr::Protocol;
    let ip = maddr.iter().find_map(|p| match p {
        Protocol::Ip4(a) => Some(std::net::IpAddr::V4(a)),
        Protocol::Ip6(a) => Some(std::net::IpAddr::V6(a)),
        _ => None,
    });
    match ip {
        Some(std::net::IpAddr::V4(v4)) => {
            !v4.is_loopback()
                && !v4.is_private()
                && !v4.is_link_local()
                && !v4.is_multicast()
                && !v4.is_unspecified()
                && !v4.is_broadcast()
        }
        Some(std::net::IpAddr::V6(v6)) => {
            let first: u16 = v6.segments()[0];
            let is_ula_or_lla =
                // fc00::/7  ULA
                (first & 0xfe00) == 0xfc00
                // fe80::/10 链路本地
                || (first & 0xffc0) == 0xfe80;
            !is_ula_or_lla
                && !v6.is_loopback()
                && !v6.is_multicast()
                && !v6.is_unspecified()
        }
        // 无 IP 组件（/dns 或纯 relay 地址）不在此处过滤
        _ => true,
    }
}

/// 判断 Multiaddr 是否可作为二维码推荐直连地址。
///
/// 与 `is_public_internet_addr` 的区别：**保留局域网/私有地址**（192.168/16、
/// 10/8、172.16/12、IPv6 ULA），使扫码方在同一局域网无服务器环境也能直连；
/// 仅排除环回、链路本地、组播、未指定、广播以及 circuit 地址（非端点直连地址）。
pub fn is_qr_recommend_addr(maddr: &libp2p::Multiaddr) -> bool {
    use libp2p::multiaddr::Protocol;
    // /p2p-circuit 是经中继的电路地址，非端点直接可拨地址，不推荐进二维码
    if maddr.iter().any(|p| matches!(p, Protocol::P2pCircuit)) {
        return false;
    }
    let ip = maddr.iter().find_map(|p| match p {
        Protocol::Ip4(a) => Some(std::net::IpAddr::V4(a)),
        Protocol::Ip6(a) => Some(std::net::IpAddr::V6(a)),
        _ => None,
    });
    match ip {
        Some(std::net::IpAddr::V4(v4)) => {
            !v4.is_loopback()
                && !v4.is_link_local()
                && !v4.is_multicast()
                && !v4.is_unspecified()
                && !v4.is_broadcast()
        }
        Some(std::net::IpAddr::V6(v6)) => {
            let first: u16 = v6.segments()[0];
            // fe80::/10 链路本地排除；fc00::/7 ULA（局域网地址）保留
            let is_lla = (first & 0xffc0) == 0xfe80;
            !is_lla
                && !v6.is_loopback()
                && !v6.is_multicast()
                && !v6.is_unspecified()
        }
        // 无 IP 组件（/dns 或纯 relay 地址）不推荐（无法作为端点直连地址）
        _ => false,
    }
}

/// 随机刷新路由表：选取一个随机桶范围，发起 get_closest_peers 查询
///
/// 通过随机生成 PeerID 发起 Kademlia 查询，扩展路由表覆盖范围。
/// 每 30 分钟触发一次，查询结果自动填充路由表。
pub fn refresh_routing_table(swarm: &mut Swarm<MyBehaviour>) {
    let random_kp = libp2p::identity::Keypair::generate_ed25519();
    let random_peer_id = random_kp.public().to_peer_id();
    swarm
        .behaviour_mut()
        .kademlia
        .get_closest_peers(random_peer_id);
    tracing::debug!(
        "Routing table refresh: querying random peer {}",
        random_peer_id
    );
}

/// 添加地址到 Kademlia 路由表
pub fn add_kademlia_address(
    swarm: &mut Swarm<MyBehaviour>,
    peer_id: &PeerId,
    addr: libp2p::Multiaddr,
) {
    swarm.behaviour_mut().kademlia.add_address(peer_id, addr);
}

// ============================================================================
// 连接管理
// ============================================================================

/// 拨号连接
pub fn dial(swarm: &mut Swarm<MyBehaviour>, peer_id: &PeerId) {
    match swarm.dial(*peer_id) {
        Ok(()) => tracing::debug!("正在拨号: {}", peer_id),
        Err(e) => tracing::debug!("拨号 {} 失败: {}（可能已连接或正在连接）", peer_id, e),
    }
}

/// 拨号到指定地址
pub fn dial_addr(swarm: &mut Swarm<MyBehaviour>, addr: libp2p::Multiaddr) {
    match swarm.dial(addr.clone()) {
        Ok(()) => tracing::debug!("正在拨号地址: {}", addr),
        Err(e) => tracing::debug!("拨号地址 {} 失败: {}", addr, e),
    }
}
