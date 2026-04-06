use libp2p::kad::{self};
use libp2p::request_response::cbor;
use libp2p::{dcutr, identify, mdns, ping, relay, swarm::NetworkBehaviour};

use crate::{ChatMessage, ChatResponse};

/// libp2p 网络行为组合：rr+ mDNS（局域网发现）

#[derive(NetworkBehaviour)]
pub struct MyBehaviour {
    pub rr_msg: cbor::Behaviour<ChatMessage, ChatResponse>,

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
    //rendezvous: rendezvous::client::Behaviour,考虑添加服务申明
}
