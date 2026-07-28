//! NetEvent 协议：用于 P2P 网络事件通知的 request-response 协议
//!
//! 这是一个独立的 request-response 协议，与 `rr_msg`（消息传输）分开。
//! 用于好友上线通知等网络事件，方便扩展。
//!
//! # 设计原则
//! - 使用 `#[non_exhaustive]` 枚举，方便后续扩展新事件类型
//! - 每个事件类型独立处理，互不影响
//! - 响应简单（Ack），不包含复杂数据

use serde::{Deserialize, Serialize};

/// 网络事件请求
///
/// 通过 request-response 协议发送给对等节点。
/// `#[non_exhaustive]` 确保添加新变体时不会破坏现有代码。
#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum NetEventRequest {
    /// 好友上线通知
    ///
    /// 当连接建立时，主动通知对方自己的身份信息，
    /// 对方收到后可通过 DHT 查询获取最新的公钥→PeerID 映射和 ML-KEM 密钥。
    FriendOnline {
        /// 发送者的 ML-DSA 公钥 hex
        mldsa_pubkey_hex: String,
        /// 发送者的当前 PeerID（Base58 编码）
        peer_id: String,
        /// 发送者的监听地址列表
        listen_addrs: Vec<String>,
        /// 发送者的当前 ML-KEM 公钥 hex（随通知直接传递，无需 DHT 查询）
        mlkem_pubkey_hex: String,
    },
    /// 通过中继节点发现对端：向中继查询某公钥对应的 PeerID
    DiscoverPeer {
        /// 要查询的 ML-DSA 公钥 hex
        mldsa_pubkey_hex: String,
    },
}

/// 网络事件响应
///
/// 对 NetEventRequest 的确认响应。
/// `#[non_exhaustive]` 确保添加新变体时不会破坏现有代码。
#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum NetEventResponse {
    /// 确认收到
    Ack,
    /// 中继查询结果：返回对端的 ML-DSA 公钥、PeerID 和 ML-KEM 公钥
    PeerInfo {
        /// 对端的 ML-DSA 公钥 hex
        mldsa_pubkey_hex: String,
        /// 对端的 PeerID（Base58 编码）
        peer_id: String,
        /// 对端的 ML-KEM 公钥 hex
        mlkem_pubkey_hex: String,
    },
}