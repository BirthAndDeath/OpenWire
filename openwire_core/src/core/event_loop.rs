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

        // Bootstrap 就绪后或 30 秒超时后发起联系人 DHT 发现
        let cmd_tx = self.core_handle.cmd_tx.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
            let _ = cmd_tx.try_send(ChatCommand::TimerDiscoverAllContacts);
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
                } = &request {
                    // 验证：声称的 PeerID 必须与实际连接的 PeerID 一致
                    if *claimed_peer_id != peer.to_string() {
                        tracing::warn!(
                            "FriendOnline PeerID 不匹配: 声称={}, 实际={}，忽略",
                            claimed_peer_id,
                            peer
                        );
                    } else {
                        tracing::debug!(
                            "收到有效的 FriendOnline: {}.. (PeerID={})",
                            &mldsa_pubkey_hex[..16],
                            peer
                        );

                        // 缓存 (ML-DSA 公钥 → PeerID) 映射到 DHT 存储
                        let store = self.get_dht_store();
                        let _ = store.set_pubkey_peerid(mldsa_pubkey_hex, &peer);
                        // 同步更新内存缓存，如果已连接则刷新在线状态
                        self.update_peerid_pubkey_mapping(peer, mldsa_pubkey_hex.clone())
                            .await;

                        // 缓存 ML-KEM 公钥（直接从 FriendOnline 获取，无需 DHT）
                        if !mlkem_pubkey_hex.is_empty() {
                            let store = self.get_dht_store();
                            let _ = store.set_mlkem_pubkey(mldsa_pubkey_hex, mlkem_pubkey_hex);
                        }

// 检查对方是否已在联系人列表中
                        let owner_id = self.mldsa_identity_id.as_deref().unwrap_or("");
                        if !owner_id.is_empty() {
                            if let Some(pool) = storage::pool() {
                                let is_known = storage::is_contact_exists(
                                    pool,
                                    owner_id,
                                    mldsa_pubkey_hex,
                                )
                                .await
                                .unwrap_or(false);

                                if !is_known {
                                    tracing::debug!(
                                        "FriendOnline 来自非联系人 {}..，仅缓存身份，跳过自动添加",
                                        &mldsa_pubkey_hex[..16]
                                    );
                                } else {
                                    // === Fix 7: FriendOnline 处理后检查并重试待发送消息 ===
                                    // ConnectionEstablished → retry_pending_messages 在 FriendOnline
                                    // 到达前运行，使用旧 DHT 存储找不到 PeerID 就跳过。
                                    // FriendOnline 到达后缓存了 (公钥→PeerID) 映射，此时重试能成功。
                                    match storage::list_pending_by_peer(pool, mldsa_pubkey_hex).await {
                                        Ok(msgs) => {
                                            if !msgs.is_empty() {
                                                tracing::debug!(
                                                    "FriendOnline 处理后 {}.. 有 {} 条待发消息，立即重试",
                                                    &mldsa_pubkey_hex[..16],
                                                    msgs.len()
                                                );
                                                self.retry_pending_for_peer(mldsa_pubkey_hex).await;
                                            }
                                        }
                                        Err(e) => tracing::warn!(
                                            "FriendOnline 处理后查询待发消息失败: {}", e
                                        ),
                                    }
                                }
                            }
                        }
                    }

                // 先尝试拨号对方监听地址（包括中继地址，用于 NAT 穿透）
                for addr_str in listen_addrs {
                    if let Ok(addr) = addr_str.parse::<libp2p::Multiaddr>() {
                        // 缓存到 DHT 缓存，供后续 Dial 命令使用
                        if let Err(e) = self.p2p_handle.tx.try_send(
                            P2pCommand::AddKademliaAddress { peer_id: peer, addr: addr.clone() },
                        ) {
                            tracing::warn!("Failed to send AddKademliaAddress for FriendOnline: {e:?}");
                        }
                        // 使用 try_send 避免阻塞
                        if let Err(e) = self.p2p_handle.tx.try_send(
                            P2pCommand::DialAddr { addr },
                        ) {
                            tracing::warn!("Failed to send DialAddr: {e:?}");
                        }
                    }
                }

// 发送响应确认（通过 P2pActor 发送 NetEvent 响应）
                if let Err(e) = self.p2p_handle.tx.try_send(
                    P2pCommand::SendNetEventResponse {
                        channel,
                        response: crate::p2p::netevent::NetEventResponse::Ack,
                    },
                ) {
                    tracing::warn!("Failed to send NetEventResponse: {e:?}");
                }
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
                // 无论对方是否在联系人列表中，都发送 FriendOnline 以完成身份交换。
                // 对方收到后由其 ChatCore 判断是否处理（仅已知联系人）。
                if let Some(mldsa_pubkey_hex) = self.mldsa_pubkey_hex.clone() {
                    if let Some(current_peer_id) = self.current_peer_id {
                        let mlkem = self.mlkem_pubkey_hex.clone().unwrap_or_default();
                        let friend_online =
                            crate::actor::p2p::netevent::build_friend_online_request(
                                &mldsa_pubkey_hex,
                                &current_peer_id,
                                &listen_addrs,
                                &mlkem,
                            );
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
                let store = self.get_dht_store();
                for provider in &providers {
                    let _ = store.set_pubkey_peerid(&key, provider);
                }

                // === 拨号每个发现的 provider，建立 P2P 连接 ===
                // GetProviders 从 DHT 查询到联系人 PeerID 后，Kademlia 路由表中已有其地址，
                // 通过 Dial 触发 libp2p 自动查找并连接
                for provider in &providers {
                    if !self.connected_peers.contains_key(provider) {
                        tracing::info!(
                            "DHT 发现联系人 {}..，正在拨号 PeerID={}",
                            &key[..16.min(key.len())],
                            provider
                        );
                        if let Err(e) = self.p2p_handle.tx.try_send(
                            P2pCommand::Dial { peer_id: *provider },
                        ) {
                            tracing::warn!("Failed to send Dial to provider {}: {e:?}", provider);
                        }
                    }
                }

                // 如果已连接的 PeerID 现在有了公钥映射，刷新在线状态
                // 这解决了 ConnectionEstablished 触发时 peerid_to_pubkey 尚未建立映射
                // 导致 send_online_status() 无法正确标记该联系人为在线的问题
                for provider in &providers {
                    if self.connected_peers.contains_key(provider) {
                        self.send_online_status().await;
                        break;
                    }
                }

                // 触发所有待发送消息的重试
                // 可能在连接建立时重试失败（PeerID 尚未缓存），
                // 现在 GetProviders 提供了正确的映射，再次重试
                self.retry_pending_messages().await;
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
                    let _ = store.set_mlkem_pubkey(&mldsa_pubkey_hex, &mlkem_pubkey_hex);
                }
                if !self.connected_peers.contains_key(&peer_id) {
                    if let Err(e) = self.p2p_handle.tx.try_send(P2pCommand::Dial { peer_id }) {
                        tracing::warn!("Failed to send Dial after DiscoverPeer: {e:?}");
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
            P2pEvent::BootstrapReady => {
                tracing::info!("DHT bootstrap ready, discovering contacts");
                self.discover_all_contacts().await;
            }
        }
    }

    /// 将当前身份发布到 DHT 网络（本地数据库 + 网络发布）
    pub(crate) fn publish_current_identity_to_dht(&mut self) {
        if let (Some(pubkey), Some(pid)) = (self.mldsa_pubkey_hex.clone(), self.current_peer_id) {
            let mlkem = self.mlkem_pubkey_hex.clone().unwrap_or_default();
            let store = self.get_dht_store();
            let _ = store.set_pubkey_peerid(&pubkey, &pid);
            if !mlkem.is_empty() {
                let _ = store.set_mlkem_pubkey(&pubkey, &mlkem);
            }
            if let Err(e) = self.core_handle.cmd_tx.try_send(ChatCommand::DhtPublishIdentity {
                mldsa_pubkey_hex: pubkey.clone(),
                peer_id: pid.to_string(),
                mlkem_pubkey_hex: mlkem,
            }) {
                tracing::warn!("Failed to send DhtPublishIdentity: {e:?}");
            }
            tracing::info!("Published current identity to DHT network");
        }
    }

    /// 对所有已添加的联系人发起 DHT 发现
    pub(crate) async fn discover_all_contacts(&self) {
        if let Some(pool) = storage::pool() {
            let owner_id = self.mldsa_identity_id.as_deref().unwrap_or("");
            if !owner_id.is_empty() {
                match storage::list_contacts(pool, owner_id).await {
                    Ok(contacts) => {
                        let count = contacts.len();
                        tracing::info!("启动后向 {} 位联系人发送 DHT 发现命令", count);
                        for contact in &contacts {
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
        // 清理不再连接的 PeerID 对应的 pubkey→peerid 映射
        let store = self.get_dht_store();
        if let Ok(all_pubkeys) = store.get_all_pubkeys() {
            for pubkey in &all_pubkeys {
                if let Ok(Some(peer_id)) = store.get_peerid_by_pubkey(pubkey) {
                    if !self.connected_peers.contains_key(&peer_id) {
                        let _ = store.remove_pubkey_peerid(pubkey);
                        let _ = store.remove_mlkem_pubkey(pubkey);
                    }
                }
            }
        }
        tracing::debug!("DHT cleanup tick (hourly): removed stale entries");
    }
}
