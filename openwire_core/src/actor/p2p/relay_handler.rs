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
use crate::command::{PaidNetworkMode, RelayRole};
use super::P2pEvent;
use super::DHT_RELAY_INDEX_KEY;

/// 中继设置持久化文件名：`data_dir/relay_settings.json`
///
/// 保存用户显式选择的 `relay_role` 与 `paid_network_mode`，
/// 使「付费网络检测 API 不可用 / 用户设为 disabled」时能按持久化身份运行
/// （默认 client），而不是每次重启退化为"当作付费网络强制关闭中继"。
const RELAY_SETTINGS_FILE: &str = "relay_settings.json";

/// 中继设置（可持久化）
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub(super) struct RelaySettings {
    /// 用户显式选择的中继角色："server" / "client" / "off"；
    /// `None` 表示用户从未显式设置（走自动迁移/默认 client）
    #[serde(default)]
    pub relay_role: Option<String>,
    /// 付费网络检测模式："free" / "paid" / "disabled"；`None` 表示未检测
    #[serde(default)]
    pub paid_network_mode: Option<String>,
}

/// 从 data_dir 加载中继设置（文件缺失/解析失败时返回默认）
pub(super) fn load_relay_settings(data_dir: &std::path::Path) -> RelaySettings {
    let path = data_dir.join(RELAY_SETTINGS_FILE);
    match std::fs::read_to_string(&path) {
        Ok(content) => match serde_json::from_str(&content) {
            Ok(s) => {
                tracing::debug!("已加载中继设置: {:?}", path);
                s
            }
            Err(e) => {
                tracing::warn!("解析中继设置失败 {}: {e}", path.display());
                RelaySettings::default()
            }
        },
        Err(_) => RelaySettings::default(),
    }
}

impl RelaySettings {
    /// 持久化到 data_dir（先写临时文件再改名，避免崩溃留下半截 JSON）。
    ///
    /// 注意：`std::fs::rename` 在 Windows 上若目标已存在会失败，
    /// 必须先删除旧目标文件再改名（两次调用之间有小窗口，
    /// 但相比直接 write 覆写，能避免留下半截 JSON 导致下次启动解析失败）。
    fn save(&self, data_dir: &std::path::Path) {
        let path = data_dir.join(RELAY_SETTINGS_FILE);
        let tmp = data_dir.join(format!("{RELAY_SETTINGS_FILE}.tmp"));
        if let Err(e) = std::fs::write(&tmp, serde_json::to_string_pretty(self).unwrap_or_default())
        {
            tracing::warn!("保存中继设置失败(临时文件) {}: {e}", tmp.display());
            return;
        }
        // 目标已存在先删除，再改名（Windows 兼容）
        if path.exists() {
            let _ = std::fs::remove_file(&path);
        }
        if let Err(e) = std::fs::rename(&tmp, &path) {
            tracing::warn!("保存中继设置失败(改名) {}: {e}", path.display());
            let _ = std::fs::remove_file(&tmp);
        }
    }

    /// 解析中继角色（未知值回退 None）
    pub(super) fn parsed_relay_role(&self) -> Option<RelayRole> {
        self.relay_role.as_deref().and_then(|s| match s {
            "server" => Some(RelayRole::Server),
            "client" => Some(RelayRole::Client),
            "off" => Some(RelayRole::Off),
            _ => None,
        })
    }

    /// 解析付费网络模式（未知值回退 None）
    pub(super) fn parsed_paid_mode(&self) -> Option<PaidNetworkMode> {
        self.paid_network_mode.as_deref().and_then(|s| match s {
            "free" => Some(PaidNetworkMode::Free),
            "paid" => Some(PaidNetworkMode::Metered),
            "disabled" => Some(PaidNetworkMode::Disabled),
            _ => None,
        })
    }
}

/// Relay DHT 查询最小间隔（秒）—— 防止 dial_relay_nodes 高频调用时 DHT 查询风暴
const RELAY_DHT_QUERY_INTERVAL_SECS: u64 = 60;
/// 退避时间上限（秒）—— 不再使用 1 小时，避免 NAT 节点长时间断连
const BACKOFF_MAX_SECS: u64 = 300;
/// 退避重置间隔（秒）—— 冷却到期后须再经过此时长且无新失败，才重置计数器。
/// 持续失败会不断刷新冷却时间，此条件不会满足，避免无限重置。
const BACKOFF_RESET_SECS: u64 = 300;
/// 同时请求 circuit reservation 的候选中继数量上限。
/// 配置中继不受此限制；候选节点仅在已成功 reservation 数低于上限时参与，
/// 避免过量占用远端中继（轻量级 server）的 reservation 配额。
const MAX_RESERVATION_RELAYS: usize = 4;

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

/// 判断地址是否含完整传输层（Tcp / QuicV1），不含传输层的地址不可拨号/不可 reservation。
fn has_transport(addr: &Multiaddr) -> bool {
    addr.iter().any(|p| matches!(p, Protocol::Tcp(_) | Protocol::QuicV1))
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
            if !has_transport(&full_addr) {
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
    /// 按 Circuit Relay v2 标准流程：dial → Identify（确认 HOP 协议）→
    /// listen_on(relay_addr/p2p-circuit)。配置的中继节点总是保留；
    /// 通过 Identify/DHT 发现且已确认支持 HOP、具备传输地址的候选节点，
    /// 在 reservation 数量上限内同样请求 —— 不依赖当前 libp2p-relay 对
    /// ReservationReqAccepted 事件的具体实现，上游修复后自动正确。
    pub(crate) fn on_relay_connected(&mut self, relay_peer_id: &PeerId) -> bool {
        // 1. 配置的中继节点（不受上限约束）
        let (pid, addr) = match self
            .relay_nodes
            .iter()
            .find(|(pid, _)| pid == relay_peer_id)
        {
            Some((p, a)) => (*p, a.clone()),
            None => {
                // 2. 候选节点：仅具备纯传输地址、且 successfully reservation 数未达上限时参与
                // （按成功数计：失败的/被拒的尝试不占槽位，避免耗尽候选配额）
                if self.reservation_established.len() >= MAX_RESERVATION_RELAYS {
                    tracing::debug!(
                        "relay {} 候选 reservation 达到上限({MAX_RESERVATION_RELAYS})，跳过",
                        relay_peer_id
                    );
                    return false;
                }
                match self
                    .relay_candidates
                    .iter()
                    .find(|(pid, addr, _)| pid == relay_peer_id && has_transport(addr))
                {
                    Some((p, a, _)) => (*p, a.clone()),
                    None => {
                        tracing::debug!(
                            "relay {} 无可用 reservation 地址（配置或候选均不可用）",
                            relay_peer_id
                        );
                        return false;
                    }
                }
            }
        };
        self.request_circuit_reservation(&pid, &addr)
    }

    fn request_circuit_reservation(&mut self, pid: &PeerId, addr: &Multiaddr) -> bool {
        // addr 已归一化为纯传输地址，安全追加 P2p + P2pCircuit
        let circuit_addr = addr
            .clone()
            .with(Protocol::P2p(*pid))
            .with(Protocol::P2pCircuit);
        match self.swarm.listen_on(circuit_addr) {
            Ok(_) => {
                tracing::info!(
                    "=== RESERVATION REQ: sent to relay {pid} ==="
                );
                true
            }
            Err(e) => {
                tracing::warn!("listen_on relay {pid} failed: {e:?}");
                false
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
        // 断开即报废已发起的 reservation：清空后重新连接时可重新请求
        // （修复 Public→Private 翻转后 reservation_attempted 残留导致不重新 reservation）
        self.reservation_attempted.clear();
        self.reservation_established.clear();
    }

    pub(crate) fn try_enable_relay_server(&mut self) {
        if self.relay_server_enabled {
            return;
        }
        // 用户显式选择的角色最高优先级：显式 server 即使计费网络也启用（用户自担流量）。
        // 计费门只拦截「自动路径」——未显式配置角色时避免意外产生流量费用。
        let user_explicit_server =
            self.relay_role_user_configured && self.relay_role == RelayRole::Server;
        if self.paid_network && !user_explicit_server {
            tracing::debug!("Relay server not allowed on paid network, skipping");
            return;
        }
        // 公网节点自动授权中继 + 角色迁移（在角色检查之前，确保迁移能生效）。
        // 用户显式选择 server 最高优先级：不受「必须公网」限制，直接放行。
        if !self.relay_server_allowed {
            if user_explicit_server {
                self.relay_server_allowed = true;
                tracing::info!("User explicitly chose relay role 'server', authorizing");
            } else if matches!(self.nat_status, libp2p::autonat::NatStatus::Public(_))
                // 检测禁用（Disabled）时不做自动迁移：保持持久化角色（默认 client），
                // 只有明确检测为免费（Free）的自动路径才在公网时升级为 server。
                && self.paid_network_mode != PaidNetworkMode::Disabled
            {
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
        self.persist_relay_settings();
    }

    /// 持久化当前中继设置到磁盘（relay_settings.json）
    ///
    /// 只持久化「用户显式设置」的角色：自动迁移（公网 client→server）
    /// 是运行时可逆决策，不应写盘——否则重启后会被误判为用户显式选择，
    /// 使付费网络下中继服务关不掉（流量费风险）。自动迁移的角色在
    /// 每次启动时由 nat_status + paid_network_mode 重新评估。
    pub(crate) fn persist_relay_settings(&self) {
        let settings = RelaySettings {
            relay_role: if self.relay_role_user_configured {
                Some(match self.relay_role {
                    RelayRole::Server => "server".to_string(),
                    RelayRole::Client => "client".to_string(),
                    RelayRole::Off => "off".to_string(),
                })
            } else {
                None
            },
            paid_network_mode: Some(match self.paid_network_mode {
                PaidNetworkMode::Free => "free".to_string(),
                PaidNetworkMode::Metered => "paid".to_string(),
                PaidNetworkMode::Disabled => "disabled".to_string(),
            }),
        };
        settings.save(&self.data_dir);
        tracing::debug!(
            "中继设置已持久化: role={:?} (user_configured={}), mode={:?}",
            settings.relay_role,
            self.relay_role_user_configured,
            self.paid_network_mode
        );
    }

    /// 关闭所有 relay 客户端连接，清理相关状态
    pub(crate) fn disable_relay_client(&mut self) {
        self.disconnect_relay_nodes();
        self.relay_candidates.clear();
        self.reservation_attempted.clear();
        self.reservation_established.clear();
        self.relay_reconnect_cooldown_until = None;
        self.relay_reconnect_attempt = 0;
        tracing::info!("Relay client disabled");
    }
}
