use std::time::{Duration, SystemTime, UNIX_EPOCH};

use libp2p::identity;
use rand::{RngExt, rng};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[repr(u8)]
pub enum ChatMessageType {
    Text = 0,
    FileHash = 1,
    #[doc(hidden)]
    __NonExhaustive = 255,
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
    /// 发送者签名
    pub signature: Vec<u8>,
    /// 发送者公钥 protobuf 编码
    pub sender_public_key: Vec<u8>,
}

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

    pub fn new_signed(
        msgtype: ChatMessageType,
        data: Vec<u8>,
        keypair: &identity::Keypair,
    ) -> anyhow::Result<Self> {
        let timestamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis() as u64;
        let mut nonce = [0u8; 16];
        rng().fill(&mut nonce);
        let hash = Self::compute_hash(msgtype, timestamp, &nonce, &data);
        let signature = keypair.sign(&hash)?;
        let sender_public_key = keypair.public().encode_protobuf();

        Ok(Self {
            msgtype,
            timestamp,
            nonce,
            data,
            hash,
            signature,
            sender_public_key,
        })
    }

    pub fn verify(&self, sender_peer_id: &libp2p::PeerId) -> anyhow::Result<bool> {
        let public_key = identity::PublicKey::try_decode_protobuf(&self.sender_public_key)?;
        if &public_key.to_peer_id() != sender_peer_id {
            return Ok(false);
        }
        if !self.is_fresh() {
            return Ok(false);
        }
        let computed = Self::compute_hash(self.msgtype, self.timestamp, &self.nonce, &self.data);
        if computed != self.hash {
            return Ok(false);
        }
        Ok(public_key.verify(&computed, &self.signature))
    }

    pub fn is_fresh(&self) -> bool {
        let now = SystemTime::now();
        let message_time = UNIX_EPOCH + Duration::from_millis(self.timestamp);
        if message_time > now + Duration::from_secs(60) {
            return false;
        }
        match now.duration_since(message_time) {
            Ok(age) => age <= Duration::from_secs(60 * 60 * 24),
            Err(_) => false,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChatResponse {
    pub timestamp: u64,
}
