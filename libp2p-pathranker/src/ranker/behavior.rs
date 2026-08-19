use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use libp2p::{Multiaddr, PeerId, multiaddr::Protocol};

/// 警戒等级，节点根据本地感知的威胁程度自动调节防御强度。
///
/// 等级从 Normal(0) 到 Shelter(3)，逐级收紧资源分配和准入策略。
/// 评估由 SmartNode 的 AlertEvaluator 周期性完成，结果注入 BehaviorRanker。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum AlertLevel {
    Normal = 0,
    Vigilant = 1,
    Defensive = 2,
    Shelter = 3,
}

impl AlertLevel {
    pub fn from_score(score: f64) -> Self {
        if score > 3.0 {
            AlertLevel::Shelter
        } else if score > 2.0 {
            AlertLevel::Defensive
        } else if score > 1.0 {
            AlertLevel::Vigilant
        } else {
            AlertLevel::Normal
        }
    }
}

/// 行为感知评分器配置。
///
/// 所有参数都有合理的默认值，可直接使用 `RankerConfig::default()`。
#[derive(Debug, Clone)]
pub struct RankerConfig {
    /// EWMA 平滑因子，越大越侧重于最新观测值（默认 0.2）
    pub alpha: f64,
    /// 长期信用初始分（默认 50.0）
    pub initial_credit: f64,
    /// 信用低于此阈值时路径被忽略，除非是唯一候选（默认 30.0）
    pub credit_threshold: f64,
    /// 滑动窗口大小，用于突变检测（默认 10）
    pub recent_window: usize,
    /// 触发突变惩罚的连续失败次数（默认 3）
    pub burst_failure_threshold: usize,
    /// 突变后恢复所需的连续成功次数（默认 5）
    pub recovery_success_threshold: usize,
    /// 路由探索预算比例，每次 `rank()` 以此概率选未知地址（默认 1%）
    pub exploration_budget: f64,
    /// 多样性 top-K，排序后取前 K 个做加权随机（默认 3）
    pub diversity_top_k: usize,
    /// 超过此时间未使用则评分乘以 time_decay（默认 5 分钟）
    pub decay_timeout: Duration,
    /// 邻居评分打折系数，观察者自身信用越低折扣越大（默认 0.8）
    pub neighbor_discount: f64,
    /// 每个 peer 最大记录地址数（默认 20）
    pub max_addrs_per_peer: usize,
    /// 全局最大记录 peer 数，超过后 LRU 淘汰（默认 1000）
    pub max_peers: usize,
    /// 突变后观察期（秒），期内连续 3 次成功可快速恢复（默认 30s）
    pub mutation_observation_secs: u64,
    /// 地址超过此小时数未使用，`rank()` 中临时信用 ×0.9（默认 1 小时）
    pub unused_decay_hours: u64,
}

impl Default for RankerConfig {
    fn default() -> Self {
        Self {
            alpha: 0.2,
            initial_credit: 50.0,
            credit_threshold: 30.0,
            recent_window: 10,
            burst_failure_threshold: 3,
            recovery_success_threshold: 5,
            exploration_budget: 0.01,
            diversity_top_k: 3,
            decay_timeout: Duration::from_secs(5 * 60),
            neighbor_discount: 0.8,
            max_addrs_per_peer: 20,
            max_peers: 1000,
            mutation_observation_secs: 30,
            unused_decay_hours: 1,
        }
    }
}

impl RankerConfig {
    /// 根据警戒等级动态计算信用阈值。等级每升一级 +15。
    pub fn credit_threshold_at(&self, level: AlertLevel) -> f64 {
        self.credit_threshold + (level as u8 as f64) * 15.0
    }

    /// 根据警戒等级动态计算探索预算。等级每升一级 ×0.3。
    pub fn exploration_budget_at(&self, level: AlertLevel) -> f64 {
        self.exploration_budget * (1.0 - (level as u8 as f64) * 0.3).max(0.0)
    }

    /// 根据警戒等级动态计算突变阈值。等级越高阈值越低（更易触发）。
    pub fn burst_failure_threshold_at(&self, level: AlertLevel) -> usize {
        let base = self.burst_failure_threshold as i32;
        (base - level as i32).max(1) as usize
    }

    /// 根据警戒等级动态计算多样性 top-K。
    pub fn diversity_top_k_at(&self, level: AlertLevel) -> usize {
        match level {
            AlertLevel::Normal => self.diversity_top_k,
            AlertLevel::Vigilant => (self.diversity_top_k + 1).min(5),
            AlertLevel::Defensive => (self.diversity_top_k / 2).max(1),
            AlertLevel::Shelter => 1.min(self.diversity_top_k),
        }
    }

    /// 警戒等级对自评分影响的放大倍数（等级越高自评越主导）。
    pub fn self_score_multiplier_at(&self, level: AlertLevel) -> f64 {
        1.0 + (level as u8 as f64) * 0.5
    }
}

/// 单条地址的评分条目。
///
/// 记录了 EWMA 延迟、EWMA 成功率、综合评分、长期信用、
/// 滑动窗口成功/失败历史、延迟历史、路径类型、突变状态以及远程自评分。
#[derive(Debug, Clone)]
pub struct ScoreEntry {
    /// 短期 EWMA 延迟（ms），初始 500ms
    pub latency_ewma: f64,
    /// 短期 EWMA 成功率 [0,1]，初始 0.5
    pub success_rate: f64,
    /// 综合评分缓存 [0,1]，由 `feedback()` 每次更新计算
    pub score: f64,
    /// 长期信用 [0,100]，慢变量，突变时减半
    pub credit: f64,
    /// 最近更新时间
    pub last_updated: Instant,
    /// 最近使用时间（用于未使用衰减检测）
    pub last_used: Instant,
    /// 滑动窗口：最近 N 次成功/失败记录
    pub recent_success: VecDeque<bool>,
    /// 路径类型（Direct/Relay/Quic/HolePunch）
    pub path_type: PathType,
    /// 突变后还需连续成功次数才能恢复
    pub recovery_needed: usize,
    /// 最近 3 次延迟历史，用于趋势检测
    pub latency_history: VecDeque<f64>,
    /// 突变惩罚结束时间。期内连续 3 次成功可提前恢复
    pub mutation_penalty_until: Option<Instant>,
    /// 远程节点自评分数 [0,1]，从协议 ScoreResponse 获取
    pub peer_self_score: f64,
}

impl ScoreEntry {
    pub fn new(path_type: PathType) -> Self {
        Self {
            latency_ewma: 500.0,
            success_rate: 0.5,
            score: 0.5,
            credit: 50.0,
            last_updated: Instant::now(),
            last_used: Instant::now(),
            recent_success: VecDeque::with_capacity(10),
            path_type,
            recovery_needed: 0,
            latency_history: VecDeque::with_capacity(5),
            mutation_penalty_until: None,
            peer_self_score: 1.0,
        }
    }
}

/// 路径类型枚举。不同类型在评分中享有不同加成分（bonus）。
///
/// 优先级：Direct > Quic > HolePunch > Relay/Unknown
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PathType {
    Direct,
    HolePunch,
    Relay,
    Quic,
    Unknown,
}

impl PathType {
    /// 根据 multiaddr 推断路径类型，优先级：Relay > Quic > WebRTC > WebSocket > TCP
    pub fn from_multiaddr(addr: &Multiaddr) -> Self {
        let mut has_quic = false;
        let mut has_circuit = false;
        let mut has_webrtc = false;
        for p in addr.iter() {
            match p {
                Protocol::P2pCircuit => has_circuit = true,
                Protocol::Quic | Protocol::QuicV1 => has_quic = true,
                Protocol::WebRTC | Protocol::WebRTCDirect => has_webrtc = true,
                _ => {}
            }
        }
        if has_circuit {
            PathType::Relay
        } else if has_webrtc {
            PathType::Direct
        } else if has_quic {
            PathType::Quic
        } else {
            PathType::Direct
        }
    }

    /// 协议偏好加成，控制台输出顺序：QUIC > Direct > HolePunch > WebSocket > Relay
    /// 加成值会叠加到 rank() 的综合评分中，影响地址排序优先级。
    pub fn bonus(&self) -> f64 {
        match self {
            PathType::Quic => 0.15,
            PathType::Direct => 0.10,
            PathType::HolePunch => 0.08,
            PathType::Relay => 0.0,
            PathType::Unknown => 0.0,
        }
    }
}

/// 核心行为感知评分器。
///
/// 维护双层映射：`PeerId → (Multiaddr → ScoreEntry)`。
/// 支持 EWMA 延迟/成功率、长期信用、突变检测、延迟趋势分析、
/// 远程自评融合、本地负载感知、LRU 内存淘汰。
///
/// # 双维评分模型
///
/// `effective_score = score * peer_self_score`
/// - `score`：基于历史表现的他评（EWMA 延迟 + 成功率）
/// - `peer_self_score`：远程节点自评的当前负载（从协议获取）
///
/// 当远程节点过载时自评下降，有效评分自然降低，流量自动分流。
pub struct BehaviorRanker {
    scores: HashMap<PeerId, HashMap<Multiaddr, ScoreEntry>>,
    config: RankerConfig,
    self_score: f64,
    alert_level: AlertLevel,
    /// 上次 feedback() 是否触发了突变惩罚，由 consume_mutation_flag() 消费
    mutation_flagged: bool,
}

impl BehaviorRanker {
    pub fn new(config: RankerConfig) -> Self {
        Self {
            scores: HashMap::new(),
            config,
            self_score: 1.0,
            alert_level: AlertLevel::Normal,
            mutation_flagged: false,
        }
    }

    pub fn with_default() -> Self {
        Self::new(RankerConfig::default())
    }

    pub fn set_alert_level(&mut self, level: AlertLevel) {
        self.alert_level = level;
    }

    pub fn alert_level(&self) -> AlertLevel {
        self.alert_level
    }

    /// 设置本地自评分 [0,1]。由 SmartNode 的负载监控定期调用。
    /// 1.0 = 完全空闲，0.0 = 完全过载。`< 0.3` 触发 overloaded 标记。
    pub fn set_self_score(&mut self, score: f64) {
        self.self_score = score.clamp(0.0, 1.0);
    }

    /// 获取当前本地自评分。
    pub fn self_score(&self) -> f64 {
        self.self_score
    }

    /// 是否过载（`self_score < 0.3`）。
    /// 过载时 `rank()` 会关闭探索、减少多样性，对外响应会标记 `overloaded=true`。
    pub fn is_overloaded(&self) -> bool {
        // 警戒等级越高，过载阈值越严格
        let threshold = match self.alert_level {
            AlertLevel::Normal => 0.3,
            AlertLevel::Vigilant => 0.4,
            AlertLevel::Defensive => 0.5,
            AlertLevel::Shelter => 0.7,
        };
        self.self_score < threshold
    }

    /// 消费突变标志：如果上次 feedback() 触发了突变惩罚，返回 true 并清除标志。
    /// 由 SmartNode 在 dial 失败后调用，以通知 AlertEvaluator。
    pub fn consume_mutation_flag(&mut self) -> bool {
        let flag = self.mutation_flagged;
        self.mutation_flagged = false;
        flag
    }

    /// 获取某 peer 的地址评分表。如果该 peer 不存在则创建空表。
    /// 当全局 peer 数达到 `max_peers` 时，淘汰平均评分最低的 peer（保留高分可靠节点），
    /// 淘汰键为注意评分而非 LRU（不维护 peer 级最近使用时间）。
    pub fn get_or_insert(&mut self, peer: &PeerId) -> &mut HashMap<Multiaddr, ScoreEntry> {
        if self.scores.len() >= self.config.max_peers && !self.scores.contains_key(peer) {
            let evict = self
                .scores
                .iter()
                .min_by(|(_, map_a), (_, map_b)| {
                    let avg_a = map_a.values().map(|e| e.score).sum::<f64>()
                        / (map_a.len() as f64).max(1.0);
                    let avg_b = map_b.values().map(|e| e.score).sum::<f64>()
                        / (map_b.len() as f64).max(1.0);
                    avg_a
                        .partial_cmp(&avg_b)
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .map(|(k, _)| *k);
            if let Some(k) = evict {
                self.scores.remove(&k);
            }
        }
        self.scores.entry(*peer).or_default()
    }

    pub fn get_entry(&self, peer: &PeerId, addr: &Multiaddr) -> Option<&ScoreEntry> {
        self.scores.get(peer).and_then(|map| map.get(addr))
    }

    pub fn get_entry_mut(&mut self, peer: &PeerId, addr: &Multiaddr) -> Option<&mut ScoreEntry> {
        self.scores.get_mut(peer).and_then(|map| map.get_mut(addr))
    }

    pub fn has_local_scores(&self, peer: &PeerId) -> bool {
        self.scores
            .get(peer)
            .map(|m| !m.is_empty())
            .unwrap_or(false)
    }

    /// 对候选地址排序，返回按有效评分降序的地址序列。
    ///
    /// 评分计算步骤：
    /// 1. 基础评分 = `entry.score`（无历史时使用 PathType 初始分）
    /// 2. 协议偏好加成：`PathType::bonus()` 叠加（QUIC > Direct > HolePunch > Relay）
    /// 3. 延迟趋势衰减：最近 3 次持续上升 → ×0.9
    /// 4. 融合远程自评：`effective_score = score * peer_self_score`
    ///    警戒等级越高自评权重越大（`self_score_multiplier` 放大）
    /// 5. 信用过滤：使用 `credit_threshold_at(alert_level)` 动态阈值
    /// 6. 未使用地址临时信用 ×0.9
    ///
    /// 探索预算和多样性也由 `alert_level` 动态调节。
    pub fn rank(&self, peer: &PeerId, addrs: Vec<Multiaddr>) -> Vec<Multiaddr> {
        if addrs.is_empty() {
            return vec![];
        }

        let now = Instant::now();
        let unused_cutoff = Duration::from_secs(self.config.unused_decay_hours * 3600);
        let credit_threshold = self.config.credit_threshold_at(self.alert_level);
        let self_score_mult = self.config.self_score_multiplier_at(self.alert_level);

        // 默认初始分：无历史记录的地址使用此值，确保新地址也有机会被尝试。
        // 加上 protocol_bonus 后（QUIC=0.15 → 0.45，Direct=0.10 → 0.40），
        // 低于 ScoreEntry 的默认 score（0.5），所以有历史记录的地址始终优先于新地址。
        // 各协议新地址的初始排序：QUIC(0.45) > Direct(0.40) > HolePunch(0.38) > Relay(0.30)
        const DEFAULT_SCORE: f64 = 0.3;
        const DEFAULT_CREDIT: f64 = 50.0;

        let mut scored: Vec<_> = addrs
            .iter()
            .cloned()
            .map(|addr| {
                let path_type = PathType::from_multiaddr(&addr);
                let protocol_bonus = path_type.bonus();

                let entry = self.scores.get(peer).and_then(|m| m.get(&addr));
                match entry {
                    Some(e) => {
                        // feedback() 已包含 PathType::bonus()，此处不再重复叠加
                        let mut adjusted_score = e.score;

                        if e.latency_history.len() >= 3 {
                            let recent: Vec<_> = e.latency_history.iter().copied().collect();
                            if recent[0] <= recent[1] && recent[1] <= recent[2] {
                                adjusted_score *= 0.9;
                            }
                        }

                        let effective_self_score =
                            1.0 - (1.0 - e.peer_self_score) * self_score_mult;
                        adjusted_score *= effective_self_score;

                        let mut effective_credit = e.credit;
                        if now.duration_since(e.last_used) > unused_cutoff {
                            effective_credit *= 0.9;
                        }

                        (addr, adjusted_score, effective_credit)
                    }
                    None => {
                        // 无历史记录的地址：使用协议初始分 + 默认信用
                        (addr, DEFAULT_SCORE + protocol_bonus, DEFAULT_CREDIT)
                    }
                }
            })
            .collect();

        let has_valid = scored
            .iter()
            .any(|(_, _, credit)| *credit >= credit_threshold);
        if !has_valid && !scored.is_empty() {
            for (_, _, credit) in scored.iter_mut() {
                *credit = self.config.initial_credit;
            }
        } else if has_valid {
            scored.retain(|(_, _, credit)| *credit >= credit_threshold);
        }

        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        let mut result: Vec<Multiaddr> = scored.into_iter().map(|(addr, _, _)| addr).collect();

        if result.len() > 1 {
            let exploration_budget = self.config.exploration_budget_at(self.alert_level);
            let diversity_k = self.config.diversity_top_k_at(self.alert_level);
            let effective_exploration = if self.is_overloaded() {
                0.0
            } else {
                exploration_budget
            };
            let effective_k = if self.is_overloaded() {
                1.min(result.len())
            } else {
                diversity_k.min(result.len())
            };

            let k = effective_k;
            let top: Vec<_> = result[..k].to_vec();

            let weights: Vec<f64> = top
                .iter()
                .enumerate()
                .map(|(i, _)| {
                    if k == 1 {
                        1.0
                    } else if self.alert_level >= AlertLevel::Defensive {
                        1.0 / k as f64
                    } else if i == 0 {
                        0.7
                    } else if i == 1 {
                        0.2
                    } else {
                        0.1 / (k - 2) as f64
                    }
                })
                .collect();

            if rand::random::<f64>() < effective_exploration
                && let Some(unknown) = self.find_exploration_addr(peer, &addrs)
            {
                result.retain(|a| *a != unknown);
                result.insert(0, unknown);
                return result;
            }

            let idx = weighted_random(&weights);
            if idx < top.len() {
                let chosen = top[idx].clone();
                result.retain(|a| *a != chosen);
                result.insert(0, chosen);
            }
        }

        result
    }

    fn find_exploration_addr(&self, peer: &PeerId, candidates: &[Multiaddr]) -> Option<Multiaddr> {
        candidates
            .iter()
            .find(|addr| {
                self.scores
                    .get(peer)
                    .map(|m| m.get(*addr).map(|e| e.score < 0.3).unwrap_or(true))
                    .unwrap_or(true)
            })
            .cloned()
    }

    /// 反馈通信结果，更新评分。
    ///
    /// 更新项：
    /// - EWMA 延迟（仅 `latency > 0` 时更新，避免虚假高分）
    /// - EWMA 成功率
    /// - 延迟历史（保留最近 3 次，用于趋势检测）
    /// - 综合评分：`base = success_rate / (1 + latency_ewma/100)`，加上路径类型 bonus，乘以时间衰减
    /// - 滑动窗口（最近 N 次成功/失败）
    /// - 长期信用：成功 +0.1，失败 -0.5，突发失败减半 + 突变观察期
    /// - 突变观察期内连续 3 次成功可提前恢复
    ///
    /// 延迟趋势检测：如果最近 2 次延迟 EWMA 上升超过 20%，信用 -0.5。
    /// 这个机制约束了"慢变量"信用分，防止延迟恶化后信用仍高。
    pub fn feedback(&mut self, peer: &PeerId, addr: &Multiaddr, latency: Duration, success: bool) {
        let alpha = self.config.alpha;
        let decay_timeout = self.config.decay_timeout;
        let recent_window = self.config.recent_window;
        let burst_failure_threshold = self.config.burst_failure_threshold_at(self.alert_level);
        let recovery_success_threshold = self.config.recovery_success_threshold;
        let max_addrs_per_peer = self.config.max_addrs_per_peer;
        let mutation_observation = Duration::from_secs(self.config.mutation_observation_secs);

        let latency_ms = if latency.as_nanos() == 0 {
            None
        } else {
            Some(latency.as_secs_f64() * 1000.0)
        };

        let entry =
            self.get_or_insert_entry(peer, addr, latency_ms.unwrap_or(500.0), max_addrs_per_peer);

        let mut triggered_mutation = false;

        if let Some(l_ms) = latency_ms {
            entry.latency_ewma = alpha * l_ms + (1.0 - alpha) * entry.latency_ewma;

            entry.latency_history.push_back(l_ms);
            if entry.latency_history.len() > 3 {
                entry.latency_history.pop_front();
            }

            // 延迟趋势检测（需至少 3 次采样）：最近 3 次持续上升则降信用
            if entry.latency_history.len() >= 3 {
                let hist: Vec<_> = entry.latency_history.iter().copied().collect();
                if hist[0] <= hist[1] && hist[1] <= hist[2] {
                    entry.credit = (entry.credit - 0.5).max(0.0);
                }
            }
        }

        let success_val = if success { 1.0 } else { 0.0 };
        entry.success_rate = alpha * success_val + (1.0 - alpha) * entry.success_rate;

        let now = Instant::now();
        let time_decay = if now.duration_since(entry.last_used) > decay_timeout {
            0.95
        } else {
            1.0
        };

        let base_score = entry.success_rate / (1.0 + entry.latency_ewma / 100.0);
        entry.score = (base_score + entry.path_type.bonus()) * time_decay;
        entry.score = entry.score.clamp(0.0, 1.0);

        entry.recent_success.push_back(success);
        if entry.recent_success.len() > recent_window {
            entry.recent_success.pop_front();
        }

        if success {
            entry.credit = (entry.credit + 0.1).min(100.0);

            if entry.recovery_needed > 0 {
                entry.recovery_needed -= 1;
                if entry.recovery_needed == 0 {
                    entry.success_rate = 0.5;
                    entry.credit = 50.0;
                    entry.mutation_penalty_until = None;
                }
            }

            // 突变观察期内快速恢复：需连续 3 次成功
            if let Some(until) = entry.mutation_penalty_until {
                if now < until {
                    let consecutive = entry
                        .recent_success
                        .iter()
                        .rev()
                        .take_while(|&&s| s)
                        .count();
                    if consecutive >= 3 {
                        entry.success_rate = 0.5;
                        entry.credit = 50.0;
                        entry.score = 0.5;
                        entry.recovery_needed = 0;
                        entry.mutation_penalty_until = None;
                    }
                } else {
                    entry.mutation_penalty_until = None;
                    entry.recovery_needed = 0;
                }
            }
        } else {
            if entry.recovery_needed > 0 {
                entry.recovery_needed += 1;
            } else {
                let recent_failures: usize = entry.recent_success.iter().filter(|&&s| !s).count();
                if recent_failures >= burst_failure_threshold {
                    entry.credit *= 0.5;
                    entry.score = (entry.score * 0.3).max(0.0);
                    entry.recovery_needed = recovery_success_threshold;
                    entry.mutation_penalty_until = Some(now + mutation_observation);
                    triggered_mutation = true;
                } else {
                    entry.credit = (entry.credit - 0.5).max(0.0);
                }
            }
        }

        entry.last_updated = now;
        entry.last_used = now;
        self.mutation_flagged = triggered_mutation;
    }

    /// 注入邻居评分（通过协作协议获取的远程评分）。
    ///
    /// 应用 `neighbor_discount` 折扣后，与本地评分取最大值（不覆盖本地观测）。
    /// 此方法不修改 `peer_self_score`（它应由直接通信时的自评更新）。
    pub fn inject_neighbor_score(
        &mut self,
        peer: &PeerId,
        addr: &Multiaddr,
        neighbor_score: f64,
        path_type: PathType,
    ) {
        let neighbor_discount = self.config.neighbor_discount;
        let entry = self.get_or_insert_entry(peer, addr, 500.0, self.config.max_addrs_per_peer);
        let discounted = neighbor_score * neighbor_discount;
        entry.score = entry.score.max(discounted).min(1.0);
        entry.path_type = path_type;
        entry.last_updated = Instant::now();
    }

    /// 更新某 peer 某地址的远程自评分数。
    /// 在收到该 peer 的直接评分响应时调用，存储其声明的负载等级。
    pub fn set_peer_self_score(&mut self, peer: &PeerId, addr: &Multiaddr, peer_self_score: f64) {
        let entry = self.get_or_insert_entry(peer, addr, 500.0, self.config.max_addrs_per_peer);
        entry.peer_self_score = peer_self_score.clamp(0.0, 1.0);
    }

    /// 获取某 peer 当前综合评分最高的地址。
    pub fn best_addr(&self, peer: &PeerId) -> Option<Multiaddr> {
        self.scores
            .get(peer)
            .and_then(|map| {
                map.iter().max_by(|a, b| {
                    a.1.score
                        .partial_cmp(&b.1.score)
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
            })
            .map(|(addr, _)| addr.clone())
    }

    /// 获取或创建某 peer 某地址的评分条目。
    /// 如果该 peer 的地址数达到 `max_addrs`，淘汰评分最低的地址。
    fn get_or_insert_entry(
        &mut self,
        peer: &PeerId,
        addr: &Multiaddr,
        latency_ms: f64,
        max_addrs: usize,
    ) -> &mut ScoreEntry {
        let map = self.get_or_insert(peer);
        if map.len() >= max_addrs
            && let Some(lowest) = map
                .iter()
                .min_by(|a, b| {
                    a.1.score
                        .partial_cmp(&b.1.score)
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .map(|(k, _)| k.clone())
        {
            map.remove(&lowest);
        }
        map.entry(addr.clone()).or_insert_with(|| {
            let mut entry = ScoreEntry::new(PathType::from_multiaddr(addr));
            entry.last_updated = Instant::now();
            entry.latency_ewma = latency_ms;
            entry
        })
    }
}

/// 按权重数组随机选择一个索引。权重不需要归一化。
fn weighted_random(weights: &[f64]) -> usize {
    let total: f64 = weights.iter().sum();
    if total == 0.0 {
        return 0;
    }
    let mut r = rand::random::<f64>() * total;
    for (i, w) in weights.iter().enumerate() {
        r -= w;
        if r <= 0.0 {
            return i;
        }
    }
    weights.len() - 1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rank_empty() {
        let ranker = BehaviorRanker::with_default();
        let peer = PeerId::random();
        assert!(ranker.rank(&peer, vec![]).is_empty());
    }

    #[test]
    fn test_feedback_updates_score() {
        let mut ranker = BehaviorRanker::with_default();
        let peer = PeerId::random();
        let addr: Multiaddr = "/ip4/127.0.0.1/tcp/8080".parse().unwrap();
        ranker.feedback(&peer, &addr, Duration::from_millis(50), true);
        assert!(ranker.get_entry(&peer, &addr).unwrap().score > 0.5);
    }

    #[test]
    fn test_rank_orders_by_score() {
        let mut ranker = BehaviorRanker::new(RankerConfig {
            diversity_top_k: 1,
            ..Default::default()
        });
        let peer = PeerId::random();
        let a: Multiaddr = "/ip4/10.0.0.1/tcp/8080".parse().unwrap();
        let b: Multiaddr = "/ip4/10.0.0.2/tcp/8080".parse().unwrap();
        ranker.feedback(&peer, &a, Duration::from_millis(10), true);
        ranker.feedback(&peer, &b, Duration::from_millis(500), false);
        let ranked = ranker.rank(&peer, vec![a.clone(), b.clone()]);
        assert_eq!(ranked[0], a, "higher score should be first");
    }

    #[test]
    fn test_self_score_fusion() {
        let mut ranker = BehaviorRanker::with_default();
        let peer = PeerId::random();
        let addr: Multiaddr = "/ip4/127.0.0.1/tcp/8080".parse().unwrap();
        ranker.feedback(&peer, &addr, Duration::from_millis(50), true);
        ranker.set_peer_self_score(&peer, &addr, 0.3);
        let ranked = ranker.rank(&peer, vec![addr]);
        assert_eq!(ranked.len(), 1, "address should still be ranked");
    }

    #[test]
    fn test_credit_threshold_filter() {
        let config = RankerConfig {
            credit_threshold: 60.0,
            ..Default::default()
        };
        let mut ranker = BehaviorRanker::new(config);
        let peer = PeerId::random();
        let addr: Multiaddr = "/ip4/127.0.0.1/tcp/8080".parse().unwrap();
        ranker.feedback(&peer, &addr, Duration::from_millis(50), true);
        let ranked = ranker.rank(&peer, vec![addr]);
        assert_eq!(ranked.len(), 1, "credit reset should include the address");
    }

    #[test]
    fn test_alert_level_dynamic_params() {
        let config = RankerConfig::default();
        assert_eq!(config.credit_threshold_at(AlertLevel::Shelter), 75.0);
        assert_eq!(config.credit_threshold_at(AlertLevel::Normal), 30.0);
        assert!(
            config.exploration_budget_at(AlertLevel::Defensive)
                < config.exploration_budget_at(AlertLevel::Normal)
        );
    }

    #[test]
    fn test_feedback_real_latency_updates_ewma() {
        let mut ranker = BehaviorRanker::with_default();
        let peer = PeerId::random();
        let addr: Multiaddr = "/ip4/127.0.0.1/tcp/8080".parse().unwrap();
        ranker.feedback(&peer, &addr, Duration::from_millis(100), true);
        let entry = ranker.get_entry(&peer, &addr).unwrap();
        assert!(entry.latency_ewma < 500.0, "latency EWMA should move from initial 500ms");
    }

    #[test]
    fn test_failure_punishes_the_real_addr() {
        let mut ranker = BehaviorRanker::with_default();
        let peer = PeerId::random();
        let a: Multiaddr = "/ip4/10.0.0.1/tcp/8080".parse().unwrap();
        let b: Multiaddr = "/ip4/10.0.0.2/tcp/8080".parse().unwrap();
        // 地址 a 成功建立过一次，地址 b 一直失败
        ranker.feedback(&peer, &a, Duration::from_millis(50), true);
        for _ in 0..4 {
            ranker.feedback(&peer, &b, Duration::from_millis(800), false);
        }
        let ranked = ranker.rank(&peer, vec![a.clone(), b.clone()]);
        assert_eq!(ranked[0], a, "healthy addr should outrank the failing one");
        let credit_b = ranker.get_entry(&peer, &b).unwrap().credit;
        assert!(credit_b < 50.0, "failing addr credit should drop: {credit_b}");
    }
}
