//! Relay 节点连接管理
//!
//! 处理中继节点的拨号、重连、启用/禁用中继服务。
//!
//! 拨号策略：
//! - 优先尝试配置的 relay 节点（静态列表）
//! - 其次尝试通过 Identify 自动发现的 relay 候选节点
//! - 最后通过 DHT GetProviders 查询其他中继节点
//! - 失败后退避：30s → 60s → 120s → 300s（几何倍增 30·2ⁿ⁻¹，封顶 300s）；
//!   冷却到期后若 5 分钟内无新的拨号调用，重置失败计数器。
//!
//! 数据不变量：`relay_nodes` 与 `relay_candidates` 中存储的 Multiaddr
//! 均为「纯传输地址」——不含 `P2p` 组件，由调用方在拨号/监听时统一追加。

use std::str::FromStr;

use libp2p::multiaddr::Protocol;
use libp2p::{Multiaddr, PeerId};

use super::P2pActor;
use crate::command::RelayRole;
use super::P2pEvent;
use super::DHT_RELAY_INDEX_KEY;

/// Relay DHT 查询最小间隔（秒）—— 防止 dial_relay_nodes 高频调用时 DHT 查询风暴
const RELAY_DHT_QUERY_INTERVAL_SECS: u64 = 60;
/// 退避时间上限（秒）—— 不再使用 1 小时，避免 NAT 节点长时间断连
const BACKOFF_MAX_SECS: u64 = 300;
/// 退避重置间隔（秒）—— 冷却到期后须再经过此时长且无新失败，才重置计数器。
/// 持续失败会不断刷新冷却时间，此条件不会满足，避免无限重置。
const BACKOFF_RESET_SECS: u64 = 300;

/// 从 Multiaddr 中剥离所有 `P2p` 组件，返回纯传输地址。
/// 用于在插入 relay_nodes / relay_candidates 前归一化，确保存储不变量。
pub(super) fn strip_p2p(addr: &Multiaddr) -> Multiaddr {
    addr.iter()
        .filter(|p| !matches!(p, Protocol::P2p(_)))
        .fold(Multiaddr::empty(), |mut m, p| {
            m.push(p);
            m
        })
}

/// 解析并归一化配置的中继节点列表，丢弃无效/重复条目。
/// 返回 `(PeerId, 纯传输地址)` 列表，保证不含 `P2p` 组件且 `PeerId` 唯一。
/// `kind` 用于日志标注（"relay" / "bootstrap"）。
pub(super) fn parse_relay_nodes(raw: Vec<(String, String)>, kind: &str) -> Vec<(PeerId, Multiaddr)> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::with_capacity(raw.len());
    for (pid_str, addr_str) in raw {
        let pid = match PeerId::from_str(&pid_str) {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!("无效的 {kind} PeerID '{}': {}", pid_str, e);
                continue;
            }
        };
        let addr = match addr_str.parse::<Multiaddr>() {
            Ok(a) => a,
            Err(e) => {
                tracing::warn!("无效的 {kind} Multiaddr '{}': {}", addr_str, e);
                continue;
            }
        };
        if !seen.insert(pid) {
            tracing::warn!("重复的 {kind} PeerID '{}'，已跳过", pid_str);
            continue;
        }
        out.push((pid, strip_p2p(&addr)));
    }
    out
}

impl P2pActor {
    /// 向所有 relay 节点发起拨号（NAT 后节点需要中继连接）
    pub(crate) fn dial_relay_nodes(&mut self) {
        // 角色非 client 时不拨号中继
        if self.relay_role != RelayRole::Client {
            tracing::debug!("Relay role is {:?}, skipping dial_relay_nodes", self.relay_role);
            return;
        }
        // 如果 server 意外开着，自动关闭（互斥保证）
        if self.relay_server_enabled {
            tracing::warn!("Relay server was enabled while role is client, auto-disabling");
            self.disable_relay_server();
        }

        let now = std::time::Instant::now();

        // 冷却到期后再经过 BACKOFF_RESET_SECS 且无新失败，才重置计数器。
        // 持续失败时每次都会刷新 cooldown_until，此条件不会满足，避免无限重置。
        if self.relay_reconnect_attempt > 0
            && let Some(cooldown) = self.relay_reconnect_cooldown_until
                && now >= cooldown + std::time::Duration::from_secs(BACKOFF_RESET_SECS) {
                    tracing::debug!("冷却到期已超过 {}s，重置 relay 重连计数器", BACKOFF_RESET_SECS);
                    self.relay_reconnect_attempt = 0;
                    self.relay_reconnect_cooldown_until = None;
                }

        if let Some(cooldown) = self.relay_reconnect_cooldown_until
            && now < cooldown {
                tracing::debug!("Relay reconnect in cooldown, skipping (until {:?})", cooldown);
                return;
            }

        let has_configured = !self.relay_nodes.is_empty();
        let has_candidates = !self.relay_candidates.is_empty();
        if !has_configured && !has_candidates { return; }

        let mut dial_success = false;
        let mut attempted = false;

        // 配置节点：地址已归一化为纯传输地址，直接追加 P2p 组件
        let configured_nodes = self.relay_nodes.clone();
        for (relay_peer_id, relay_addr) in &configured_nodes {
            let full_addr = relay_addr.clone().with(Protocol::P2p(*relay_peer_id));
            attempted = true;
            if self.try_relay_connection(relay_peer_id, &full_addr).is_ok() {
                dial_success = true;
            }
        }

        // 候选节点：地址已归一化（DHT 发现的纯 P2p 占位经 strip 后为空，会被传输层检查跳过）
        let candidates = self.relay_candidates.clone();
        for (candidate_peer_id, candidate_addr, _added_at) in &candidates {
            let full_addr = candidate_addr.clone().with(Protocol::P2p(*candidate_peer_id));
            // 仅当含完整传输层（Tcp / QuicV1）时才拨号；
            // 如果地址为空（DHT 发现的占位），Kademlia 尚未填充传输地址，跳过
            let has_transport = full_addr
                .iter()
                .any(|p| matches!(p, Protocol::Tcp(_) | Protocol::QuicV1));
            if !has_transport {
                tracing::debug!(
                    "Skipping dial for relay candidate {}: no transport address yet",
                    candidate_peer_id
                );
                continue;
            }
            attempted = true;
            if self.try_relay_connection(candidate_peer_id, &full_addr).is_ok() {
                dial_success = true;
            }
        }

        // 清理过期候选节点：移除添加超过 5 分钟且仍无传输地址的空占位条目，
        // 防止 DHT 返回的空地址永久占用候选列表槽位。
        const CANDIDATE_EXPIRY_SECS: u64 = 300;
        let before_len = self.relay_candidates.len();
        self.relay_candidates.retain(|(_, addr, added_at)| {
            !addr.iter().next().is_none()
                || added_at.elapsed().as_secs() < CANDIDATE_EXPIRY_SECS
        });
        if self.relay_candidates.len() < before_len {
            tracing::debug!(
                "Pruned {} expired empty-address relay candidates",
                before_len - self.relay_candidates.len()
            );
        }

        // DHT 查询频率控制：每次 dial_relay_nodes 调用时，
        // 至少间隔 RELAY_DHT_QUERY_INTERVAL_SECS 才发起 get_providers，防止查询风暴。
        if self.relay_dht_query_cooldown_until.is_none_or(|c| now >= c) {
                self.relay_dht_query_cooldown_until =
                    Some(now + std::time::Duration::from_secs(RELAY_DHT_QUERY_INTERVAL_SECS));
                let _ = self
                    .swarm
                    .behaviour_mut()
                    .kademlia
                    .get_providers(libp2p::kad::RecordKey::new(&DHT_RELAY_INDEX_KEY));
            }

        if !dial_success && attempted {
            let is_public = matches!(self.nat_status, libp2p::autonat::NatStatus::Public(..));
            if !is_public {
                self.relay_reconnect_attempt += 1;
                let backoff_secs = 30u64
                    .saturating_mul(2u64.saturating_pow(
                        self.relay_reconnect_attempt.saturating_sub(1),
                    ))
                    .min(BACKOFF_MAX_SECS);
                let msg = format!(
                    "中继连接失败（第{}次）：所有 relay 节点均无法连接，{}s 后重试",
                    self.relay_reconnect_attempt, backoff_secs
                );
                tracing::warn!("{}", msg);
                self.relay_reconnect_cooldown_until =
                    Some(now + std::time::Duration::from_secs(backoff_secs));
                let _ = self
                    .event_tx
                    .try_send(P2pEvent::Log(format!("relay_warning:{}", msg)));
            } else {
                tracing::debug!("公网节点，跳过中继连接失败警告");
                self.relay_reconnect_attempt = 0;
            }
        } else if dial_success {
            self.relay_reconnect_attempt = 0;
        }
    }

    fn try_relay_connection(&mut self, peer_id: &PeerId, full_addr: &Multiaddr) -> Result<(), String> {
        self.swarm
            .dial(full_addr.clone())
            .map_err(|e| format!("dial failed: {}", e))?;
        tracing::debug!("Dialed relay node: {}", peer_id);
        // Reservation 在 ConnectionEstablished 事件中请求，延迟到连接建立后
        // 与官方 DCUtR 示例一致：先 dial → 等 Identify → 再 listen_on
        Ok(())
    }

    /// 连接建立后向中继请求 circuit reservation
    ///
    /// 仅向配置的中继节点发送 reservation 请求。
    /// 自动发现的候选节点（Identify/DHT）虽然支持 relay 协议，
    /// 但可能是 IPFS 引导节点等非中继服务器，不会接受 reservation。
    /// 候选节点用于拨号回退，不用于 reservation。
    pub(crate) fn on_relay_connected(&mut self, relay_peer_id: &PeerId) -> bool {
        for (pid, addr) in &self.relay_nodes {
            if pid == relay_peer_id {
                // addr 已归一化为纯传输地址，安全追加 P2p + P2pCircuit
                let circuit_addr = addr
                    .clone()
                    .with(Protocol::P2p(*pid))
                    .with(Protocol::P2pCircuit);
                return match self.swarm.listen_on(circuit_addr) {
                    Ok(_) => {
                        tracing::info!(
                            "=== RESERVATION REQ: sent to relay {relay_peer_id} ==="
                        );
                        true
                    }
                    Err(e) => {
                        tracing::warn!("listen_on relay {} failed: {e:?}", relay_peer_id);
                        false
                    }
                };
            }
        }
        tracing::debug!(
            "relay {} not in configured relay list, skipping reservation (candidate only)",
            relay_peer_id
        );
        false
    }

    pub(crate) fn disconnect_relay_nodes(&mut self) {
        let connected: Vec<PeerId> = self.relay_connections.iter().copied().collect();
        for pid in &connected {
            if let Err(e) = self.swarm.disconnect_peer_id(*pid) {
                tracing::warn!("Failed to disconnect relay {}: {:?}", pid, e);
            }
        }
        self.relay_connections.clear();
        self.relay_candidates.clear();
    }

    pub(crate) fn try_enable_relay_server(&mut self) {
        if self.relay_server_enabled {
            return;
        }
        // 计费网络不允许启用中继（避免产生额外流量费用）
        if self.paid_network {
            tracing::debug!("Relay server not allowed on paid network, skipping");
            return;
        }
        // 公网节点自动授权中继 + 角色迁移（在角色检查之前，确保迁移能生效）
        if !self.relay_server_allowed {
            if matches!(self.nat_status, libp2p::autonat::NatStatus::Public(_)) {
                self.relay_server_allowed = true;
                // 自动迁移旧节点：默认角色 client 升级到 server（用户未显式设置过时）
                if !self.relay_role_user_configured && self.relay_role == RelayRole::Client {
                    self.relay_role = RelayRole::Server;
                    tracing::warn!("公网节点，中继角色已自动升级为 'server'（可在网络监控 UI 中改回 'client'）");
                }
                tracing::info!("Auto-authorized relay server: node is on public network");
            } else {
                tracing::debug!("Relay server not allowed, skipping");
                return;
            }
        }
        // 硬保护：用户显式禁止（allowed=false）或尚未授权时，不执行 start_providing
        if !self.relay_server_allowed {
            tracing::debug!("Relay server not allowed, skipping");
            return;
        }
        // 角色非 server 时不启用中继服务（放在自动授权之后，确保迁移在角色检查前完成）
        if self.relay_role != RelayRole::Server {
            tracing::debug!("Relay role is {:?}, skipping relay server enable", self.relay_role);
            return;
        }
        // libp2p 0.56 中继服务 behaviour 复用基础 TCP/QUIC 传输，
        // 无需 listen_on(/p2p-circuit)；启用即向 DHT 注册以便任何节点发现，
        // 同时打开 behaviour 的运行时开关（本地 patch），真正开始服务入站中继请求。
        let relay_key = libp2p::kad::RecordKey::new(&DHT_RELAY_INDEX_KEY);
        match self
            .swarm
            .behaviour_mut()
            .kademlia
            .start_providing(relay_key)
        {
            Ok(_) => {
                self.swarm
                    .behaviour_mut()
                    .relay_server
                    .set_server_enabled(true);
                tracing::info!(
                    "Relay server enabled: providing /{DHT_RELAY_INDEX_KEY}, serving HOP to any peer"
                );
                self.relay_server_enabled = true;
            }
            Err(e) => tracing::warn!("Relay server start_providing failed: {e:?}"),
        }
    }

    pub(crate) fn disable_relay_server(&mut self) {
        if !self.relay_server_enabled {
            return;
        }
        let relay_key = libp2p::kad::RecordKey::new(&DHT_RELAY_INDEX_KEY);
        self.swarm
            .behaviour_mut()
            .kademlia
            .stop_providing(&relay_key);
        // 关闭 behaviour 运行时开关，拒绝新的入站 reservation/circuit 请求
        self.swarm
            .behaviour_mut()
            .relay_server
            .set_server_enabled(false);
        self.relay_server_enabled = false;
        tracing::info!("Relay server disabled (stopped DHT providing + relay behaviour)");
    }

    /// 设置中继角色，强制互斥
    pub(crate) fn set_relay_role(&mut self, role: RelayRole) {
        self.relay_role_user_configured = true;
        match role {
            RelayRole::Server => {
                self.disable_relay_client();
                self.relay_role = RelayRole::Server;
                self.try_enable_relay_server();
            }
            RelayRole::Client => {
                self.relay_role = RelayRole::Client;
                self.disable_relay_server();
                self.dial_relay_nodes();
            }
            RelayRole::Off => {
                self.disable_relay_server();
                self.disable_relay_client();
                self.relay_role = RelayRole::Off;
            }
        }
    }

    /// 关闭所有 relay 客户端连接，清理相关状态
    pub(crate) fn disable_relay_client(&mut self) {
        self.disconnect_relay_nodes();
        self.relay_candidates.clear();
        self.reservation_attempted.clear();
        self.relay_reconnect_cooldown_until = None;
        self.relay_reconnect_attempt = 0;
        tracing::info!("Relay client disabled");
    }
}
