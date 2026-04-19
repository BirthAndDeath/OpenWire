use crate::crypto::constant_time_compare;
use anyhow;
use libp2p::identity;
use rand::{RngExt, rng};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use snap::raw::decompress_len;
use snap::raw::{Decoder, Encoder};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
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

    /// 压缩数据
    fn compress_data(data: &[u8]) -> anyhow::Result<Vec<u8>> {
        let mut encoder = Encoder::new();
        let compressed = encoder.compress_vec(data)?;
        Ok(compressed)
    }

    /// 解压缩数据
    fn decompress_data(data: &[u8]) -> anyhow::Result<Vec<u8>> {
        const MAX_SIZE: usize = 1024 * 1024; // 1 MB

        if data.is_empty() {
            return Err(anyhow::anyhow!("Compressed data is empty"));
        }

        // 限制输入数据大小，防止处理过大的非有效负载或潜在的 DoS

        if data.len() > MAX_SIZE * 2 {
            return Err(anyhow::anyhow!(
                "Compressed data is suspiciously large: {} bytes",
                data.len()
            ));
        }

        // 预检解压后的大小
        let decompressed_len = decompress_len(data)?;

        if decompressed_len > MAX_SIZE {
            return Err(anyhow::anyhow!(
                "Decompressed data exceeds size limit: {} bytes",
                decompressed_len
            ));
        }

        let mut decoder = Decoder::new();
        // 根据预检的大小分配缓冲区
        let mut buffer = vec![0u8; decompressed_len];

        // 执行解压
        decoder.decompress(data, &mut buffer)?;

        Ok(buffer)
    }

    pub fn new_signed(
        msgtype: ChatMessageType,
        data: Vec<u8>,
        keypair: &identity::Keypair,
    ) -> anyhow::Result<Self> {
        let timestamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis() as u64;
        let mut nonce = [0u8; 16];
        rng().fill(&mut nonce);

        // 压缩数据
        let compressed_data = Self::compress_data(&data)?;

        // 对压缩后的数据进行哈希和签名
        let hash = Self::compute_hash(msgtype, timestamp, &nonce, &compressed_data);
        let signature = keypair.sign(&hash)?;
        let sender_public_key = keypair.public().encode_protobuf();

        Ok(Self {
            msgtype,
            timestamp,
            nonce,
            data: compressed_data,
            hash,
            signature,
            sender_public_key,
        })
    }

    /// 获取解压缩后的数据
    pub fn get_decompressed_data(&self) -> anyhow::Result<Vec<u8>> {
        Self::decompress_data(&self.data)
    }

    pub fn verify(&self, sender_peer_id: &libp2p::PeerId) -> anyhow::Result<bool> {
        // 1. 验证公钥格式并解码
        let public_key = identity::PublicKey::try_decode_protobuf(&self.sender_public_key)
            .map_err(|e| anyhow::anyhow!("Invalid sender public key format: {}", e))?;

        // 2. 验证公钥对应的 PeerID 是否匹配
        if &public_key.to_peer_id() != sender_peer_id {
            return Ok(false);
        }

        // 3. 验证消息新鲜度 (防止重放攻击的一部分)
        if !self.is_fresh() {
            return Ok(false);
        }

        // 4. 验证数据完整性 (Hash)
        // 注意：hash 是基于压缩后的数据计算的
        let computed = Self::compute_hash(self.msgtype, self.timestamp, &self.nonce, &self.data);
        if !constant_time_compare(&computed, &self.hash) {
            return Ok(false);
        }

        // 5. 验证签名
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
