use crate::crypto::constant_time_compare;
use libp2p::PeerId;
use rand::{RngExt, rng};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub mod file_stream;
pub use file_stream::{
    ChunkReadConfig, ChunkResponse, FileHashInfo, FileStreamChunk, FileStreamMeta,
};

/// 消息最大允许年龄（秒）
const MESSAGE_MAX_AGE_SECS: u64 = 60 * 60;
/// 消息未来时间容差（秒）
const MESSAGE_FUTURE_TOLERANCE_SECS: u64 = 60;

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[repr(u8)]
#[non_exhaustive]
pub enum ChatMessageType {
    Text = 0,
    FileHash = 1,
    FileStream = 2,
    /// 文件下载请求（接收方发起，请求发送方开始传输文件）
    FileDownloadRequest = 3,
    /// 消息送达回执
    DeliveryReceipt = 4,
    /// ML-KEM 公钥交换（连接建立时自动交换最新的临时加密公钥）
    MlkemKeyExchange = 5,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
/// 聊天消息结构，包含签名、时间戳和完整性校验
pub struct ChatMessage {
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

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChatResponse {
    pub timestamp: u64,
    /// 随机 nonce，用于防止重放攻击
    pub nonce: [u8; 16],
    /// 响应方的 ML-DSA 签名，签名内容为 (timestamp, nonce)
    pub signature: Vec<u8>,
    /// 响应方的 ML-DSA 公钥（原始格式，1952 字节）
    pub sender_public_key: Vec<u8>,
}

impl ChatResponse {
    /// 创建一个新的签名响应
    pub fn new_signed(
        mldsa_private_key: &[u8],
        mldsa_public_key: &[u8],
    ) -> crate::error::MessageResult<Self> {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_millis() as u64;
        let mut nonce = [0u8; 16];
        rand::rng().fill(&mut nonce);

        let hash = Self::compute_hash(timestamp, &nonce);
        let signature = crate::signature::sign_data(mldsa_private_key, &hash).map_err(|e| {
            crate::error::MessageError::SignFailed(Box::new(std::io::Error::new(
                std::io::ErrorKind::Other,
                e.to_string(),
            )))
        })?;

        Ok(Self {
            timestamp,
            nonce,
            signature,
            sender_public_key: mldsa_public_key.to_vec(),
        })
    }

    /// 验证响应签名
    pub fn verify(&self) -> crate::error::MessageResult<bool> {
        let hash = Self::compute_hash(self.timestamp, &self.nonce);
        crate::signature::verify_signature(&self.sender_public_key, &hash, &self.signature).map_err(
            |e| {
                crate::error::MessageError::VerifyFailed(Box::new(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    e.to_string(),
                )))
            },
        )
    }

    fn compute_hash(timestamp: u64, nonce: &[u8]) -> Vec<u8> {
        let mut hasher = sha2::Sha256::new();
        hasher.update(b"ChatResponse-v1");
        hasher.update(timestamp.to_be_bytes());
        hasher.update(nonce);
        hasher.finalize().to_vec()
    }
}

// ========== ChatMessage 方法 ==========

impl ChatMessage {
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
            crate::error::MessageError::SignFailed(Box::new(std::io::Error::new(
                std::io::ErrorKind::Other,
                e.to_string(),
            )))
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
            return Ok(false);
        }

        // 2. 验证数据完整性 (Hash)
        let computed = Self::compute_hash(self.msgtype, self.timestamp, &self.nonce, &self.data);
        if !constant_time_compare(&computed, &self.hash) {
            return Ok(false);
        }

        // 3. 验证 ML-DSA 签名
        crate::signature::verify_signature(&self.sender_public_key, &computed, &self.signature)
            .map_err(|e| {
                crate::error::MessageError::VerifyFailed(Box::new(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    e.to_string(),
                )))
            })
    }

    /// 验证消息签名、哈希、新鲜度，以及 DHT 身份绑定
    ///
    /// 在 verify() 的基础上，额外验证：
    /// 4. sender_public_key（ML-DSA 公钥）是否在 DHT 中有合法的身份绑定记录
    /// 5. 如果提供了 expected_peer_id，验证其是否与 DHT 绑定一致
    ///
    /// 这是完整的消息验证链路，防止攻击者使用自己的密钥对签名消息但冒充他人身份。
    ///
    /// # 参数
    /// - `store`: DHT 记录存储，用于查询身份绑定
    /// - `expected_peer_id`: 期望的发送方 PeerID（如果为 None 则跳过 PeerID 匹配检查）
    ///
    /// # 返回
    /// - `Ok(true)`: 所有验证通过
    /// - `Ok(false)`: 任一验证失败
    /// - `Err(e)`: 验证过程中发生错误（如数据库错误）
    pub fn verify_with_identity_binding(
        &self,
        store: &crate::p2p::dht::RedbRecordStore,
        expected_peer_id: Option<&PeerId>,
    ) -> crate::error::MessageResult<bool> {
        // 1-3. 基础验证（签名、哈希、新鲜度）
        if !self.verify()? {
            return Ok(false);
        }

        // 4. DHT 身份绑定验证
        // 检查 sender_public_key 是否在 DHT 中有合法的身份绑定记录
        let sender_pubkey_hex = hex::encode(&self.sender_public_key);
        match store.get_peerid_by_pubkey(&sender_pubkey_hex)? {
            Some(dht_peer_id) => {
                // 5. 如果提供了期望的 PeerID，验证是否匹配
                if let Some(expected) = expected_peer_id
                    && &dht_peer_id != expected
                {
                    tracing::warn!(
                        "身份绑定验证失败: ML-DSA {} 的 DHT PeerID {} 与消息来源 PeerID {} 不匹配",
                        &sender_pubkey_hex[..16],
                        dht_peer_id,
                        expected
                    );
                    return Ok(false);
                }
                tracing::debug!(
                    "身份绑定验证通过: ML-DSA {} -> PeerID {}",
                    &sender_pubkey_hex[..16],
                    dht_peer_id
                );
                Ok(true)
            }
            None => {
                tracing::warn!(
                    "身份绑定验证失败: ML-DSA {} 在 DHT 中无身份绑定记录",
                    &sender_pubkey_hex[..16]
                );
                Ok(false)
            }
        }
    }

    pub fn is_fresh(&self) -> bool {
        let now = SystemTime::now();
        let message_time = UNIX_EPOCH + Duration::from_millis(self.timestamp);
        if message_time > now + Duration::from_secs(MESSAGE_FUTURE_TOLERANCE_SECS) {
            return false;
        }
        match now.duration_since(message_time) {
            Ok(age) => age <= Duration::from_secs(MESSAGE_MAX_AGE_SECS),
            Err(_) => false,
        }
    }
}
