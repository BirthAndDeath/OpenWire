#![allow(missing_docs)]
use crate::p2p::netevent::{NetEventRequest, NetEventResponse};
use crate::{ChatMessage, ChatResponse};
use libp2p::kad::{self};
use libp2p::request_response::cbor;

use libp2p::{
    autonat, connection_limits, dcutr, identify, mdns, memory_connection_limits, ping, relay,
    swarm::NetworkBehaviour,
};

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
/// - `pathranker`: 路径评分协议（行为感知路由）
/// libp2p 网络行为组合（由 NetworkBehaviour derive 生成事件枚举）
#[derive(NetworkBehaviour)]
#[allow(missing_docs)]
pub struct MyBehaviour {
    /// 消息传输 request-response
    pub rr_msg: cbor::Behaviour<ChatMessage, ChatResponse>,

    /// 网络事件通知 request-response（好友上线等）
    pub rr_netevent: cbor::Behaviour<NetEventRequest, NetEventResponse>,

    /// mDNS 协议：局域网内自动发现对等节点
    pub mdns: mdns::tokio::Behaviour,
    /// Kademlia 协议：分布式哈希表，用于节点定位和路由
    pub kademlia: kad::Behaviour<kad::store::MemoryStore>,
    /// Ping 协议（连接保活/延迟检测）
    pub ping: ping::Behaviour,

    /// AutoNAT 协议（检测自身 NAT 类型，决定是否启用中继服务）
    pub autonat: autonat::Behaviour,
    /// Identify 协议（地址/协议交换）
    pub identify: identify::Behaviour,
    /// Relay 协议（NAT 穿透，Client 模式）
    pub relay_client: relay::client::Behaviour,
    /// DCUtR 协议（直连升级，配合 Relay）
    pub dcutr: dcutr::Behaviour,

    /// 连接数限制
    pub limits: connection_limits::Behaviour,
    /// 内存连接数限制
    pub memory_limits: memory_connection_limits::Behaviour,

    /// 路径评分协议（行为感知路由，EWMA 评分 + 信誉 + 协作评分）
    #[cfg(feature = "pathranker")]
    pub pathranker: libp2p_pathranker::PathRankerBehaviour,
}
