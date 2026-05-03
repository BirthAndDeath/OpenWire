use libp2p::PeerId;
use libp2p::kad::RecordKey;
use std::collections::HashMap;
use std::time::SystemTime;

use crate::signature::DhtRecordSignature;

/// DHT 记录验证器配置
#[derive(Debug, Clone)]
pub struct RecordValidatorConfig {
    /// 每个节点的最大记录数
    pub max_records_per_peer: usize,
    /// 是否启用严格验证
    pub strict_validation: bool,
    /// 签名最大允许年龄（毫秒），防止重放攻击
    pub max_signature_age_ms: u64,
}

impl Default for RecordValidatorConfig {
    fn default() -> Self {
        Self {
            max_records_per_peer: 1000,
            strict_validation: true,
            max_signature_age_ms: 60000, // 60秒
        }
    }
}

/// 节点统计信息
#[derive(Debug, Clone)]
pub struct PeerStats {
    records_count: usize,
    last_seen: SystemTime,
    records_failed: u32,
}

impl Default for PeerStats {
    fn default() -> Self {
        Self {
            records_count: 0,
            last_seen: SystemTime::now(),
            records_failed: 0,
        }
    }
}

/// DHT 记录验证参数
///
/// 将 DHT 记录验证所需的全部参数封装为结构体，
/// 避免 `validate_dht_record` 方法参数过多触发 clippy::too_many_arguments。
#[derive(Debug, Clone)]
pub struct DhtRecordValidationParams<'a> {
    /// 发布者 PeerID
    pub publisher: &'a PeerId,
    /// 记录键
    pub key: &'a RecordKey,
    /// 记录值
    pub record_value: &'a [u8],
    /// ML-DSA 签名
    pub signature: &'a [u8],
    /// 签名时间戳（Unix 毫秒）
    pub timestamp: u64,
    /// 盐值（32 字节，防止重放攻击）
    pub salt: &'a [u8; 32],
}

/// DHT 记录验证器（基于签名验证）
pub struct RecordValidator {
    config: RecordValidatorConfig,
    peer_stats: HashMap<PeerId, PeerStats>,
    /// 缓存的公钥：PeerID -> ML-DSA/Ed25519 公钥
    peer_public_keys: HashMap<PeerId, Vec<u8>>,
}

impl RecordValidator {
    /// 创建新的验证器
    pub fn new(config: RecordValidatorConfig) -> Self {
        Self {
            config,
            peer_stats: HashMap::new(),
            peer_public_keys: HashMap::new(),
        }
    }

    /// 注册节点的公钥（用于签名验证）
    pub fn register_peer_public_key(&mut self, peer_id: &PeerId, public_key: Vec<u8>) {
        self.peer_public_keys.insert(*peer_id, public_key);
    }

    /// 检查节点是否超过资源限制
    pub fn check_resource_limits(&self, peer_id: &PeerId, current_records: usize) -> bool {
        if !self.config.strict_validation {
            return true;
        }

        if let Some(stats) = self.peer_stats.get(peer_id) {
            stats.records_count + current_records <= self.config.max_records_per_peer
        } else {
            current_records <= self.config.max_records_per_peer
        }
    }

    /// 更新节点记录统计
    pub fn update_record_count(&mut self, peer_id: &PeerId, delta: isize) {
        let stats = self.peer_stats.entry(*peer_id).or_default();
        if delta > 0 {
            stats.records_count += delta as usize;
        } else {
            stats.records_count = stats.records_count.saturating_sub((-delta) as usize);
        }
        stats.last_seen = SystemTime::now();
    }

    /// 获取节点统计信息
    pub fn get_peer_stats(&self, peer_id: &PeerId) -> Option<&PeerStats> {
        self.peer_stats.get(peer_id)
    }

    /// 验证 DHT 记录的合法性（基于签名验证）
    /// 委托给 DhtRecordSignature 进行实际的签名验证
    ///
    /// # 注意
    /// 调用方需要从 DHT 记录中提取正确的 signature/timestamp/salt 字段传入。
    ///
    /// # 调用场景
    /// - events.rs::handle_kademlia_event() 在 GetRecord 查询返回记录时，
    ///   从存储中提取签名元数据后调用此方法进行验证。
    /// - 此方法不直接验证 libp2p::kad::Record（该类型不包含自定义签名元数据），
    ///   而是验证从 StoredRecord 中提取的 DhtRecordSignature。
    pub fn validate_dht_record(&mut self, params: DhtRecordValidationParams) -> bool {
        if !self.config.strict_validation {
            return true;
        }

        // 检查资源限制（每次验证视为一条记录）
        if !self.check_resource_limits(params.publisher, 1) {
            tracing::warn!(
                "DHT record rejected: resource limit exceeded for peer {}",
                params.publisher
            );
            return false;
        }

        // 获取发布者的公钥
        let public_key = match self.peer_public_keys.get(params.publisher) {
            Some(pk) => pk,
            None => {
                tracing::warn!(
                    "No public key registered for publisher {}",
                    params.publisher
                );
                return false;
            }
        };

        // 使用 DhtRecordSignature 进行验证（包含时间戳检查和签名验证）
        let sig = DhtRecordSignature {
            timestamp: params.timestamp,
            salt: *params.salt,
            signature: params.signature.to_vec(),
        };

        match sig.verify(
            public_key,
            params.key.as_ref(),
            params.record_value,
            params.publisher,
            self.config.max_signature_age_ms,
        ) {
            Ok(true) => {
                // 更新记录计数
                self.update_record_count(params.publisher, 1);
                true
            }
            Ok(false) => {
                tracing::warn!(
                    "DHT record signature verification failed for peer {}",
                    params.publisher
                );
                let stats = self.peer_stats.entry(*params.publisher).or_default();
                stats.records_failed += 1;
                false
            }
            Err(e) => {
                tracing::error!("DHT record signature verification error: {:?}", e);
                false
            }
        }
    }
}
