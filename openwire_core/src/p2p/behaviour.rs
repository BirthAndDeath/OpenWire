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
    ///pub kademlia: kad::Behaviour<super::dht::RedbRecordStore>, 对于客户端传播储存太慢,改为使用内存
    pub kademlia: kad::Behaviour<kad::store::MemoryStore>,
    /// Ping 协议（连接保活/延迟检测）
    pub ping: ping::Behaviour,

    /// AutoNAT 协议（检测自身 NAT 类型，决定是否启用中继服务）
    pub autonat: autonat::Behaviour,
    /// Identify 协议（地址/协议交换）
    pub identify: identify::Behaviour,
    /// Relay 协议（NAT 穿透，Client 模式）
    pub relay_client: relay::client::Behaviour,
    /// Relay Server 模式（为他人提供中继，公网节点自动启用）
    pub relay_server: relay::Behaviour,
    /// DCUtR 协议（直连升级，配合 Relay）
    pub dcutr: dcutr::Behaviour,

    /// 连接数限制
    pub limits: connection_limits::Behaviour,

    /// 内存连接数限制
    pub memmory_limits: memory_connection_limits::Behaviour, // UPnP 协议（端口映射，可选）有一个神秘宏bug，我不知道为什么后来人可以试试修复
                                                             //pub upnp: upnp::Behaviour,
}
