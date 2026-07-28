//! Relay 节点连接管理
//!
//! 处理中继节点的拨号、重连、启用/禁用中继服务。
//!
//! 拨号策略：
//! - 优先尝试配置的 relay 节点（静态列表）
//! - 其次尝试通过 Identify 自动发现的 relay 候选节点
//! - 最后通过 DHT GetProviders 查询其他中继节点
//! - 失败后退避：30s → 60s → 90s，上限 120s（每 5 分钟重置计数器）

use std::str::FromStr;

use libp2p::multiaddr::Protocol;
use libp2p::{PeerId, Multiaddr};

use super::P2pActor;
use super::P2pEvent;
use super::DHT_RELAY_INDEX_KEY;

/// 退避时间上限（秒）—— 不再使用 1 小时，避免 NAT 节点长时间断连
const BACKOFF_MAX_SECS: u64 = 120;
/// 退避重置间隔（秒）—— 每 5 分钟重置失败计数器，避免永久退避
const BACKOFF_RESET_SECS: u64 = 300;

impl P2pActor {
    /// 向所有 relay 节点发起拨号（NAT 后节点需要中继连接）
    pub(crate) fn dial_relay_nodes(&mut self) {
        let now = std::time::Instant::now();

        // 定期重置退避计数器，避免因临时 relay 不可用而永远退避
        if self.relay_reconnect_attempt > 0 {
            if let Some(cooldown) = self.relay_reconnect_cooldown_until {
                if now >= cooldown + std::time::Duration::from_secs(BACKOFF_RESET_SECS) {
                    tracing::debug!("退避已超过 {}s，重置 relay 重连计数器", BACKOFF_RESET_SECS);
                    self.relay_reconnect_attempt = 0;
                    self.relay_reconnect_cooldown_until = None;
                }
            }
        }

        if let Some(cooldown) = self.relay_reconnect_cooldown_until {
            if now < cooldown {
                tracing::debug!("Relay reconnect in cooldown, skipping (until {:?})", cooldown);
                return;
            }
        }

        let has_configured = !self.relay_nodes.is_empty();
        let has_candidates = !self.relay_candidates.is_empty();
        if !has_configured && !has_candidates { return; }

        // 移除 relay_dialed 的激进阻止——只要不在冷却期且有候选节点就允许拨号
        let mut dial_success = false;
        let mut attempted = false;

        let configured_nodes = self.relay_nodes.clone();
        for (relay_peer_id, relay_addr) in &configured_nodes {
            match relay_addr.parse::<Multiaddr>() {
                Ok(addr) => {
                    let has_p2p = addr.iter().any(|p| matches!(p, Protocol::P2p(..)));
                    let full_addr = if has_p2p { addr.clone() } else {
                        match PeerId::from_str(relay_peer_id) {
                            Ok(pid) => addr.clone().with_p2p(pid).unwrap_or(addr.clone()),
                            Err(e) => { tracing::warn!("无效的 relay PeerID '{}': {}", relay_peer_id, e); continue; }
                        }
                    };
                    attempted = true;
                    if self.try_relay_connection(relay_peer_id, &full_addr).is_ok() {
                        dial_success = true;
                    }
                }
                Err(e) => tracing::warn!("无效的 relay 地址 '{}': {}", relay_addr, e),
            }
        }

        let candidates = self.relay_candidates.clone();
        for (candidate_peer_id, candidate_addr) in &candidates {
            let has_p2p = candidate_addr.iter().any(|p| matches!(p, Protocol::P2p(..)));
            let full_addr = if has_p2p { candidate_addr.clone() } else {
                candidate_addr.clone().with_p2p(*candidate_peer_id).unwrap_or(candidate_addr.clone())
            };
            attempted = true;
            if self.try_relay_connection(&candidate_peer_id.to_string(), &full_addr).is_ok() {
                dial_success = true;
            }
        }

        let _ = self.swarm.behaviour_mut().kademlia
            .get_providers(libp2p::kad::RecordKey::new(&DHT_RELAY_INDEX_KEY));

        if !dial_success && attempted {
            let is_public = matches!(self.nat_status, libp2p::autonat::NatStatus::Public(..));
            if !is_public {
                self.relay_reconnect_attempt += 1;
                let backoff_secs = 30u64
                    .saturating_mul(2u64.saturating_pow(self.relay_reconnect_attempt.saturating_sub(1)))
                    .min(BACKOFF_MAX_SECS);
                let msg = format!(
                    "中继连接失败（第{}次）：所有 relay 节点均无法连接，{}s 后重试",
                    self.relay_reconnect_attempt, backoff_secs
                );
                tracing::warn!("{}", msg);
                self.relay_reconnect_cooldown_until = Some(now + std::time::Duration::from_secs(backoff_secs));
                let _ = self.event_tx.try_send(P2pEvent::Log(format!("relay_warning:{}", msg)));
            } else {
                tracing::debug!("公网节点，跳过中继连接失败警告");
                self.relay_reconnect_attempt = 0;
            }
        } else if dial_success {
            self.relay_reconnect_attempt = 0;
        }
    }

    fn try_relay_connection(&mut self, peer_id_str: &str, full_addr: &Multiaddr) -> Result<(), String> {
        self.swarm.dial(full_addr.clone()).map_err(|e| format!("dial failed: {}", e))?;
        tracing::debug!("Dialed relay node: {}", peer_id_str);
        // Reservation 在 ConnectionEstablished 事件中请求，延迟到连接建立后
        // 与官方 DCUtR 示例一致：先 dial → 等 Identify → 再 listen_on
        Ok(())
    }

    /// 连接建立后向中继请求 circuit reservation
    pub(crate) fn on_relay_connected(&mut self, relay_peer_id: &PeerId) {
        // 与官方 DCUtR 示例完全对齐：
        // swarm.listen_on(relay_address.with(Protocol::P2pCircuit))
        // relay_address 必须包含完整路径 /ip4/.../p2p/<relay_peer>
        for (pid_str, addr_str) in &self.relay_nodes {
            if let (Ok(pid), Ok(addr)) = (pid_str.parse::<PeerId>(), addr_str.parse::<Multiaddr>()) {
                if pid == *relay_peer_id {
                    let circuit_addr = addr.with(Protocol::P2p(pid)).with(Protocol::P2pCircuit);
                    match self.swarm.listen_on(circuit_addr) {
                        Ok(_) => tracing::info!("=== RESERVATION REQ: sent to relay {relay_peer_id} ==="),
                        Err(e) => tracing::warn!("listen_on relay {} failed: {e:?}", relay_peer_id),
                    }
                }
            }
        }
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
        if self.relay_server_enabled { return; }
        if !self.relay_server_allowed {
            tracing::debug!("Relay server not allowed (metered network), skipping");
            return;
        }
        let relay_key = libp2p::kad::RecordKey::new(&DHT_RELAY_INDEX_KEY);
        let _ = self.swarm.behaviour_mut().kademlia.start_providing(relay_key);
        if let Ok(addr) = "/p2p-circuit".parse::<libp2p::Multiaddr>() {
            match self.swarm.listen_on(addr) {
                Ok(_) => {
                    tracing::debug!("Relay server enabled: listening on /p2p-circuit");
                    self.relay_server_enabled = true;
                }
                Err(e) => tracing::warn!("Failed to listen on /p2p-circuit: {:?}", e),
            }
        }
    }

    pub(crate) fn disable_relay_server(&mut self) {
        if !self.relay_server_enabled { return; }
        let relay_key = libp2p::kad::RecordKey::new(&DHT_RELAY_INDEX_KEY);
        self.swarm.behaviour_mut().kademlia.stop_providing(&relay_key);
        self.relay_server_enabled = false;
        tracing::info!("Relay server disabled");
    }
}