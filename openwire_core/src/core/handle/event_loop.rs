use libp2p::PeerId;

use crate::actor::RUNTIME as rt;

use crate::{
    actor::p2p::{P2pCommand, P2pEvent},
    command::ChatCommand,
    core::{timers, ChatCore},
    storage,
};

impl ChatCore {
    /// 启动核心事件循环（使用独立的 Tokio 运行时）
    pub fn run(mut self) -> std::thread::JoinHandle<()> {
        std::thread::spawn(move || {
            rt.block_on(async move {
                self.run_inner().await;
            });
        })
    }

    /// 启动核心事件循环（使用调用方提供的 Tokio 运行时句柄）
    pub fn run_on_runtime(
        mut self,
        rt_handle: tokio::runtime::Handle,
    ) -> std::thread::JoinHandle<()> {
        let handle = rt_handle.clone();
        std::thread::spawn(move || {
            let _guard = handle.enter();
            handle.block_on(async move {
                self.run_inner().await;
            });
        })
    }

    /// 内部事件循环：DHT 注册 + 主循环
    async fn run_inner(&mut self) {
        // 启动所有定时器任务（独立 tokio::spawn，通过 cmd_tx 发送命令）
        let shutdown_token = self.core_handle.shutdown_token.clone();
        timers::spawn_all(self.core_handle.cmd_tx.clone(), shutdown_token);

        // 启动后立即执行一次 DHT 身份发布
        self.publish_current_identity_to_dht();

        // 启动后 15 秒首次触发联系人发现，之后每 60 秒重试一次
        // 即使无公网节点可达，本地 DhtCache 历史缓存或 mDNS 仍可发现部分联系人。
        // BootstrapReady 是加速路径，此定时器是兜底保障。
        let cmd_tx = self.core_handle.cmd_tx.clone();
        let shutdown_token = self.core_handle.shutdown_token.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(15)).await;
            let _ = cmd_tx.try_send(ChatCommand::TimerDiscoverAllContacts);
            // 后续每 60 秒重试，确保 bootstrap 完成后能重新发现
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
            loop {
                tokio::select! {
                    _ = interval.tick() => {
                        let _ = cmd_tx.try_send(ChatCommand::TimerDiscoverAllContacts);
                    }
                    _ = shutdown_token.cancelled() => break,
                }
            }
        });

        // 主事件循环：仅处理 P2pActor 事件和控制命令（定时器通过命令驱动）
        loop {
            tokio::select! {
                // 处理取消信号
                _ = self.core_handle.shutdown_token.cancelled() => {
                    tracing::info!("ChatCore 收到取消信号，正在关闭...");
                    self.shutdown_p2p();
                    break;
                }
                // 从 P2pActor 接收网络事件
                Some(event) = self.rx_p2p_event.recv() => {
                    self.handle_p2p_event(event).await;
                }
                Some(cmd) = self.rx_cmd.recv() => {
                    if matches!(cmd, ChatCommand::Shutdown) {
                        tracing::info!("P2P core shutting down...");
                        self.shutdown_p2p();
                        break;
                    }
                    // 处理身份切换：更新 DHT 注册循环的身份信息
                    if matches!(cmd, ChatCommand::SelectIdentity { .. }) {
                        self.handle_command(cmd).await;
                        self.publish_current_identity_to_dht();
                    } else {
                        self.handle_command(cmd).await;
                    }
                }
            }
        }
    }

    /// 向 P2pActor 发送关闭命令并保存路由表
    fn shutdown_p2p(&self) {
        if let Err(e) = self.p2p_handle.tx.try_send(
            P2pCommand::SaveRoutingTable,
        ) {
            tracing::warn!("Failed to send SaveRoutingTable on shutdown: {e:?}");
        }
        if let Err(e) = self.p2p_handle.tx.try_send(
            P2pCommand::Shutdown,
        ) {
            tracing::warn!("Failed to send Shutdown on shutdown: {e:?}");
        }
    }

    /// 处理来自 P2pActor 的网络事件
    async fn handle_p2p_event(&mut self, event: P2pEvent) {
        match event {
            P2pEvent::MessageReceived {
                peer,
                message,
                channel,
            } => {
                // 处理入站消息（原有逻辑在 events.rs 中）
                crate::p2p::handle_incoming_request(self, peer, channel, message).await;
            }
            P2pEvent::NetEventRequestReceived {
                peer,
                request,
                channel,
            } => {
                tracing::debug!("收到 NetEvent 请求: peer={}, request={:?}", peer, request);

                // === Fix 2: 处理 FriendOnline 通知：缓存对方身份 + 触发反向发现 ===
                // FriendOnline 直接携带所有身份信息（ML-DSA 公钥、PeerID、ML-KEM 公钥），
                // 无需等待 DHT 查询，连接建立后立即可用。
                if let crate::p2p::netevent::NetEventRequest::FriendOnline {
                    mldsa_pubkey_hex,
                    peer_id: claimed_peer_id,
                    listen_addrs,
                    mlkem_pubkey_hex,
                    signature,
                    version,
                    ..
                } = &request {
                    self.handle_friend_online_request(
                        mldsa_pubkey_hex.clone(),
                        claimed_peer_id.clone(),
                        listen_addrs.clone(),
                        mlkem_pubkey_hex.clone(),
                        *version,
                        signature.clone(),
                        peer,
                        channel,
                    ).await;
                    return;
                }
            }

            P2pEvent::ConnectionEstablished { peer_id, listen_addrs } => {
                tracing::info!("Connection established with {} ({} listen addrs)", peer_id, listen_addrs.len());
                self.connected_peers.entry(peer_id).and_modify(|c| *c += 1).or_insert(1);

                // 从 DHT 反向查找该 PeerID 对应的 ML-DSA 公钥
                let store = self.get_dht_store();
                match store.get_pubkey_by_peerid(&peer_id) {
                    Ok(Some(pubkey_hex)) => {
                        self.peerid_to_pubkey.insert(peer_id, pubkey_hex);
                    }
                    _ => {
                        tracing::debug!(
                            "ConnectionEstablished: 本地未缓存 PeerID {} 对应的公钥",
                            peer_id
                        );
                    }
                }

// === Fix 1: 连接建立后向对方发送 FriendOnline 通知（身份交换）===
                // 向所有对端发送 FriendOnline。非 OpenWire 节点（如 IPFS 引导节点）
                // 不支持 NetEvent 协议，发送会报 UnsupportedProtocols 错误，在 P2pActor
                // 中静默处理，不影响其他流程。
                if let Some(mldsa_pubkey_hex) = self.mldsa_pubkey_hex.clone()
                    && let Some(current_peer_id) = self.current_peer_id
                    && let Some(mldsa_private_key) = self.mldsa_private_key.as_ref() {
                        let mlkem = self.mlkem_pubkey_hex.clone().unwrap_or_default();
                        let Some(friend_online) =
                            crate::actor::p2p::netevent::build_friend_online_request(
                                &mldsa_pubkey_hex,
                                &current_peer_id,
                                &listen_addrs,
                                &mlkem,
                                mldsa_private_key,
                            )
                        else {
                            tracing::warn!("FriendOnline 签名失败，跳过发送");
                            return;
                        };
                        if let Err(e) = self.p2p_handle.tx.try_send(
                            P2pCommand::SendNetEvent {
                                peer_id,
                                request: friend_online,
                            },
                        ) {
                            tracing::warn!("Failed to send FriendOnline NetEvent: {e:?}");
                        }
                        tracing::info!(
                            "向 {}.. 发送了 FriendOnline 通知",
                            &mldsa_pubkey_hex[..16]
                        );
                    }

                // 触发在线状态更新
                self.send_online_status().await;
                // 注意：不再在 ConnectionEstablished 时立即重试，改由 FriendOnline 到达后重试
                // 这样可以确保身份信息（ML-KEM 公钥、PeerID 映射）已就绪后再发送
            }
            P2pEvent::ConnectionClosed { peer_id } => {
                tracing::info!("Connection closed with {}", peer_id);
                // 使用连接计数：减到 0 才真正移除，防止双连接误判离线
                if let std::collections::hash_map::Entry::Occupied(mut entry) = self.connected_peers.entry(peer_id) {
                    *entry.get_mut() = entry.get().saturating_sub(1);
                    if *entry.get() == 0 {
                        entry.remove();
                        self.peerid_to_pubkey.remove(&peer_id);
                    }
                }
                // 触发在线状态更新
                self.send_online_status().await;
            }
            P2pEvent::MdnsDiscovered { peer_id, addr } => {
                tracing::info!("mDNS discovered: {} at {}", peer_id, addr);
                if let Err(e) = self.p2p_handle.tx.try_send(
                    P2pCommand::AddKademliaAddress { peer_id, addr },
                ) {
                    tracing::warn!("Failed to send AddKademliaAddress for mDNS: {e:?}");
                }
            }
            P2pEvent::MdnsExpired { peer_id } => {
                tracing::info!("mDNS expired: {}", peer_id);
            }
            P2pEvent::IdentifyReceived {
                peer_id,
                listen_addrs,
            } => {
                for addr in listen_addrs {
                    if let Err(e) = self.p2p_handle.tx.try_send(
                    P2pCommand::AddKademliaAddress { peer_id, addr },
                ) {
                        tracing::warn!("Failed to send AddKademliaAddress for Identify: {e:?}");
                    }
                }
            }
            P2pEvent::GetProvidersResult {
                key,
                providers,
            } => {
                tracing::debug!(
                    "GetProvidersResult for key={}.., providers={:?}",
                    &key[..16.min(key.len())],
                    providers
                );
                if providers.is_empty() {
                    return;
                }

                // 仅用于拨号标签，不写入身份映射（身份映射仅由 FriendOnline 签名验证后建立）
                let actual_pubkey = self.dht_query_key_to_pubkey.get(&key).cloned();

                // === 拨号每个发现的 provider，建立 P2P 连接 ===
                for provider in &providers {
                    if !self.connected_peers.contains_key(provider) {
                        let label = actual_pubkey.as_ref().map_or(
                            format!("{}..", &key[..16.min(key.len())]),
                            |p| format!("{}..", &p[..16]),
                        );
                        tracing::info!(
                            "DHT 发现联系人 {}，正在拨号 PeerID={}",
                            label,
                            provider
                        );
                        if let Err(e) = self.p2p_handle.tx.try_send(
                            P2pCommand::Dial { peer_id: *provider },
                        ) {
                            tracing::warn!("Failed to send Dial to provider {}: {e:?}", provider);
                        }
                    }
                }

                // 刷新在线状态
                for provider in &providers {
                    if self.connected_peers.contains_key(provider) {
                        self.send_online_status().await;
                        break;
                    }
                }

                // 触发所有待发送消息的重试
                self.retry_pending_messages(None).await;
            }
            P2pEvent::PeerInfoReceived {
                mldsa_pubkey_hex,
                peer_id,
                mlkem_pubkey_hex,
            } => {
                tracing::info!("=== DISCOVER_PEER OK: {}.. → {} ===", &mldsa_pubkey_hex[..16.min(mldsa_pubkey_hex.len())], peer_id);
                let store = self.get_dht_store();
                let _ = store.set_pubkey_peerid(&mldsa_pubkey_hex, &peer_id);
                if !mlkem_pubkey_hex.is_empty() {
                    self.peerid_to_mlkem.insert(peer_id, mlkem_pubkey_hex.clone());
                }
                if !self.connected_peers.contains_key(&peer_id)
                    && let Err(e) = self.p2p_handle.tx.try_send(P2pCommand::Dial { peer_id }) {
                        tracing::warn!("Failed to send Dial after DiscoverPeer: {e:?}");
                    }
            }
            P2pEvent::PeerNotFound { mldsa_pubkey_hex } => {
                tracing::info!(
                    "=== DISCOVER_PEER NOT FOUND: {}.. (对方尚未添加本节点) ===",
                    &mldsa_pubkey_hex[..16.min(mldsa_pubkey_hex.len())]
                );
                // 对方尚未添加本节点为联系人，中继无此记录。
                // 不执行任何操作，等待对方添加后通过 FriendOnline 通知本节点。
            }
            P2pEvent::FriendOnlineNack { peer, reason } => {
                match reason {
                    crate::p2p::netevent::NackReason::SignatureVerificationFailed => {
                        // 对方无法验证本节点身份签名，可能是本节点私钥损坏或对方实现问题。
                        // 记录为错误，但不主动断开（可能是对方 bug，非本节点问题）。
                        tracing::error!(
                            "对方 {} 拒绝本节点 FriendOnline：签名验证失败",
                            peer
                        );
                    }
                    crate::p2p::netevent::NackReason::VersionMismatch { expected, got } => {
                        tracing::warn!(
                            "对方 {} 拒绝本节点 FriendOnline：协议版本不兼容（期望 {expected}，本节点 {got}）",
                            peer
                        );
                    }
                    crate::p2p::netevent::NackReason::PeerIdMismatch => {
                        tracing::warn!(
                            "对方 {} 拒绝本节点 FriendOnline：PeerID 不匹配",
                            peer
                        );
                    }
                    crate::p2p::netevent::NackReason::Other { description } => {
                        tracing::warn!(
                            "对方 {} 拒绝本节点 FriendOnline：{description}",
                            peer
                        );
                    }
                }
            }
            P2pEvent::GetRecordResult { .. } => {
                // GetRecordResult 由 events.rs 中的 DHT 查询回调处理，
                // 此处无需额外逻辑
            }
            P2pEvent::Log(msg) => {
                if let Some(warning) = msg.strip_prefix("relay_warning:") {
                    tracing::warn!("P2pActor relay warning: {}", warning);
                    self.send_warning_mpsc(warning.to_string()).await;
                } else {
                    tracing::info!("P2pActor: {}", msg);
                }
            }
            P2pEvent::DhtPublishFailed { error } => {
                tracing::warn!("DHT 发布失败: {}", error);
                self.send_warning_mpsc(format!("DHT 发布失败: {}", error)).await;
            }
            P2pEvent::MessageSent { peer, message_hash } => {
                // P2P 层确认消息已发送，标记为已发送
                if let Some(pool) = storage::pool() {
                    let _ = storage::mark_sent_by_hash(pool, &message_hash).await;
                    tracing::debug!("P2P 确认消息 {}.. 发送成功", &message_hash[..16]);
                }
            }
            P2pEvent::MessageSendFailed { peer, message_hash } => {
                tracing::warn!("P2P 发送消息 {}.. 失败（消息已保持待发送状态，等待重试）", &message_hash[..16]);
            }
            P2pEvent::BootstrapReady => {
                tracing::info!("DHT bootstrap ready, discovering contacts");
                // 启动发现弱点：discover_all_contacts 依赖 DHT GetProviders 查询，
                //
                // 1. 路由表节点数不足 → GetProviders 进入空网络，超时无结果
                // 2. 中继节点在路由表中但中继不参与 DHT → GetProviders 发往中继，无响应
                // 3. 所有联系人离线 → 无 FriendOnline，无 ML-KEM 缓存，消息进入离线队列
                //
                // 实际回退：如果本次 session 之前通过 DHT 或 FriendOnline 缓存过
                // pubkey→peerid 映射（DhtCache），即使 GetProviders 失败，
                // contact_ops::discover_contact 的本地缓存命中也能直接拨号。
                //
                // 强化方案：BootstrapOk 超时后启动一个定时器，每 30s 重试
                // discover_all_contacts 直到至少一个联系人成功解析。
                self.discover_all_contacts().await;
            }
        }
    }

    /// 将当前身份发布到 DHT 网络（本地数据库 + 网络发布）
    pub(crate) fn publish_current_identity_to_dht(&mut self) {
        if let (Some(pubkey), Some(pid)) = (self.mldsa_pubkey_hex.clone(), self.current_peer_id) {
            let store = self.get_dht_store();
            let _ = store.set_pubkey_peerid(&pubkey, &pid);
            if let Err(e) = self.core_handle.cmd_tx.try_send(ChatCommand::DhtPublishIdentity {
                mldsa_pubkey_hex: pubkey.clone(),
            }) {
                tracing::warn!("Failed to send DhtPublishIdentity: {e:?}");
            }
            tracing::info!("Published current identity to DHT network");
        }
    }

    /// 对所有已添加的联系人发起 DHT 发现
    ///
    /// 在 `discover_contact()` 中预先存储 DHT 查询键 → 公钥的映射，
    /// 确保 `GetProvidersResult` 能正确反向匹配到联系人。
    pub(crate) async fn discover_all_contacts(&self) {
        if let Some(pool) = storage::pool() {
            let owner_id = self.mldsa_identity_id.as_deref().unwrap_or("");
            if !owner_id.is_empty() {
                match storage::list_contacts(pool, owner_id).await {
                    Ok(contacts) => {
                        let count = contacts.len();
                        tracing::info!("启动后向 {} 位联系人发送 DHT 发现命令", count);
                        for contact in &contacts {
                            // 使用 try_send 写入到 cmd_tx 的 DiscoverContact 中
                            if let Err(e) = self.core_handle.cmd_tx.try_send(ChatCommand::DiscoverContact {
                                mldsa_pubkey_hex: contact.mldsa_pubkey_hex.clone(),
                                name: contact.name.clone(),
                            }) {
                                tracing::warn!("Failed to send DiscoverContact for {}: {e:?}", &contact.mldsa_pubkey_hex[..16]);
                            }
                        }
                        tracing::info!("启动后 DHT 发现命令发送完成，共 {} 位联系人", count);
                    }
                    Err(e) => {
                        tracing::warn!("启动后读取联系人列表失败: {}", e);
                    }
                }
            }
        }
    }

    /// 清理过期的 DHT 记录
    pub(crate) fn cleanup_expired_dht_records(&mut self) {
        let store = self.get_dht_store();
        let Ok(all_pubkeys) = store.get_all_pubkeys() else {
            return;
        };

        let mut stale_keys: Vec<String> = Vec::new();
        let mut stale_peer_ids: Vec<PeerId> = Vec::new();
        for pubkey in &all_pubkeys {
            let Some(peer_id) = store.get_peerid_by_pubkey(pubkey).ok().flatten() else {
                continue;
            };
            if !self.connected_peers.contains_key(&peer_id) {
                stale_keys.push(pubkey.clone());
                stale_peer_ids.push(peer_id);
            }
        }

        for key in &stale_keys {
            let _ = store.remove_pubkey_peerid(key);
        }
        for pid in &stale_peer_ids {
            self.peerid_to_mlkem.remove(pid);
        }

        // 清理过期的 DHT 查询键映射（保留最近发现的）
        let stale_query_keys: Vec<String> = self.dht_query_key_to_pubkey
            .keys()
            .filter(|k| {
                let pubkey = self.dht_query_key_to_pubkey.get(*k).unwrap();
                !all_pubkeys.contains(pubkey)
            })
            .cloned()
            .collect();
        for key in &stale_query_keys {
            self.dht_query_key_to_pubkey.remove(key);
        }

        tracing::debug!(
            "DHT cleanup tick (hourly): removed {} stale entries, {} stale query keys",
            stale_keys.len(),
            stale_query_keys.len()
        );
    }

    /// 发送 NetEvent 响应（支持 Ack / Nack，用于错误路径快速返回）
    fn send_netevent_response(
        &self,
        channel: libp2p::request_response::ResponseChannel<crate::p2p::netevent::NetEventResponse>,
        response: crate::p2p::netevent::NetEventResponse,
    ) {
        if let Err(e) = self.p2p_handle.tx.try_send(P2pCommand::SendNetEventResponse {
            channel,
            response,
        }) {
            tracing::warn!("Failed to send NetEventResponse: {e:?}");
        }
    }

    async fn handle_friend_online_request(
        &mut self,
        mldsa_pubkey_hex: String,
        claimed_peer_id: String,
        listen_addrs: Vec<String>,
        mlkem_pubkey_hex: String,
        version: Option<u8>,
        signature: Option<Vec<u8>>,
        peer: libp2p::PeerId,
        channel: libp2p::request_response::ResponseChannel<crate::p2p::netevent::NetEventResponse>,
    ) {
        if claimed_peer_id != peer.to_string() {
            tracing::warn!("FriendOnline PeerID 不匹配: 声称={}, 实际={}", claimed_peer_id, peer);
            self.send_netevent_response(
                channel,
                crate::p2p::netevent::NetEventResponse::Nack {
                    reason: crate::p2p::netevent::NackReason::PeerIdMismatch,
                },
            );
            return;
        }
        if let Some(v) = version {
            if v != crate::p2p::netevent::NETEVENT_VERSION {
                tracing::warn!("FriendOnline 协议版本不兼容: 期望=1, 实际={} (PeerID={})", v, peer);
                self.send_netevent_response(
                    channel,
                    crate::p2p::netevent::NetEventResponse::Nack {
                        reason: crate::p2p::netevent::NackReason::VersionMismatch {
                            expected: crate::p2p::netevent::NETEVENT_VERSION, got: v,
                        },
                    },
                );
                return;
            }
        }
        if let Some(sig) = &signature {
            if !crate::p2p::netevent::verify_friend_online_signature(
                &mldsa_pubkey_hex, &claimed_peer_id, &listen_addrs, &mlkem_pubkey_hex, sig,
            ) {
                let short = &mldsa_pubkey_hex[..16.min(mldsa_pubkey_hex.len())];
                tracing::warn!("FriendOnline 签名验证失败: 声称公钥 {}.. (PeerID={})", short, peer);
                self.send_netevent_response(
                    channel,
                    crate::p2p::netevent::NetEventResponse::Nack {
                        reason: crate::p2p::netevent::NackReason::SignatureVerificationFailed,
                    },
                );
                return;
            }
        }
        tracing::debug!("收到有效的 FriendOnline: {}.. (PeerID={})", &mldsa_pubkey_hex[..16.min(mldsa_pubkey_hex.len())], peer);
        let store = self.get_dht_store();
        let _ = store.set_pubkey_peerid(&mldsa_pubkey_hex, &peer);
        self.update_peerid_pubkey_mapping(peer, mldsa_pubkey_hex.clone()).await;
        if !mlkem_pubkey_hex.is_empty() {
            self.peerid_to_mlkem.insert(peer, mlkem_pubkey_hex.clone());
        }
        let owner_id = self.mldsa_identity_id.as_deref().unwrap_or("");
        if !owner_id.is_empty()
            && let Some(pool) = storage::pool() {
                if storage::is_contact_exists(pool, owner_id, &mldsa_pubkey_hex).await.unwrap_or(false) {
                    if let Ok(msgs) = storage::list_pending_by_peer(pool, &mldsa_pubkey_hex).await {
                        if !msgs.is_empty() {
                            self.retry_pending_messages(Some(&mldsa_pubkey_hex)).await;
                        }
                    }
                }
            }
        for addr_str in &listen_addrs {
            if let Ok(addr) = addr_str.parse::<libp2p::Multiaddr>() {
                let _ = self.p2p_handle.tx.try_send(P2pCommand::AddKademliaAddress { peer_id: peer, addr: addr.clone() });
                let _ = self.p2p_handle.tx.try_send(P2pCommand::DialAddr { addr });
            }
        }
        self.send_netevent_response(channel, crate::p2p::netevent::NetEventResponse::Ack);
    }
}
