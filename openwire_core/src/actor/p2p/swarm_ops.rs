//! Swarm 操作方法
//!
//! 封装对 libp2p Swarm 的常用操作，供 P2pActor 使用。
//! 包括：发送消息、发送 NetEvent、DHT 操作、连接管理等。

use libp2p::{PeerId, Swarm};

use crate::log::truncate_str;
use crate::p2p::behaviour::MyBehaviour;
use crate::p2p::netevent::{NetEventRequest, NetEventResponse};
use crate::{ChatMessage, ChatResponse};

// ============================================================================
// 消息发送
// ============================================================================

/// 通过 rr_msg 协议发送消息
pub fn send_message(swarm: &mut Swarm<MyBehaviour>, peer_id: &PeerId, message: ChatMessage) {
    swarm.behaviour_mut().rr_msg.send_request(peer_id, message);
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

/// 发布身份到 DHT（start_providing + put_record）
pub fn publish_identity_to_dht(
    swarm: &mut Swarm<MyBehaviour>,
    mldsa_pubkey_hex: &str,
    mlkem_pubkey_hex: &str,
) {
    // 1. 使用 Kademlia 原生 provider 机制发布 PeerID
    let key = libp2p::kad::RecordKey::new(&mldsa_pubkey_hex.to_string());
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

    // 2. 发布 ML-KEM 公钥记录
    if !mlkem_pubkey_hex.is_empty() {
        let record_key = format!("mlkem:{}", mldsa_pubkey_hex);
        let record = libp2p::kad::Record {
            key: libp2p::kad::RecordKey::new(&record_key),
            value: mlkem_pubkey_hex.as_bytes().to_vec(),
            publisher: None,
            expires: None,
        };
        match swarm
            .behaviour_mut()
            .kademlia
            .put_record(record, libp2p::kad::Quorum::One)
        {
            Ok(query_id) => {
                tracing::debug!(
                    "Published ML-KEM pubkey for ML-DSA {} (query_id: {:?})",
                    truncate_str(mldsa_pubkey_hex, 16),
                    query_id,
                );
            }
            Err(e) => {
                tracing::warn!(
                    "Failed to publish ML-KEM pubkey for ML-DSA {}: {:?}",
                    truncate_str(mldsa_pubkey_hex, 16),
                    e
                );
            }
        }
    }
}

/// 发起 GetProviders 查询
pub fn get_providers(swarm: &mut Swarm<MyBehaviour>, key: &str) {
    let record_key = libp2p::kad::RecordKey::new(&key);
    let _query_id = swarm.behaviour_mut().kademlia.get_providers(record_key);
}

/// 发起 GetRecord 查询
pub fn get_record(swarm: &mut Swarm<MyBehaviour>, key: &str) {
    let record_key = libp2p::kad::RecordKey::new(&key);
    let _query_id = swarm.behaviour_mut().kademlia.get_record(record_key);
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
