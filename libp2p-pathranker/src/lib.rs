use std::collections::HashMap;
use std::time::{Duration, Instant};

use libp2p::{
    Multiaddr, PeerId, StreamProtocol,
    request_response::{self, OutboundRequestId, ProtocolSupport, ResponseChannel, cbor},
    swarm::{FromSwarm, NetworkBehaviour},
};
use serde::{Deserialize, Serialize};

pub mod ranker;

use ranker::behavior::{AlertLevel, BehaviorRanker, RankerConfig};

pub const PROTOCOL_NAME: StreamProtocol = StreamProtocol::new("/openwire/pathranker/1.0");

const REQUEST_SIZE_MAX: u64 = 256 * 1024;
const RESPONSE_SIZE_MAX: u64 = 256 * 1024;
const REQUEST_TIMEOUT_SECS: u64 = 10;
const MAX_CONCURRENT_STREAMS: usize = 10;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoreRequest {
    pub target: String,
    pub nonce: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoreResponse {
    pub score: f64,
    pub updated_at: u64,
    pub nonce: u64,
    pub target: String,
    pub signature: Vec<u8>,
    pub responder_key: Vec<u8>,
    pub overloaded: bool,
    pub self_score: f64,
}

pub type PathRankerEvent = request_response::Event<ScoreRequest, ScoreResponse>;

/// 警戒态势评估器：监控连接速率、突变率、失败率，输出警戒等级
struct AlertEvaluator {
    conn_rate_ewma: f64,
    baseline_conn_rate: f64,
    mutation_count: usize,
    recent_failures: usize,
    baseline_samples: usize,
    current_level_since: std::time::Instant,
    current_level: AlertLevel,
    last_event_time: std::time::Instant,
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
        }
    }

    fn record_connection_event(&mut self) {
        let now = std::time::Instant::now();
        let elapsed = now
            .duration_since(self.last_event_time)
            .as_secs_f64()
            .max(0.001);
        let instant_rate = 1.0 / elapsed;
        self.conn_rate_ewma = 0.3 * instant_rate + 0.7 * self.conn_rate_ewma;
        self.last_event_time = now;
    }

    fn record_mutation(&mut self) {
        self.mutation_count += 1;
    }

    fn record_failure(&mut self) {
        self.recent_failures += 1;
    }

    fn evaluate(&mut self) -> AlertLevel {
        if self.baseline_samples < 100 {
            self.baseline_conn_rate = (self.baseline_conn_rate * self.baseline_samples as f64
                + self.conn_rate_ewma)
                / (self.baseline_samples as f64 + 1.0);
            self.baseline_samples += 1;
        }
        let conn_ratio = if self.baseline_conn_rate > 0.01 {
            self.conn_rate_ewma / self.baseline_conn_rate
        } else {
            1.0
        };
        let alert_score = (conn_ratio - 1.0).max(0.0) * 3.0
            + (self.mutation_count as f64).min(5.0) * 0.5
            + self.recent_failures as f64 * 0.2;
        let new_level = AlertLevel::from_score(alert_score);
        let now = std::time::Instant::now();
        if new_level > self.current_level {
            self.current_level = new_level;
            self.current_level_since = now;
        } else if new_level < self.current_level {
            if now.duration_since(self.current_level_since) > Duration::from_secs(30) {
                self.current_level = new_level;
                self.current_level_since = now;
            }
        } else {
            self.current_level_since = now;
        }
        self.mutation_count = (self.mutation_count as f64 * 0.9) as usize;
        self.recent_failures = (self.recent_failures as f64 * 0.9) as usize;
        self.current_level
    }
}

/// `/openwire/pathranker/1.0` 协议的网络行为实现 + 行为感知评分引擎。
///
/// 协议事件（收到评分查询/响应）由 `P2pActor` 通过 `PathRankerEvent` 处理，
/// 连接生命周期反馈由 `on_swarm_event` 自动采集更新评分。
pub struct PathRankerBehaviour {
    inner: cbor::Behaviour<ScoreRequest, ScoreResponse>,
    local_key: libp2p::identity::Keypair,
    pub ranker: BehaviorRanker,
    alert_evaluator: AlertEvaluator,
    last_alert_tick: std::time::Instant,
    /// 进行中的出站拨号：PeerId → (拨号地址, 开始时间)。
    /// 用于在成功时测量真实 RTT、在失败时把惩罚归因到真实地址，
    /// 而非仅用 Multiaddr::empty() 占位（后者使失败永远不会作用于候选地址）。
    pending_dials: HashMap<PeerId, (Multiaddr, Instant)>,
}

impl PathRankerBehaviour {
    pub fn new(local_key: libp2p::identity::Keypair) -> Self {
        let codec = cbor::codec::Codec::<ScoreRequest, ScoreResponse>::default()
            .set_request_size_maximum(REQUEST_SIZE_MAX)
            .set_response_size_maximum(RESPONSE_SIZE_MAX);
        let rr = cbor::Behaviour::with_codec(
            codec,
            [(PROTOCOL_NAME, ProtocolSupport::Full)],
            request_response::Config::default()
                .with_request_timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS))
                .with_max_concurrent_streams(MAX_CONCURRENT_STREAMS),
        );
        Self {
            inner: rr,
            local_key,
            ranker: BehaviorRanker::new(RankerConfig::default()),
            alert_evaluator: AlertEvaluator::new(),
            last_alert_tick: std::time::Instant::now(),
            pending_dials: HashMap::new(),
        }
    }

    pub fn local_peer_id(&self) -> PeerId {
        self.local_key.public().to_peer_id()
    }

    /// 记录一次出站拨号尝试（目标地址 + 开始时间）。
    ///
    /// 由上层在每次实际发起拨号前调用（含多次地址的逐个尝试）：
    /// - 连接建立时用其测量真实 RTT 并 feedback 到目标地址；
    /// - 拨号失败时把失败惩罚归因到真实地址（而非空地址占位）。
    ///
    /// 同一 PeerId 的多次尝试会覆盖为最后一次（最后一次尝试决定成败归属）。
    pub fn note_dial_started(&mut self, peer: &PeerId, addr: &Multiaddr) {
        self.pending_dials
            .insert(*peer, (addr.clone(), Instant::now()));
    }

    // ====== 协议方法 ======

    #[allow(dead_code)]
    pub fn send_query(&mut self, peer: &PeerId, target: String) -> OutboundRequestId {
        self.inner.send_request(
            peer,
            ScoreRequest {
                target,
                nonce: rand::random(),
            },
        )
    }

    pub fn send_response(
        &mut self,
        channel: ResponseChannel<ScoreResponse>,
        mut resp: ScoreResponse,
    ) -> Result<(), ScoreResponse> {
        let public_key = self.local_key.public();
        resp.responder_key = public_key.encode_protobuf();
        let responder_id = public_key.to_peer_id();
        let payload = Self::build_signing_payload(
            &responder_id,
            resp.score,
            resp.updated_at,
            &resp.target,
            resp.nonce,
            resp.self_score,
        );
        match self.local_key.sign(&payload) {
            Ok(sig) => {
                resp.signature = sig;
                self.inner.send_response(channel, resp)
            }
            Err(e) => {
                tracing::error!("failed to sign ScoreResponse: {:?}", e);
                Err(resp)
            }
        }
    }

    #[allow(dead_code)]
    pub fn verify_response(resp: &ScoreResponse) -> bool {
        let pk = match libp2p::identity::PublicKey::try_decode_protobuf(&resp.responder_key) {
            Ok(pk) => pk,
            Err(_) => return false,
        };
        let responder_id = pk.to_peer_id();
        let payload = Self::build_signing_payload(
            &responder_id,
            resp.score,
            resp.updated_at,
            &resp.target,
            resp.nonce,
            resp.self_score,
        );
        pk.verify(&payload, &resp.signature)
    }

    fn build_signing_payload(
        responder: &PeerId,
        score: f64,
        updated_at: u64,
        target: &str,
        nonce: u64,
        self_score: f64,
    ) -> Vec<u8> {
        let mut data = Vec::new();
        let responder_bytes = responder.to_bytes();
        data.extend_from_slice(&(responder_bytes.len() as u32).to_be_bytes());
        data.extend_from_slice(&responder_bytes);
        data.extend_from_slice(&score.to_be_bytes());
        data.extend_from_slice(&updated_at.to_be_bytes());
        data.extend_from_slice(&(target.len() as u32).to_be_bytes());
        data.extend_from_slice(target.as_bytes());
        data.extend_from_slice(&nonce.to_be_bytes());
        data.extend_from_slice(&self_score.to_be_bytes());
        data
    }

    // ====== 评分引擎代理方法 ======

    pub fn rank(&self, peer: &PeerId, addrs: Vec<Multiaddr>) -> Vec<Multiaddr> {
        self.ranker.rank(peer, addrs)
    }

    pub fn feedback(&mut self, peer: &PeerId, addr: &Multiaddr, latency: Duration, success: bool) {
        self.ranker.feedback(peer, addr, latency, success);
    }

    pub fn best_addr(&self, peer: &PeerId) -> Option<Multiaddr> {
        self.ranker.best_addr(peer)
    }

    pub fn set_alert_level(&mut self, level: AlertLevel) {
        self.ranker.set_alert_level(level);
    }

    pub fn alert_level(&self) -> AlertLevel {
        self.ranker.alert_level()
    }

    pub fn consume_mutation_flag(&mut self) -> bool {
        self.ranker.consume_mutation_flag()
    }

    pub fn inject_neighbor_score(
        &mut self,
        peer: &PeerId,
        addr: &Multiaddr,
        score: f64,
        path_type: ranker::behavior::PathType,
    ) {
        self.ranker
            .inject_neighbor_score(peer, addr, score, path_type);
    }

    pub fn set_peer_self_score(&mut self, peer: &PeerId, addr: &Multiaddr, score: f64) {
        self.ranker.set_peer_self_score(peer, addr, score);
    }

    pub fn is_overloaded(&self) -> bool {
        self.ranker.is_overloaded()
    }

    /// 限频警戒评估：每秒最多一次
    fn maybe_tick_alert(&mut self) {
        let now = std::time::Instant::now();
        if now.duration_since(self.last_alert_tick) < Duration::from_secs(1) {
            return;
        }
        self.last_alert_tick = now;
        let new_level = self.alert_evaluator.evaluate();
        if new_level != self.ranker.alert_level() {
            tracing::info!(
                "alert level changed: {:?} → {:?}",
                self.ranker.alert_level(),
                new_level
            );
            self.ranker.set_alert_level(new_level);
        }
    }
}

impl NetworkBehaviour for PathRankerBehaviour {
    type ConnectionHandler =
        <cbor::Behaviour<ScoreRequest, ScoreResponse> as NetworkBehaviour>::ConnectionHandler;
    type ToSwarm = request_response::Event<ScoreRequest, ScoreResponse>;

    fn handle_established_inbound_connection(
        &mut self,
        connection_id: libp2p::swarm::ConnectionId,
        peer: PeerId,
        local_addr: &Multiaddr,
        remote_addr: &Multiaddr,
    ) -> Result<Self::ConnectionHandler, libp2p::swarm::ConnectionDenied> {
        self.inner.handle_established_inbound_connection(
            connection_id,
            peer,
            local_addr,
            remote_addr,
        )
    }

    fn handle_established_outbound_connection(
        &mut self,
        connection_id: libp2p::swarm::ConnectionId,
        peer: PeerId,
        addr: &Multiaddr,
        role_override: libp2p::core::Endpoint,
        port_use: libp2p::core::transport::PortUse,
    ) -> Result<Self::ConnectionHandler, libp2p::swarm::ConnectionDenied> {
        self.inner.handle_established_outbound_connection(
            connection_id,
            peer,
            addr,
            role_override,
            port_use,
        )
    }

    fn on_swarm_event(&mut self, event: FromSwarm) {
        match &event {
            FromSwarm::ConnectionEstablished(e) => {
                let remote = e.endpoint.get_remote_address().clone();
                // 若该 Peer 有记录拨号开始时间，用真实 RTT；否则 latency=0 表示不更新延迟
                let rtt = self
                    .pending_dials
                    .remove(&e.peer_id)
                    .map(|(_, start)| start.elapsed());
                self.ranker.feedback(
                    &e.peer_id,
                    &remote,
                    rtt.unwrap_or(Duration::from_secs(0)),
                    true,
                );
                self.alert_evaluator.record_connection_event();
                self.maybe_tick_alert();
            }
            FromSwarm::ConnectionClosed(_) => {
                self.alert_evaluator.record_connection_event();
                self.maybe_tick_alert();
            }
            FromSwarm::DialFailure(e) => {
                if let Some(peer_id) = e.peer_id {
                    match self.pending_dials.remove(&peer_id) {
                        // 归因到真实拨号地址：失败惩罚真正作用于候选地址的评分/信用
                        Some((addr, start)) => {
                            self.ranker.feedback(&peer_id, &addr, start.elapsed(), false);
                        }
                        // 内部/Kademlia 发起的拨号无地址记录：
                        // 沿用空地址占位，仅保留 alert 侧的失败率观测
                        None => {
                            self.ranker.feedback(
                                &peer_id,
                                &Multiaddr::empty(),
                                Duration::from_secs(2),
                                false,
                            );
                        }
                    }
                    self.alert_evaluator.record_failure();
                    if self.ranker.consume_mutation_flag() {
                        self.alert_evaluator.record_mutation();
                    }
                    self.maybe_tick_alert();
                }
            }
            _ => {}
        }
        self.inner.on_swarm_event(event);
    }

    fn on_connection_handler_event(
        &mut self,
        peer_id: PeerId,
        connection_id: libp2p::swarm::ConnectionId,
        event: libp2p::swarm::THandlerOutEvent<Self>,
    ) {
        self.inner
            .on_connection_handler_event(peer_id, connection_id, event);
    }

    fn poll(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<libp2p::swarm::ToSwarm<Self::ToSwarm, libp2p::swarm::THandlerInEvent<Self>>>
    {
        self.inner.poll(cx)
    }
}
