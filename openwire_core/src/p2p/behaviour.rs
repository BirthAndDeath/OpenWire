use libp2p::kad::{self};
use libp2p::request_response::cbor;
use libp2p::{dcutr, identify, mdns, ping, relay, swarm::NetworkBehaviour};

use crate::{ChatMessage, ChatResponse};
use crate::p2p::netevent::{NetEventRequest, NetEventResponse};

/// libp2p 网络行为组合
///
/// # 协议说明
/// - `rr_msg`: 消息传输 request-response（聊天消息）
/// - `rr_netevent`: 网络事件通知 request-response（好友上线等）
/// - `mdns`: 局域网自动发现
/// - `kademlia`: DHT 分布式哈希表
/// - `ping`: 连接保活/延迟检测
/// - `identify`: 地址/协议交换
/// - `relay`: NAT 穿透中继
/// - `dcutr`: 直连升级

#[derive(NetworkBehaviour)]
pub struct MyBehaviour {
    /// 消息传输 request-response
    pub rr_msg: cbor::Behaviour<ChatMessage, ChatResponse>,

    /// 网络事件通知 request-response（好友上线等）
    pub rr_netevent: cbor::Behaviour<NetEventRequest, NetEventResponse>,

    /// mDNS 协议：局域网内自动发现对等节点
    pub mdns: mdns::tokio::Behaviour,
    /// Kademlia 协议：分布式哈希表，用于节点定位和路由
    pub kademlia: kad::Behaviour<super::dht::RedbRecordStore>,
    //Ping 协议（连接保活/延迟检测）
    pub ping: ping::Behaviour,
    // Identify 协议（地址/协议交换）
    pub identify: identify::Behaviour,
    // Relay 协议（NAT 穿透，可选）
    pub relay: relay::Behaviour,
    // DCUtR 协议（直连升级，配合 Relay）
    pub dcutr: dcutr::Behaviour,
}
