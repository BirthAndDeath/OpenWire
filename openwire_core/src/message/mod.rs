use crate::crypto::constant_time_compare;
use rand::{RngExt, rng};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// 文件流子模块
pub mod file_stream;
pub use file_stream::{
    ChunkReadConfig, DownloadRequest, DownloadResponse, FileHashInfo, FileStreamChunk,
};

/// 消息最大允许年龄（秒）
const MESSAGE_MAX_AGE_SECS: u64 = 60 * 60;
/// 消息未来时间容差（秒）
const MESSAGE_FUTURE_TOLERANCE_SECS: u64 = 60;

/// 聊天消息类型（消息种类标识）
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[repr(u8)]
#[non_exhaustive]
pub enum ChatMessageType {
    /// 未知类型（仅用于数据库哨兵，表示未初始化，不参与协议）
    Unknown = 255,
    /// 文本消息
    Text = 0,
    /// 文件哈希分享
    FileHash = 1,
    /// 文件流分片
    FileStream = 2,
    /// 文件下载请求（接收方发起，请求发送方开始传输文件）
    FileDownloadRequest = 3,
    /// 消息送达回执
    DeliveryReceipt = 4,
    /// 在线状态通知（通过 gossipsub 发布/订阅）
    OnlineStatus = 5,
    /// 文件下载响应（发送方同意/拒绝）
    FileDownloadResponse = 6,
}

impl TryFrom<i32> for ChatMessageType {
    type Error = ();

    fn try_from(value: i32) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Text),
            1 => Ok(Self::FileHash),
            2 => Ok(Self::FileStream),
            3 => Ok(Self::FileDownloadRequest),
            4 => Ok(Self::DeliveryReceipt),
            5 => Ok(Self::OnlineStatus),
            6 => Ok(Self::FileDownloadResponse),
            255 => Ok(Self::Unknown),
            _ => Err(()),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
/// 聊天消息结构，包含签名、时间戳和完整性校验
pub struct ChatMessage {
    /// 消息类型
    pub msgtype: ChatMessageType,
    /// 消息发送时间戳 (ms since UNIX_EPOCH)
    pub timestamp: u64,
    /// 随机 nonce，用于防止同一时间重复放大消息泛洪
    pub nonce: [u8; 16],
    /// 消息内容
    pub data: Vec<u8>,
    /// 数据哈希，用于完整性校验
    pub hash: Vec<u8>,
    /// 发送者 ML-DSA 签名
    pub signature: Vec<u8>,
    /// 发送者 ML-DSA 公钥（原始格式，1952 字节）
    pub sender_public_key: Vec<u8>,
}

/// 消息响应（签名确认）
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChatResponse {
    /// 响应时间戳
    pub timestamp: u64,
    /// 随机 nonce，用于防止重放攻击
    pub nonce: [u8; 16],
    /// 响应方的 ML-DSA 签名，签名内容为 (timestamp, nonce, request_hash)
    pub signature: Vec<u8>,
    /// 响应方的 ML-DSA 公钥（原始格式，1952 字节）
    pub sender_public_key: Vec<u8>,
    /// 对应请求的消息哈希，绑定响应到请求以防止跨请求重放
    /// `None` 表示来自旧版客户端，跳过请求绑定检查
    #[serde(default)]
    pub request_hash: Option<Vec<u8>>,
}

impl ChatResponse {
    /// 创建一个新的签名响应
    pub fn new_signed(
        mldsa_private_key: &[u8],
        mldsa_public_key: &[u8],
        request_hash: Option<&[u8]>,
    ) -> crate::error::MessageResult<Self> {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_millis() as u64;
        let mut nonce = [0u8; 16];
        rand::rng().fill(&mut nonce);

        let hash = Self::compute_hash(timestamp, &nonce, request_hash);
        let signature = crate::signature::sign_data(mldsa_private_key, &hash).map_err(|e| {
            crate::error::MessageError::SignFailed(Box::new(std::io::Error::other(e.to_string())))
        })?;

        Ok(Self {
            timestamp,
            nonce,
            signature,
            sender_public_key: mldsa_public_key.to_vec(),
            request_hash: request_hash.map(|h| h.to_vec()),
        })
    }

    /// 验证响应签名
    pub fn verify(&self) -> crate::error::MessageResult<bool> {
        let hash = Self::compute_hash(self.timestamp, &self.nonce, self.request_hash.as_deref());
        crate::signature::verify_signature(&self.sender_public_key, &hash, &self.signature).map_err(
            |e| {
                crate::error::MessageError::VerifyFailed(Box::new(std::io::Error::other(
                    e.to_string(),
                )))
            },
        )
    }

    fn compute_hash(timestamp: u64, nonce: &[u8], request_hash: Option<&[u8]>) -> Vec<u8> {
        let mut hasher = sha2::Sha256::new();
        hasher.update(b"ChatResponse-v2");
        hasher.update(timestamp.to_be_bytes());
        hasher.update(nonce);
        if let Some(rh) = request_hash {
            hasher.update(rh);
        }
        hasher.finalize().to_vec()
    }
}

// ========== ChatMessage 方法 ==========

impl ChatMessage {
    /// 计算消息哈希（msgtype + timestamp + nonce + data）
    pub fn compute_hash(
        msgtype: ChatMessageType,
        timestamp: u64,
        nonce: &[u8],
        data: &[u8],
    ) -> Vec<u8> {
        let mut hasher = Sha256::new();
        hasher.update([msgtype as u8]);
        hasher.update(timestamp.to_be_bytes());
        hasher.update(nonce);
        hasher.update(data);
        hasher.finalize().to_vec()
    }

    /// 创建一个新的聊天消息实例，并使用 ML-DSA 签名
    ///
    /// # 参数
    /// - `data`: 已预处理的数据（FileStream 类型应为序列化后的 FileStreamChunk）
    pub fn new_signed(
        msgtype: ChatMessageType,
        data: Vec<u8>,
        mldsa_private_key: &[u8],
        mldsa_public_key: &[u8],
    ) -> crate::error::MessageResult<Self> {
        let timestamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis() as u64;
        let mut nonce = [0u8; 16];
        rng().fill(&mut nonce);

        // 对处理后的数据进行哈希
        let hash = Self::compute_hash(msgtype, timestamp, &nonce, &data);

        // 使用 ML-DSA 签名
        let signature = crate::signature::sign_data(mldsa_private_key, &hash).map_err(|e| {
            crate::error::MessageError::SignFailed(Box::new(std::io::Error::other(e.to_string())))
        })?;

        Ok(Self {
            msgtype,
            timestamp,
            nonce,
            data,
            hash,
            signature,
            sender_public_key: mldsa_public_key.to_vec(),
        })
    }

    /// 验证消息签名、哈希和新鲜度（不包含 DHT 身份绑定验证）
    ///
    /// 这是基础验证层，仅验证：
    /// 1. 消息新鲜度（防止重放攻击）
    /// 2. 数据完整性（Hash 匹配）
    /// 3. ML-DSA 签名有效性
    ///
    /// ⚠️ 注意：此方法不验证 sender_public_key 是否与 DHT 中注册的身份绑定一致。
    /// 攻击者可以使用自己的 ML-DSA 密钥对签名消息，但声称是其他身份。
    /// 在需要身份绑定的场景中，请使用 verify_with_identity_binding() 替代。
    pub fn verify(&self) -> crate::error::MessageResult<bool> {
        // 1. 验证消息新鲜度 (防止重放攻击的一部分)
        if !self.is_fresh() {
            let now = SystemTime::now();
            let message_time = UNIX_EPOCH + Duration::from_millis(self.timestamp);
            tracing::debug!(
                "verify: freshness failed. now_ms={}, msg_timestamp={}, msg_time={:?}, future_tolerance_secs={}",
                now.duration_since(UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0),
                self.timestamp,
                message_time.duration_since(UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0),
                MESSAGE_FUTURE_TOLERANCE_SECS
            );
            return Ok(false);
        }

        // 2. 验证数据完整性 (Hash)
        let computed = Self::compute_hash(self.msgtype, self.timestamp, &self.nonce, &self.data);
        if !constant_time_compare(&computed, &self.hash) {
            tracing::debug!(
                "verify: hash mismatch. computed={}.. stored={}.. data_len={}",
                &hex::encode(&computed)[..16.min(hex::encode(&computed).len())],
                &hex::encode(&self.hash)[..16.min(hex::encode(&self.hash).len())],
                self.data.len()
            );
            return Ok(false);
        }

        // 3. 验证 ML-DSA 签名
        match crate::signature::verify_signature(&self.sender_public_key, &computed, &self.signature)
        {
            Ok(true) => Ok(true),
            Ok(false) => {
                tracing::debug!(
                    "verify: signature invalid. pubkey_len={}, sig_len={}, data_len={}",
                    self.sender_public_key.len(),
                    self.signature.len(),
                    self.data.len()
                );
                Ok(false)
            }
            Err(e) => Err(crate::error::MessageError::VerifyFailed(Box::new(
                std::io::Error::other(e.to_string()),
            ))),
        }
    }

/// 检查消息是否在有效时间窗口内（防止重放攻击）
///
/// 规则：
/// - 消息来自过去：年龄 ≤ `MESSAGE_MAX_AGE_SECS`
/// - 消息来自未来：超前量 ≤ `MESSAGE_FUTURE_TOLERANCE_SECS`（容忍 60 秒以内时钟偏差）
pub fn is_fresh(&self) -> bool {
        let now = SystemTime::now();
        let message_time = UNIX_EPOCH + Duration::from_millis(self.timestamp);
        match now.duration_since(message_time) {
            // 消息来自过去：检查最大年龄
            Ok(age) => age <= Duration::from_secs(MESSAGE_MAX_AGE_SECS),
            // 消息来自未来（本机时钟偏慢或对方时钟偏快）：仅在容差范围内通过
            Err(_) => {
                if let Ok(future) = message_time.duration_since(now) {
                    future <= Duration::from_secs(MESSAGE_FUTURE_TOLERANCE_SECS)
                } else {
                    false
                }
            }
        }
    }
}

// ========== OnlineStatusPayload ==========

/// 在线状态通知负载
///
/// 通过 gossipsub 协议发布/订阅，用于实时通知好友自己的在线状态变化。
/// 该结构体被序列化后放入 ChatMessage.data 字段，利用 ChatMessage 的 ML-DSA 签名机制保证安全性。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OnlineStatusPayload {
    /// 发送者的 ML-DSA 公钥（hex 编码）
    pub mldsa_pubkey_hex: String,
    /// 是否在线
    pub online: bool,
    /// 状态变化时间戳（ms since UNIX_EPOCH）
    pub timestamp: u64,
    /// 当前 PeerID（Base58 编码）
    pub peer_id: String,
    /// 当前监听地址列表
    pub listen_addrs: Vec<String>,
}
