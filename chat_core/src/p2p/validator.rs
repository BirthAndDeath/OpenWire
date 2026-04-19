use libp2p::PeerId;
use libp2p::kad::RecordKey;
use lru::LruCache;
use rand::RngExt;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::time::{Duration, SystemTime};

/// 挑战验证器配置
#[derive(Debug, Clone)]
pub struct ChallengeValidatorConfig {
    /// 挑战超时时间（秒）
    pub challenge_timeout: Duration,
    /// 每个节点的最大记录数
    pub max_records_per_peer: usize,
    /// 最大待处理挑战数
    pub max_pending_challenges: usize,
    /// 是否启用严格验证
    pub strict_validation: bool,
}

impl Default for ChallengeValidatorConfig {
    fn default() -> Self {
        Self {
            challenge_timeout: Duration::from_secs(30),
            max_records_per_peer: 1000,
            max_pending_challenges: 1000,
            strict_validation: true,
        }
    }
}

/// 挑战信息
#[derive(Debug, Clone)]
struct ChallengeInfo {
    peer_id: PeerId,
    key: RecordKey,
    nonce: [u8; 32],
    created_at: SystemTime,
}

/// 节点统计信息
#[derive(Debug, Clone)]
pub struct PeerStats {
    records_count: usize,
    last_seen: SystemTime,
    challenges_passed: u32,
    challenges_failed: u32,
}

impl Default for PeerStats {
    fn default() -> Self {
        Self {
            records_count: 0,
            last_seen: SystemTime::now(),
            challenges_passed: 0,
            challenges_failed: 0,
        }
    }
}

/// DHT 挑战验证器
pub struct ChallengeValidator {
    config: ChallengeValidatorConfig,
    pending_challenges: LruCache<[u8; 32], ChallengeInfo>, // key: challenge_hash
    peer_stats: HashMap<PeerId, PeerStats>,
}

impl ChallengeValidator {
    /// 创建新的验证器
    pub fn new(config: ChallengeValidatorConfig) -> Self {
        Self {
            config,
            pending_challenges: LruCache::new(NonZeroUsize::new(1000).unwrap()),
            peer_stats: HashMap::new(),
        }
    }

    /// 生成挑战
    pub fn generate_challenge(&mut self, peer_id: &PeerId, key: &RecordKey) -> Vec<u8> {
        let mut rng = rand::rng();
        let mut nonce = [0u8; 32];
        rng.fill(&mut nonce);

        let challenge_info = ChallengeInfo {
            peer_id: *peer_id,
            key: key.clone(),
            nonce,
            created_at: SystemTime::now(),
        };

        // 计算挑战哈希
        let challenge_hash = Self::compute_challenge_hash(&nonce, key);

        // 存储挑战信息
        self.pending_challenges.put(challenge_hash, challenge_info);

        // 返回挑战数据（nonce + timestamp）
        let timestamp = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let mut challenge_data = Vec::with_capacity(40);
        challenge_data.extend_from_slice(&nonce);
        challenge_data.extend_from_slice(&timestamp.to_be_bytes());

        challenge_data
    }

    /// 验证挑战响应
    pub fn verify_challenge_response(
        &mut self,
        peer_id: &PeerId,
        key: &RecordKey,
        challenge_data: &[u8],
        _signature: &[u8],
    ) -> bool {
        if challenge_data.len() < 32 {
            return false;
        }

        let nonce = &challenge_data[0..32];
        let challenge_hash = Self::compute_challenge_hash(nonce.try_into().unwrap(), key);

        // 查找待处理挑战
        let challenge_info = match self.pending_challenges.get(&challenge_hash) {
            Some(info) => info,
            None => return false,
        };

        // 验证 PeerID 匹配
        if &challenge_info.peer_id != peer_id {
            return false;
        }

        // 验证挑战未超时
        if let Ok(elapsed) = challenge_info.created_at.elapsed()
            && elapsed > self.config.challenge_timeout
        {
            self.pending_challenges.pop(&challenge_hash);
            return false;
        }

        // TODO: 验证签名（需要集成现有的加密模块）
        // 暂时返回 true 用于测试
        let is_valid = true;

        if is_valid {
            // 更新节点统计
            let stats = self.peer_stats.entry(*peer_id).or_default();
            stats.challenges_passed += 1;
            stats.last_seen = SystemTime::now();

            // 清理已使用的挑战
            self.pending_challenges.pop(&challenge_hash);
        } else {
            let stats = self.peer_stats.entry(*peer_id).or_default();
            stats.challenges_failed += 1;
        }

        is_valid
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

    /// 清理过期挑战
    pub fn cleanup_expired_challenges(&mut self) {
        let expired_keys: Vec<[u8; 32]> = self
            .pending_challenges
            .iter()
            .filter_map(|(key, info)| {
                if let Ok(elapsed) = info.created_at.elapsed() {
                    if elapsed > self.config.challenge_timeout {
                        Some(*key)
                    } else {
                        None
                    }
                } else {
                    None
                }
            })
            .collect();

        for key in expired_keys {
            self.pending_challenges.pop(&key);
        }
    }

    /// 计算挑战哈希
    fn compute_challenge_hash(nonce: &[u8; 32], key: &RecordKey) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update(nonce);
        hasher.update(key.as_ref());
        hasher.finalize().into()
    }

    /// 获取节点统计信息
    pub fn get_peer_stats(&self, peer_id: &PeerId) -> Option<&PeerStats> {
        self.peer_stats.get(peer_id)
    }

    /// 验证 DHT 记录的合法性
    pub fn validate_dht_record(
        &mut self,
        publisher: &PeerId,
        record_size: usize,
        key: &RecordKey,
    ) -> bool {
        if !self.config.strict_validation {
            return true;
        }

        // 检查资源限制
        if !self.check_resource_limits(publisher, record_size) {
            tracing::warn!(
                "DHT record rejected: resource limit exceeded for peer {}",
                publisher
            );
            return false;
        }

        // 生成并存储挑战（用于后续验证）
        let _challenge = self.generate_challenge(publisher, key);

        // 更新记录计数
        self.update_record_count(publisher, 1);

        true
    }
}
