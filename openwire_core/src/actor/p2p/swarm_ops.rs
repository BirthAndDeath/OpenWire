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

/// 发起 GetProviders 查询（key 由调用方用 dht_key 哈希）
pub fn get_providers(swarm: &mut Swarm<MyBehaviour>, key: &str) {
    let record_key = libp2p::kad::RecordKey::new(&key);
    let _query_id = swarm.behaviour_mut().kademlia.get_providers(record_key);
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
