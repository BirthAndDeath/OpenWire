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
use sha2::{Digest, Sha256};

/// NetEvent 协议版本号。
///
/// 用于 `FriendOnline` 身份交换的兼容性协商。升级协议时递增，
/// 接收端据此判断能否正确解析并处理消息。
pub(crate) const NETEVENT_VERSION: u8 = 1;

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
        /// 协议版本号（当前为 1，用于未来兼容性检查）。
        /// `None` 表示来自旧客户端，不检查版本兼容性。
        #[serde(default)]
        version: Option<u8>,
        /// 发送者的 ML-DSA 公钥 hex
        mldsa_pubkey_hex: String,
        /// 发送者的当前 PeerID（Base58 编码）
        peer_id: String,
        /// 发送者的监听地址
        listen_addrs: Vec<String>,
        /// 发送者的当前 ML-KEM 公钥 hex（随通知直接传递，无需 DHT 查询）
        mlkem_pubkey_hex: String,
        /// 发送者对身份声明（FriendOnline 负载）的 ML-DSA 签名。
        /// `None` 表示来自旧客户端，跳过签名验证（降级信任）。
        /// 用于在接收端验证通告者确实持有 mldsa_pubkey_hex 对应的私钥，
        /// 防止攻击者冒充他人公钥污染 pubkey→PeerID / ML-KEM 缓存。
        #[serde(default)]
        signature: Option<Vec<u8>>,
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
    /// 确认收到（成功处理）
    Ack,
    /// 拒绝请求，附带结构化原因供发送方诊断、统计和自动处理。
    ///
    /// 仅在发送方自身构造错误时返回（PeerID 不匹配、版本不兼容、签名验证失败），
    /// 不用于区分"非联系人"（统一返回 `Ack` 以避免泄露联系人列表隐私）。
    Nack {
        /// 拒绝原因（结构化，便于发送方程序化处理）
        reason: NackReason,
    },
    /// 中继查询结果：返回对端的 ML-DSA 公钥、PeerID 和 ML-KEM 公钥
    PeerInfo {
        /// 对端的 ML-DSA 公钥 hex
        mldsa_pubkey_hex: String,
        /// 对端的 PeerID（Base58 编码）
        peer_id: String,
        /// 对端的 ML-KEM 公钥 hex
        mlkem_pubkey_hex: String,
    },
    /// 中继查询结果：对方未将此公钥注册到中继（对方尚未添加本节点为联系人）
    PeerNotFound {
        /// 查询的 ML-DSA 公钥 hex
        mldsa_pubkey_hex: String,
    },
}

/// FriendOnline/Netevent 拒绝原因。
///
/// 结构化枚举而非裸字符串，便于发送方按类型分类处理（重试、升级、断开等）。
/// 所有变体均不泄露接收方内部状态或联系人列表信息。
#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum NackReason {
    /// 声称的 PeerID 与实际连接的 PeerID 不一致
    PeerIdMismatch,
    /// NetEvent 协议版本不兼容
    VersionMismatch {
        /// 期望的协议版本
        expected: u8,
        /// 发送方使用的协议版本
        got: u8,
    },
    /// 身份声明签名验证失败（发送方可能未持有对应私钥）
    SignatureVerificationFailed,
    /// 其他拒绝原因（带人类可读描述，作为兜底）
    Other {
        /// 人类可读的错误描述
        description: String,
    },
}

/// 生成 FriendOnline 身份声明的规范化负载哈希。
///
/// 用于签名和验证，确保内容在发送方和接收方之间一致。
/// listen_addrs 在对等排序后编码，保证省略顺序差异不影响签名。
pub(crate) fn friend_online_payload(
    mldsa_pubkey_hex: &str,
    peer_id: &str,
    listen_addrs: &[String],
    mlkem_pubkey_hex: &str,
) -> Vec<u8> {
    let mut hasher = Sha256::new();
    hasher.update(b"OpenWire-FriendOnline-v1");
    hasher.update(mldsa_pubkey_hex.as_bytes());
    hasher.update(peer_id.as_bytes());
    hasher.update(mlkem_pubkey_hex.as_bytes());
    let mut sorted: Vec<&str> = listen_addrs.iter().map(String::as_str).collect();
    sorted.sort();
    for addr in &sorted {
        hasher.update(addr.as_bytes());
        hasher.update([0u8]);
    }
    hasher.finalize().to_vec()
}

/// 验证 FriendOnline 请求的 ML-DSA 签名。
///
/// 返回 `true` 表示签名有效、身份声明可信。
pub fn verify_friend_online_signature(
    mldsa_pubkey_hex: &str,
    peer_id: &str,
    listen_addrs: &[String],
    mlkem_pubkey_hex: &str,
    signature: &[u8],
) -> bool {
    let Ok(pubkey_bytes) = hex::decode(mldsa_pubkey_hex) else {
        return false;
    };
    if signature.is_empty() {
        return false;
    }
    let payload = friend_online_payload(mldsa_pubkey_hex, peer_id, listen_addrs, mlkem_pubkey_hex);
    crate::signature::verify_signature(&pubkey_bytes, &payload, signature).unwrap_or(false)
}