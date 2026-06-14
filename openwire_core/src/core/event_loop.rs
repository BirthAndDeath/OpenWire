use crate::actor::RUNTIME as rt;
use redb::Database;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::{
    actor::p2p::{P2pCommand, P2pEvent},
    command::ChatCommand,
    core::ChatCore,
    p2p, storage,
};

impl ChatCore {
    /// 启动核心事件循环（使用独立的 Tokio 运行时）
    pub fn run(mut self) -> std::thread::JoinHandle<()> {
        let handle = rt.handle().clone();
        std::thread::spawn(move || {
            rt.block_on(async move {
                self.run_inner(&handle).await;
            });
        })
    }

    /// 启动核心事件循环（使用调用方提供的 Tokio 运行时句柄）
    pub fn run_on_runtime(
        mut self,
        rt_handle: tokio::runtime::Handle,
    ) -> std::thread::JoinHandle<()> {
        let handle = rt_handle.clone();
        let handle_for_block = handle.clone();
        let handle_for_inner = handle.clone();
        std::thread::spawn(move || {
            let _guard = handle.enter();
            handle_for_block.block_on(async move {
                self.run_inner(&handle_for_inner).await;
            });
        })
    }

    /// 内部事件循环：DHT 注册 + 主循环
    async fn run_inner(&mut self, rt_handle: &tokio::runtime::Handle) {
        // 启动 DHT 定期注册任务
        let dht_reg_cmd_tx = self.core_handle.cmd_tx.clone();
        self.spawn_dht_registration(rt_handle);

        // 启动后立即执行一次 DHT 身份发布
        self.publish_current_identity_to_dht(&dht_reg_cmd_tx);

        // 启动后对所有已添加的联系人发起 DHT 发现（非阻塞）
        self.discover_all_contacts(&dht_reg_cmd_tx).await;

        // 主事件循环：处理 P2pActor 事件和控制命令
        // 注意：消息重试仅在 ConnectionEstablished 事件中触发，
        // 不在定时器中重试，避免对方离线时频繁无效查询。

        // DHT 清理间隔：每小时清理一次过期记录
        let mut dht_cleanup_interval = tokio::time::interval(std::time::Duration::from_secs(3600));
        dht_cleanup_interval.tick().await; // 跳过首次立即触发

        // 路由表持久化间隔：每 5 分钟保存一次，确保运行期间缓存持续更新
        let mut routing_table_save_interval =
            tokio::time::interval(std::time::Duration::from_secs(300));
        routing_table_save_interval.tick().await; // 跳过首次立即触发

        // === 主动连接维护间隔：每 5 分钟对所有联系人发起 DHT GetProviders 查询 ===
        let mut connection_maintenance_interval =
            tokio::time::interval(std::time::Duration::from_secs(300));
        connection_maintenance_interval.tick().await; // 跳过首次立即触发（启动时已调用 discover_all_contacts）

        loop {
            tokio::select! {
                // 从 P2pActor 接收网络事件
                Some(event) = self.rx_p2p_event.recv() => {
                    self.handle_p2p_event(event).await;
                }
                Some(cmd) = self.rx_cmd.recv() => {
                    if matches!(cmd, ChatCommand::Shutdown) {
                        tracing::info!("P2P core shutting down...");
                        // 通知 P2pActor 保存路由表并关闭
                        let _ = self.p2p_handle.send(
                            crate::actor::ActorCommand::Custom(P2pCommand::SaveRoutingTable),
                        ).await;
                        let _ = self.p2p_handle.send(
                            crate::actor::ActorCommand::Custom(P2pCommand::Shutdown),
                        ).await;
                        break;
                    }
                    // 处理身份切换：更新 DHT 注册循环的身份信息
                    if matches!(cmd, ChatCommand::SelectIdentity { .. }) {
                        self.handle_command(cmd).await;
                        self.publish_current_identity_to_dht(&dht_reg_cmd_tx);
                    } else {
                        self.handle_command(cmd).await;
                    }
                }
                _ = dht_cleanup_interval.tick() => {
                    self.cleanup_expired_dht_records();
                }
                _ = routing_table_save_interval.tick() => {
                    // 通知 P2pActor 保存路由表（使用 try_send 避免阻塞）
                    let _ = self.p2p_handle.tx.try_send(
                        crate::actor::ActorCommand::Custom(P2pCommand::SaveRoutingTable),
                    );
                }
                _ = connection_maintenance_interval.tick() => {
                    // 定期对所有联系人发起 DHT GetProviders 查询
                    // 这是非阻塞的：get_providers 只是发起网络查询，不等待结果
                    // 结果通过 P2pEvent::GetProvidersResult 事件处理
                    self.discover_all_contacts(&dht_reg_cmd_tx).await;
                    // 连接维护后触发离线消息重试
                    let _ = dht_reg_cmd_tx.try_send(ChatCommand::RetryPendingMessages);
                }
                else => break,
            }
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
                tracing::info!("收到 NetEvent 请求: peer={}, request={:?}", peer, request);

                // === Fix 2: 处理 FriendOnline 通知：缓存对方身份 + 触发反向发现 ===
                // FriendOnline 直接携带所有身份信息（ML-DSA 公钥、PeerID、ML-KEM 公钥），
                // 无需等待 DHT 查询，连接建立后立即可用。
                let crate::p2p::netevent::NetEventRequest::FriendOnline {
                    mldsa_pubkey_hex,
                    peer_id: claimed_peer_id,
                    listen_addrs: _,
                    mlkem_pubkey_hex,
                } = &request;
                    // 验证：声称的 PeerID 必须与实际连接的 PeerID 一致
                    if *claimed_peer_id != peer.to_string() {
                        tracing::warn!(
                            "FriendOnline PeerID 不匹配: 声称={}, 实际={}，忽略",
                            claimed_peer_id,
                            peer
                        );
                    } else {
                        tracing::info!(
                            "收到有效的 FriendOnline: {}.. (PeerID={})",
                            &mldsa_pubkey_hex[..16],
                            peer
                        );

                        // 缓存 (ML-DSA 公钥 → PeerID) 映射到 DHT 存储
                        if let Ok(store) = self.get_dht_store() {
                            let _ = store.set_pubkey_peerid(mldsa_pubkey_hex, &peer);
                        }
                        // 同步更新内存缓存，如果已连接则刷新在线状态
                        self.update_peerid_pubkey_mapping(peer, mldsa_pubkey_hex.clone())
                            .await;

                        // 缓存 ML-KEM 公钥（直接从 FriendOnline 获取，无需 DHT）
                        if !mlkem_pubkey_hex.is_empty() {
                            if let Ok(store) = self.get_dht_store() {
                                let _ = store.set_mlkem_pubkey(mldsa_pubkey_hex, mlkem_pubkey_hex);
                            }
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

                                if !is_known && !mlkem_pubkey_hex.is_empty() {
                                    tracing::info!(
                                        "FriendOnline 来自未知联系人 {}..，FriendOnline 携带了 ML-KEM，直接自动添加",
                                        &mldsa_pubkey_hex[..16]
                                    );
                                    // 直接从 FriendOnline 获取的信息添加联系人，无需 DHT
                                    let mlkem_bytes = hex::decode(mlkem_pubkey_hex.as_str())
                                        .unwrap_or_default();
                                    if !mlkem_bytes.is_empty() {
                                        self.add_contact(
                                            mldsa_pubkey_hex.to_string(),
                                            mlkem_bytes,
                                            None,
                                        )
                                        .await;
                                        let msg = format!(
                                            "已自动添加联系人: {}..（通过 FriendOnline）",
                                            &mldsa_pubkey_hex[..16]
                                        );
                                        self.send_log_mpsc(msg).await;
                                    }
                                }

                                // === Fix 7: FriendOnline 处理后检查并重试待发送消息 ===
                                // ConnectionEstablished → retry_pending_messages 在 FriendOnline
                                // 到达前运行，使用旧 DHT 存储找不到 PeerID 就跳过。
                                // FriendOnline 到达后缓存了 (公钥→PeerID) 映射，此时重试能成功。
                                match storage::list_pending(pool).await {
                                    Ok(msgs) => {
                                        let has_pending = msgs.iter().any(|m| {
                                            m.peer_pubkey_hex == *mldsa_pubkey_hex
                                        });
                                        if has_pending {
                                            tracing::info!(
                                                "FriendOnline 处理后 {}.. 有待发消息，立即重试",
                                                &mldsa_pubkey_hex[..16]
                                            );
                                            self.retry_pending_messages().await;
                                        }
                                    }
                                    Err(e) => tracing::warn!(
                                        "FriendOnline 处理后查询待发消息失败: {}", e
                                    ),
                                }
                            }
                        }
                    }

                // 发送响应确认（通过 P2pActor 发送 NetEvent 响应）
                let _ = self
                    .p2p_handle
                    .send(crate::actor::ActorCommand::Custom(
                        P2pCommand::SendNetEventResponse {
                            channel,
                            response: crate::p2p::netevent::NetEventResponse::Ack,
                        },
                    ))
                    .await;
            }
            P2pEvent::ConnectionEstablished { peer_id } => {
                tracing::info!("Connection established with {}", peer_id);
                self.connected_peers.insert(peer_id);

                // 从 DHT 反向查找该 PeerID 对应的 ML-DSA 公钥
                if let Ok(store) = self.get_dht_store() {
                    match store.get_pubkey_by_peerid(&peer_id) {
                        Ok(Some(pubkey_hex)) => {
                            self.peerid_to_pubkey.insert(peer_id, pubkey_hex);
                        }
                        _ => {
                            // Fix 3: 移除错误的 GetProviders 查询（用 PeerID 查 DHT 无意义）
                            // 后续 FriendOnline 通知或 DHT 发现会提供正确的 (公钥→PeerID) 映射
                            tracing::debug!(
                                "ConnectionEstablished: 本地未缓存 PeerID {} 对应的公钥",
                                peer_id
                            );
                        }
                    }
                }

                // === Fix 1: 连接建立后向对方发送 FriendOnline 通知（身份交换）===
                if let Some(mldsa_pubkey_hex) = self.mldsa_pubkey_hex.clone() {
                    if let Some(current_peer_id) = self.current_peer_id {
                        let mlkem = self.mlkem_pubkey_hex.clone().unwrap_or_default();
                        let friend_online =
                            crate::actor::p2p::netevent::build_friend_online_request(
                                &mldsa_pubkey_hex,
                                &current_peer_id,
                                &[], // 监听地址 - ChatCore 暂未持久化存储
                                &mlkem,
                            );
                        let _ = self
                            .p2p_handle
                            .send(crate::actor::ActorCommand::Custom(
                                P2pCommand::SendNetEvent {
                                    peer_id,
                                    request: friend_online,
                                },
                            ))
                            .await;
                        tracing::info!(
                            "已向 {} 发送 FriendOnline 通知 (PubKey={}..)",
                            peer_id,
                            &mldsa_pubkey_hex[..16]
                        );
                    }
                }

                // 触发在线状态更新
                self.send_online_status().await;

                // 连接建立后重试待发送消息
                self.retry_pending_messages().await;
            }
            P2pEvent::ConnectionClosed { peer_id } => {
                tracing::info!("Connection closed with {}", peer_id);
                self.connected_peers.remove(&peer_id);
                self.peerid_to_pubkey.remove(&peer_id);
                // 触发在线状态更新
                self.send_online_status().await;
            }
            P2pEvent::MdnsDiscovered { peer_id, addr } => {
                tracing::info!("mDNS discovered: {} at {}", peer_id, addr);
                let _ = self
                    .p2p_handle
                    .send(crate::actor::ActorCommand::Custom(
                        P2pCommand::AddKademliaAddress { peer_id, addr },
                    ))
                    .await;
            }
            P2pEvent::MdnsExpired { peer_id } => {
                tracing::info!("mDNS expired: {}", peer_id);
            }
            P2pEvent::IdentifyReceived {
                peer_id,
                listen_addrs,
            } => {
                for addr in listen_addrs {
                    let _ = self
                        .p2p_handle
                        .send(crate::actor::ActorCommand::Custom(
                            P2pCommand::AddKademliaAddress { peer_id, addr },
                        ))
                        .await;
                }
            }
            P2pEvent::GetProvidersResult { key, providers } => {
                tracing::debug!(
                    "GetProviders result: key={}.., providers={:?}",
                    &key[..16.min(key.len())],
                    providers
                );
                // 缓存到本地 DHT 数据库
                if let Ok(store) = self.get_dht_store() {
                    for provider in &providers {
                        let _ = store.set_pubkey_peerid(&key, provider);
                        self.peerid_to_pubkey.insert(*provider, key.clone());
                    }
                }
                // 如果有 provider，尝试拨号连接
                if let Some(peer_id) = providers.first() {
                    let _ = self
                        .p2p_handle
                        .send(crate::actor::ActorCommand::Custom(P2pCommand::Dial {
                            peer_id: *peer_id,
                        }))
                        .await;
                }
                // 如果已连接的 PeerID 现在有了公钥映射，刷新在线状态
                // 这解决了 ConnectionEstablished 触发时 peerid_to_pubkey 尚未建立映射
                // 导致 send_online_status() 无法正确标记该联系人为在线的问题
                for provider in &providers {
                    if self.connected_peers.contains(provider) {
                        self.send_online_status().await;
                        break;
                    }
                }
            }
            P2pEvent::GetRecordResult { key, value } => {
                tracing::debug!(
                    "GetRecord result: key={}.., value_len={}",
                    &key[..16.min(key.len())],
                    value.len()
                );
                // 缓存到本地 DHT 数据库
                if let Ok(store) = self.get_dht_store()
                    && key.starts_with("mlkem:")
                {
                    let pubkey_hex = key.strip_prefix("mlkem:").unwrap_or("");
                    if !pubkey_hex.is_empty() {
                        let mlkem_hex = String::from_utf8_lossy(&value);
                        let _ = store.set_mlkem_pubkey(pubkey_hex, &mlkem_hex);

                        // === 反向发现：收到 ML-KEM 密钥后，检查是否需要自动添加对方 ===
                        let owner_id = self.mldsa_identity_id.as_deref().unwrap_or("");
                        if !owner_id.is_empty() {
                            if let Some(pool) = storage::pool() {
                                let is_known = storage::is_contact_exists(
                                    pool,
                                    owner_id,
                                    pubkey_hex,
                                )
                                .await
                                .unwrap_or(false);

                                if !is_known {
                                    let has_peerid = store
                                        .get_peerid_by_pubkey(pubkey_hex)
                                        .ok()
                                        .flatten()
                                        .is_some();
                                    // Fix: 不能直接读刚 set 的值（总是 Some），需检查 hex 有效性
                                    let mlkem_hex_str = mlkem_hex.as_ref();
                                    let has_valid_mlkem = !mlkem_hex_str.is_empty()
                                        && hex::decode(mlkem_hex_str).is_ok();

                                    if has_peerid && has_valid_mlkem {
                                        tracing::info!(
                                            "FriendOnline 反向发现完成，自动添加联系人 {}..",
                                            &pubkey_hex[..16]
                                        );
                                        let mlkem_bytes = hex::decode(mlkem_hex.as_ref())
                                            .unwrap_or_default();
                                        self.add_contact(
                                            pubkey_hex.to_string(),
                                            mlkem_bytes,
                                            None,
                                        )
                                        .await;
                                        let msg = format!(
                                            "已自动添加联系人: {}..（通过 FriendOnline）",
                                            &pubkey_hex[..16]
                                        );
                                        self.send_log_mpsc(msg).await;
                                    }
                                }
                            }
                        }
                    }
                }
            }
            P2pEvent::Log(msg) => {
                tracing::info!("P2pActor: {}", msg);
            }
        }
    }

    /// 将当前身份发布到 DHT 网络（本地数据库 + 网络发布）
    fn publish_current_identity_to_dht(&mut self, cmd_tx: &mpsc::Sender<ChatCommand>) {
        if let (Some(pubkey), Some(pid)) = (self.mldsa_pubkey_hex.clone(), self.current_peer_id) {
            let mlkem = self.mlkem_pubkey_hex.clone().unwrap_or_default();
            if let Ok(store) = self.get_dht_store() {
                let _ = store.set_pubkey_peerid(&pubkey, &pid);
                if !mlkem.is_empty() {
                    let _ = store.set_mlkem_pubkey(&pubkey, &mlkem);
                }
            }
            let _ = cmd_tx.try_send(ChatCommand::DhtPublishIdentity {
                mldsa_pubkey_hex: pubkey.clone(),
                peer_id: pid.to_string(),
                mlkem_pubkey_hex: mlkem,
            });
            tracing::info!("Published current identity to DHT network");
        }
    }

    /// 对所有已添加的联系人发起 DHT 发现
    async fn discover_all_contacts(&self, cmd_tx: &mpsc::Sender<ChatCommand>) {
        if let Some(pool) = storage::pool() {
            let owner_id = self.mldsa_identity_id.as_deref().unwrap_or("");
            if !owner_id.is_empty() {
                match storage::list_contacts(pool, owner_id).await {
                    Ok(contacts) => {
                        let count = contacts.len();
                        tracing::info!("启动后向 {} 位联系人发送 DHT 发现命令", count);
                        for contact in &contacts {
                            let _ = cmd_tx.try_send(ChatCommand::DiscoverContact {
                                mldsa_pubkey_hex: contact.mldsa_pubkey_hex.clone(),
                                name: contact.name.clone(),
                            });
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

    /// 启动 DHT 定期注册后台任务
    fn spawn_dht_registration(&self, rt_handle: &tokio::runtime::Handle) {
        let cmd_tx = self.core_handle.cmd_tx.clone();
        let db = match self.dht_db.clone() {
            Some(db) => db,
            None => {
                tracing::error!("DHT database not initialized, DHT registration disabled");
                return;
            }
        };
        let shutdown_token = self.core_handle.shutdown_token.clone();

        let handle = rt_handle.clone();
        tokio::task::spawn_blocking(move || {
            handle.block_on(async move {
                Self::dht_registration_loop(cmd_tx, db, shutdown_token).await;
            });
        });
    }

    /// DHT 定期注册循环（每 5 分钟执行一次）
    pub(crate) async fn dht_registration_loop(
        cmd_tx: mpsc::Sender<ChatCommand>,
        db: Arc<Database>,
        shutdown_token: CancellationToken,
    ) {
        use crate::core::DHT_REGISTRATION_INTERVAL_SECS;

        let mut interval = tokio::time::interval(std::time::Duration::from_secs(
            DHT_REGISTRATION_INTERVAL_SECS,
        ));

        loop {
            tokio::select! {
                biased;

                _ = shutdown_token.cancelled() => {
                    tracing::info!("DHT registration loop shutting down gracefully");
                    break;
                }
                _ = interval.tick() => {}
            }

            let store = p2p::dht::RedbRecordStore::new(db.clone());

            let pubkeys = match store.get_all_pubkeys() {
                Ok(keys) => keys,
                Err(e) => {
                    tracing::warn!("Failed to read pubkeys from DHT database: {}", e);
                    continue;
                }
            };

            for pubkey in &pubkeys {
                let pid = match store.get_peerid_by_pubkey(pubkey) {
                    Ok(Some(pid)) => pid,
                    _ => continue,
                };

                let mlkem_hex = match store.get_mlkem_pubkey(pubkey) {
                    Ok(Some(mlkem)) => Some(mlkem),
                    _ => None,
                };

                if let Err(e) = store.set_pubkey_peerid(pubkey, &pid) {
                    tracing::warn!("Failed to refresh DHT registration: {}", e);
                }

                if let Some(ref mlkem) = mlkem_hex
                    && let Err(e) = store.set_mlkem_pubkey(pubkey, mlkem)
                {
                    tracing::warn!("Failed to publish ML-KEM pubkey: {}", e);
                }

                if let Err(e) = cmd_tx.try_send(ChatCommand::DhtPublishIdentity {
                    mldsa_pubkey_hex: pubkey.clone().to_owned(),
                    peer_id: pid.to_string(),
                    mlkem_pubkey_hex: mlkem_hex.unwrap_or_default(),
                }) {
                    tracing::warn!("Failed to send DHT publish command: {:?}", e);
                }
            }
        }
    }

    /// 定期清理过期DHT记录
    fn cleanup_expired_dht_records(&mut self) {
        if let Ok(store) = self.get_dht_store() {
            match store.cleanup_expired_records() {
                Ok((records_cleaned, providers_cleaned)) => {
                    if records_cleaned > 0 || providers_cleaned > 0 {
                        tracing::info!(
                            "DHT cleanup: removed {} expired records and {} expired providers",
                            records_cleaned,
                            providers_cleaned
                        );
                    }
                }
                Err(e) => {
                    tracing::warn!("Failed to cleanup expired DHT records: {}", e);
                }
            }
        }
    }
}
