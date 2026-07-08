use std::collections::{HashMap, HashSet};

use futures::StreamExt;
use libp2p::{
    Multiaddr, PeerId, Swarm, identity, noise, swarm::SwarmEvent, yamux, Transport, tcp,
    request_response::{self, ResponseChannel, OutboundRequestId},
    core::transport::{MemoryTransport, TransportError},
    core::upgrade,
};
use crate::ranker::behavior::{BehaviorRanker, RankerConfig, PathType, AlertLevel};
use crate::ScoreResponse;
use crate::PathRankerBehaviour;
use tracing;

/// 警戒态势评估器：监控连接速率、突变率、失败率，输出警戒等级。
///
/// 在 SmartNode 事件循环中周期性调用 `evaluate()`，结果通过
/// `ranker.set_alert_level()` 注入 BehaviorRanker 以动态调节防御强度。
///
/// 等级转换采用滞后（hysteresis）：升级立即生效，降级需低于阈值 30 秒，
/// 防止频繁震荡。
struct AlertEvaluator {
    /// 连接速率 EWMA（次/秒），基于实际时间间隔计算
    conn_rate_ewma: f64,
    /// 基线连接速率（正常时的平均值）
    baseline_conn_rate: f64,
    /// 最近 mutation 计数
    mutation_count: usize,
    /// 最近失败计数
    recent_failures: usize,
    /// 历史基线采样次数
    baseline_samples: usize,
    /// 当前等级维持时间，用于降级滞后
    current_level_since: std::time::Instant,
    current_level: AlertLevel,
    /// 上次事件时间，用于计算瞬时速率
    last_event_time: std::time::Instant,
    /// 当前时间窗口内的事件计数
    events_in_window: u64,
}

impl AlertEvaluator {
    fn new() -> Self {
        Self {
            conn_rate_ewma: 0.0,
            baseline_conn_rate: 0.0,
            mutation_count: 0,
            recent_failures: 0,
            baseline_samples: 0,
            current_level_since: std::time::Instant::now(),
            current_level: AlertLevel::Normal,
            last_event_time: std::time::Instant::now(),
            events_in_window: 0,
        }
    }

    /// 记录一次连接事件（建立或关闭），基于时间间隔计算瞬时速率。
    fn record_connection_event(&mut self) {
        self.events_in_window += 1;
        let now = std::time::Instant::now();
        let elapsed = now.duration_since(self.last_event_time).as_secs_f64().max(0.001);
        let instant_rate = self.events_in_window as f64 / elapsed;
        let alpha = 0.3;
        self.conn_rate_ewma = alpha * instant_rate + (1.0 - alpha) * self.conn_rate_ewma;
        self.last_event_time = now;
        self.events_in_window = 0;
    }

    /// 记录一次信用突变事件。
    #[allow(dead_code)]
    fn record_mutation(&mut self) {
        self.mutation_count += 1;
    }

    /// 记录一次通信失败。
    #[allow(dead_code)]
    fn record_failure(&mut self) {
        self.recent_failures += 1;
    }

    /// 评估当前警戒等级。
    /// 升级立即生效，降级需要当前等级维持超过 30 秒。
    fn evaluate(&mut self) -> AlertLevel {
        // 更新基线（前 100 次采样为正常基线）
        if self.baseline_samples < 100 {
            self.baseline_conn_rate = (self.baseline_conn_rate * self.baseline_samples as f64 + self.conn_rate_ewma)
                / (self.baseline_samples as f64 + 1.0);
            self.baseline_samples += 1;
        }

        let conn_ratio = if self.baseline_conn_rate > 0.01 {
            self.conn_rate_ewma / self.baseline_conn_rate
        } else {
            1.0
        };

        let alert_score =
            (conn_ratio - 1.0).max(0.0) * 3.0
            + (self.mutation_count as f64).min(5.0) * 0.5
            + self.recent_failures as f64 * 0.2;

        let new_level = AlertLevel::from_score(alert_score);

        let now = std::time::Instant::now();
        if new_level > self.current_level {
            // 升级：立即生效
            self.current_level = new_level;
            self.current_level_since = now;
        } else if new_level < self.current_level {
            // 降级：需要滞后 30 秒
            if now.duration_since(self.current_level_since) > std::time::Duration::from_secs(30) {
                self.current_level = new_level;
                self.current_level_since = now;
            }
        } else {
            self.current_level_since = now;
        }

        // 指数衰减，防止历史数据持续影响
        self.mutation_count = (self.mutation_count as f64 * 0.9) as usize;
        self.recent_failures = (self.recent_failures as f64 * 0.9) as usize;

        self.current_level
    }
}

/// 智能节点配置。
#[derive(Debug, Clone)]
pub struct SmartNodeConfig {
    /// 冷启动时向邻居查询评分的数量上限
    pub cold_start_queries: usize,
    /// 路径自动升级检查间隔（如中继→直连）
    pub upgrade_check_interval: std::time::Duration,
    /// 每 peer 最大并发流数，超过后 `dial_best` 跳过该 peer（拥塞控制）
    pub max_streams_per_peer: usize,
}

impl Default for SmartNodeConfig {
    fn default() -> Self {
        Self {
            cold_start_queries: 3,
            upgrade_check_interval: std::time::Duration::from_secs(60),
            max_streams_per_peer: 5,
        }
    }
}

/// 智能节点：包装 `Swarm<PathRankerBehaviour>` + `BehaviorRanker`
/// 提供自动拨号、事件驱动反馈、负载感知、并发流控制、
/// 协作评分查询/注入、签名验签和防重放。
///
/// # 核心流程
///
/// 1. `dial_best(peer, addrs)` → ranker 排序 → 按序尝试拨号 → 成功/失败自动 feedback
/// 2. `handle_swarm_event()` → 自动提取 ConnectionEstablished/Closed 反馈 + 协议事件
/// 3. 协议事件：收到查询 → 查询 ranker → 签名响应；收到响应 → 验签 → 注入邻居评分
/// 4. 过载防护：`active_streams` 限制每 peer 并发数；`ranker.self_score` 通过响应告知对方
pub struct SmartNode {
    /// libp2p Swarm，持有 PathRankerBehaviour
    pub swarm: Swarm<PathRankerBehaviour>,
    pub ranker: BehaviorRanker,
    config: SmartNodeConfig,
    /// 已发起但未收到响应的查询（防重复发送）
    pending_queries: HashSet<(PeerId, String)>,
    /// 请求 ID → (目标 peer, target 字符串)
    pending_targets: HashMap<OutboundRequestId, (PeerId, String)>,
    /// 每 peer 最近 nonce 缓存，用于防重放（LRU 1024/peer）
    recent_nonces: HashMap<PeerId, lru::LruCache<u64, ()>>,
    /// 每 peer 活跃流计数，超限时跳过拨号
    active_streams: HashMap<PeerId, usize>,
    /// 已发起但未完成的拨号（防止并发拨号 inflate active_streams）
    dialing_peers: HashSet<PeerId>,
    /// 警戒态势评估器
    alert_evaluator: AlertEvaluator,
    /// 上次警戒评估时间（每 1 秒最多评估一次）
    last_alert_tick: std::time::Instant,
}

impl SmartNode {
    /// 内部构造器，消除重复初始化逻辑。
    fn from_swarm(swarm: Swarm<PathRankerBehaviour>, config: SmartNodeConfig) -> Self {
        Self {
            swarm,
            ranker: BehaviorRanker::new(RankerConfig::default()),
            config,
            pending_queries: HashSet::new(),
            pending_targets: HashMap::new(),
            recent_nonces: HashMap::new(),
            active_streams: HashMap::new(),
            dialing_peers: HashSet::new(),
            alert_evaluator: AlertEvaluator::new(),
            last_alert_tick: std::time::Instant::now(),
        }
    }

    /// 测试用：MemoryTransport + Noise + Yamux
    pub async fn new_test() -> Self {
        let config = SmartNodeConfig::default();
        let local_key = identity::Keypair::generate_ed25519();
        let noise = noise::Config::new(&local_key).unwrap();
        let transport = MemoryTransport::default()
            .upgrade(upgrade::Version::V1)
            .authenticate(noise)
            .multiplex(yamux::Config::default())
            .boxed();

        let swarm = libp2p::SwarmBuilder::with_existing_identity(local_key)
            .with_tokio()
            .with_other_transport(|_| transport)
            .unwrap()
            .with_behaviour(|keypair| PathRankerBehaviour::new(keypair.clone()))
            .unwrap()
            .build();

        Self::from_swarm(swarm, config)
    }

    /// 生产用：TCP + DNS + Noise + Yamux，匹配 openwire_core 模式。
    pub async fn new_tcp(config: SmartNodeConfig) -> Self {
        let local_key = identity::Keypair::generate_ed25519();
        let swarm = libp2p::SwarmBuilder::with_existing_identity(local_key)
            .with_tokio()
            .with_tcp(
                tcp::Config::default(),
                noise::Config::new,
                yamux::Config::default,
            )
            .unwrap()
            .with_dns()
            .unwrap()
            .with_behaviour(|keypair| PathRankerBehaviour::new(keypair.clone()))
            .unwrap()
            .with_swarm_config(|cfg| cfg.with_max_negotiating_inbound_streams(1000))
            .build();
        Self::from_swarm(swarm, config)
    }

    /// 监听指定地址。
    pub fn listen(&mut self, addr: Multiaddr) -> Result<(), TransportError<std::io::Error>> {
        self.swarm.listen_on(addr).map(|_| ())
    }

    /// 返回监听地址列表。
    pub fn listeners(&self) -> impl Iterator<Item = &Multiaddr> {
        self.swarm.listeners()
    }

    /// 运行事件循环，自动处理 Swarm 事件和警戒评估。
    /// 当 `shutdown` 收到信号时退出循环。
    /// 保留 `poll_next_event()` 给需要自定义循环的用户。
    pub async fn run(&mut self, mut shutdown: tokio::sync::watch::Receiver<bool>) {
        loop {
            tokio::select! {
                event = self.swarm.select_next_some() => {
                    self.handle_swarm_event(event);
                }
                _ = shutdown.changed() => {
                    tracing::info!("shutdown signal received, stopping event loop");
                    break;
                }
            }
        }
    }

    /// 限频警戒评估：每秒最多评估一次，防止 decay 加速。
    fn maybe_tick_alert(&mut self) {
        let now = std::time::Instant::now();
        if now.duration_since(self.last_alert_tick) < std::time::Duration::from_secs(1) {
            return;
        }
        self.last_alert_tick = now;
        let new_level = self.alert_evaluator.evaluate();
        if new_level != self.ranker.alert_level() {
            tracing::info!(
                "alert level changed: {:?} → {:?}",
                self.ranker.alert_level(),
                new_level,
            );
            self.ranker.set_alert_level(new_level);
        }
    }

    /// 智能拨号：通过 ranker 对候选地址排序，按评分从高到低依次尝试。
    ///
    /// 并发控制：如果该 peer 的活跃流数已达 `max_streams_per_peer`，跳过拨号。
    /// 每次拨号前递增 `active_streams`，成功或失败后递减。
    /// 拨号失败自动调用 `ranker.feedback()` 记录。
    pub fn dial_best(&mut self, peer: PeerId, addrs: Vec<Multiaddr>) {
        if addrs.is_empty() {
            return;
        }

        let stream_count = self.active_streams.get(&peer).copied().unwrap_or(0);
        if stream_count >= self.config.max_streams_per_peer {
            tracing::debug!("skip dial {}: active streams {} >= max {}", peer, stream_count, self.config.max_streams_per_peer);
            return;
        }

        if self.dialing_peers.contains(&peer) {
            tracing::debug!("skip dial {}: dial already in progress", peer);
            return;
        }

        let ranked = self.ranker.rank(&peer, addrs);

        for addr in ranked {
            if self.swarm.is_connected(&peer) {
                self.dialing_peers.remove(&peer);
                break;
            }
            if self.dialing_peers.contains(&peer) {
                // 另一地址的拨号已发起，不再尝试后续地址
                break;
            }
            self.dialing_peers.insert(peer);
            *self.active_streams.entry(peer).or_insert(0) += 1;
            if let Err(e) = self.swarm.dial(addr.clone()) {
                tracing::warn!("dial {} -> {} failed: {:?}", peer, addr, e);
                self.ranker.feedback(&peer, &addr, std::time::Duration::from_millis(0), false);
                self.dialing_peers.remove(&peer);
                if let Some(count) = self.active_streams.get_mut(&peer) {
                    *count = count.saturating_sub(1);
                    if *count == 0 {
                        self.active_streams.remove(&peer);
                    }
                }
            }
        }
    }

    /// 向所有已连接节点广播评分查询（去重，已发起的跳过）。
    /// 自动生成随机 nonce 并记录 `pending_targets`，用于响应时匹配目标。
    pub fn query_neighbor_scores(&mut self, target: PeerId) {
        let target_str = target.to_string();
        for peer in self.connected_peers() {
            if self.pending_queries.contains(&(peer, target_str.clone())) {
                continue;
            }
            let request_id = self
                .swarm
                .behaviour_mut()
                .send_query(&peer, target_str.clone());
            self.pending_queries.insert((peer, target_str.clone()));
            self.pending_targets.insert(request_id, (peer, target_str.clone()));
        }
    }

    /// 处理 incoming 评分查询。
    ///
    /// 1. 校验 `target` 是否能解析为 PeerId，失败则返回 0 分响应
    /// 2. 查询 ranker 获取目标最佳地址的评分
    /// 3. 填充 `self_score`（当前本地负载）和 `overloaded` 标记
    /// 4. 交由 `PathRankerBehaviour::send_response` 自动签名后发送
    pub fn handle_score_request(
        &mut self,
        channel: ResponseChannel<ScoreResponse>,
        request: crate::ScoreRequest,
    ) {
        let target_peer = match request.target.parse::<PeerId>() {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!("received ScoreRequest with invalid target '{}': {:?}", request.target, e);
                let resp = crate::ScoreResponse {
                    score: 0.0,
                    updated_at: 0,
                    nonce: request.nonce,
                    target: request.target,
                    signature: Vec::new(),
                    responder_key: Vec::new(),
                    overloaded: false,
                    self_score: self.ranker.self_score(),
                };
                let _ = self.swarm.behaviour_mut().send_response(channel, resp);
                return;
            }
        };

        let best = self.ranker.best_addr(&target_peer);
        let (score, updated_at) = match best {
            Some(addr) => {
                let entry = self.ranker.get_entry(&target_peer, &addr);
                let score = entry.map(|e| e.score).unwrap_or(0.0);
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                (score, now)
            }
            None => (0.0, 0),
        };

        let overloaded = self.ranker.is_overloaded();

        let resp = crate::ScoreResponse {
            score,
            updated_at,
            nonce: request.nonce,
            target: request.target,
            signature: Vec::new(),
            responder_key: Vec::new(),
            overloaded,
            self_score: self.ranker.self_score(),
        };
        let _ = self
            .swarm
            .behaviour_mut()
            .send_response(channel, resp);
    }

    /// 处理评分响应。
    ///
    /// 安全校验链（按顺序，任一失败则丢弃）：
    /// 1. nonce 防重放（LRU 缓存 + 60s 时间窗口）
    /// 2. 评分范围检查 [0,1]
    /// 3. Ed25519 签名验证
    ///
    /// 通过后：
    /// - 如果对方标记 `overloaded=true`，强制标记该路径失败
    /// - 调用 `ranker.inject_neighbor_score` 注入评分和远程自评
    pub fn handle_score_response(
        &mut self,
        responder: PeerId,
        request_id: OutboundRequestId,
        response: ScoreResponse,
    ) {
        if let Some((peer, target)) = self.pending_targets.remove(&request_id) {
            self.pending_queries.remove(&(peer, target));
        }

        let nonce_cache = self.recent_nonces.entry(responder).or_insert_with(|| lru::LruCache::new(std::num::NonZeroUsize::new(1024).unwrap()));
        if nonce_cache.contains(&response.nonce) {
            tracing::warn!("replayed nonce {} from peer {}", response.nonce, responder);
            return;
        }

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        if now > response.updated_at && now - response.updated_at > 60 {
            tracing::warn!("stale timestamp {} from peer {}", response.updated_at, responder);
            return;
        }
        if response.updated_at > now + 5 {
            tracing::warn!("future timestamp {} from peer {}", response.updated_at, responder);
            return;
        }

        if !(0.0..=1.0).contains(&response.score) {
            tracing::warn!("invalid score {} from peer {}", response.score, responder);
            return;
        }

        if !PathRankerBehaviour::verify_response(&response) {
            tracing::warn!("bad signature from peer {}", responder);
            return;
        }

        nonce_cache.put(response.nonce, ());

        tracing::info!(
            "verified score from {} for target {}: {:.2} self_score={:.2} overloaded={}",
            responder,
            response.target,
            response.score,
            response.self_score,
            response.overloaded,
        );

        if response.overloaded {
            tracing::warn!("peer {} reports overloaded, marking path as failed", responder);
            if let Some(addr) = self.ranker.best_addr(&responder) {
                self.ranker.feedback(
                    &responder,
                    &addr,
                    std::time::Duration::from_millis(1000),
                    false,
                );
            }
        }

        if let Ok(target_peer) = response.target.parse::<PeerId>() {
            if let Some(addr) = self.ranker.best_addr(&target_peer) {
                let path_type = PathType::from_multiaddr(&addr);
                self.ranker.inject_neighbor_score(
                    &target_peer,
                    &addr,
                    response.score,
                    path_type,
                );
            }
        }

        // 将 responder 的自评分数存到 responder 的地址条目上
        if let Some(addr) = self.ranker.best_addr(&responder) {
            self.ranker.set_peer_self_score(&responder, &addr, response.self_score);
        }
    }

    /// 处理 Swarm 事件，自动提取连接生命周期反馈和协议事件。
    ///
    /// - `ConnectionEstablished`：记录日志、通知 evaluator
    /// - `ConnectionClosed`：清理该 peer 的所有 pending 记录，递减 active_streams，通知 evaluator
    /// - `Behaviour(event)`：交由 `handle_pathranker_event` 处理
    pub fn handle_swarm_event(&mut self, event: SwarmEvent<<PathRankerBehaviour as libp2p::swarm::NetworkBehaviour>::ToSwarm>) {
        match event {
            SwarmEvent::ConnectionEstablished { peer_id, .. } => {
                tracing::info!("connected to {}", peer_id);
                self.dialing_peers.remove(&peer_id);
                self.alert_evaluator.record_connection_event();
                self.maybe_tick_alert();
            }
            SwarmEvent::ConnectionClosed { peer_id, .. } => {
                tracing::info!("disconnected from {}", peer_id);
                self.dialing_peers.remove(&peer_id);
                self.alert_evaluator.record_connection_event();
                self.maybe_tick_alert();
                self.pending_queries.retain(|(p, _)| p != &peer_id);
                self.pending_targets.retain(|_, (p, _)| p != &peer_id);
                if let Some(count) = self.active_streams.get_mut(&peer_id) {
                    *count = count.saturating_sub(1);
                    if *count == 0 {
                        self.active_streams.remove(&peer_id);
                    }
                }
            }
            SwarmEvent::Behaviour(event) => {
                self.handle_pathranker_event(event);
            }
            _ => {}
        }
    }

    /// 周期性评估警戒等级并注入 ranker。
    /// 推荐在事件循环的每个 tick 末尾调用（约每秒 1 次）。
    pub fn tick_alert(&mut self) {
        let new_level = self.alert_evaluator.evaluate();
        if new_level != self.ranker.alert_level() {
            tracing::info!(
                "alert level changed: {:?} → {:?}",
                self.ranker.alert_level(),
                new_level,
            );
            self.ranker.set_alert_level(new_level);
        }
    }

    pub fn poll_next_event(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<SwarmEvent<<PathRankerBehaviour as libp2p::swarm::NetworkBehaviour>::ToSwarm>>> {
        self.swarm.poll_next_unpin(cx)
    }

    /// 处理 PathRanker 协议事件。
    ///
    /// - `Message::Request` → 处理评分查询
    /// - `Message::Response` → 处理评分响应
    /// - `OutboundFailure` → 清理 pending 记录
    /// - `InboundFailure` → 记录日志
    fn handle_pathranker_event(&mut self, event: crate::PathRankerEvent) {
        match event {
            crate::PathRankerEvent::Message { peer, message, .. } => {
                match message {
                    request_response::Message::Request { request, channel, .. } => {
                        self.handle_score_request(channel, request);
                    }
                    request_response::Message::Response { request_id, response, .. } => {
                        self.handle_score_response(peer, request_id, response);
                    }
                }
            }
            crate::PathRankerEvent::OutboundFailure { peer, request_id, error, .. } => {
                tracing::warn!("outbound failure to {} (req={:?}): {:?}", peer, request_id, error);
                if let Some((_, target)) = self.pending_targets.remove(&request_id) {
                    self.pending_queries.remove(&(peer, target));
                }
            }
            crate::PathRankerEvent::InboundFailure { peer, request_id, error, .. } => {
                tracing::warn!("inbound failure from {} (req={:?}): {:?}", peer, request_id, error);
            }
            _ => {}
        }
    }

    fn connected_peers(&self) -> Vec<PeerId> {
        self.swarm.connected_peers().cloned().collect()
    }
}