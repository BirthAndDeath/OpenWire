use libp2p::PeerId;
use sha2::Digest;

use crate::actor::RUNTIME as rt;

use crate::{
    actor::p2p::{P2pCommand, P2pEvent},
    command::ChatCommand,
    core::{timers, ChatCore},
    storage,
};

/// connected_peers 防御上限（真实并发连接远小于此值）
const MAX_CONNECTED_PEERS: usize = 4096;

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

        // 启动时清理超期已发送文件记录（授权表保留期，默认 90 天）
        if let Some(pool) = storage::pool()
            && let Err(e) = storage::delete_sent_files_older_than(
                pool,
                storage::SENT_FILE_RETENTION_DAYS,
            )
            .await
        {
            tracing::warn!("清理超期已发送文件记录失败: {e}");
        }

        // 启动时自动续传未完成的文件下载（经命令队列，先于常规命令处理）
        if let Err(e) = self
            .core_handle
            .cmd_tx
            .try_send(ChatCommand::ResumeFileTransfers)
        {
            tracing::warn!("Failed to queue ResumeFileTransfers: {e:?}");
        }

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
        // === 设备下线通知（尽力而为，先于 Shutdown 入队）===
        // 优雅退出前，向所有已反向解析出公钥（即好友设备）的对端发送 FriendOffline，
        // 让对端立刻从该账户的在线设备集合中移除本设备，而非等待 TCP/中继连接超时。
        // remaining_peers 尽力留空：对端以自己本地已连接的该账户设备集合为准计算剩余设备。
        if let (Some(pubkey_hex), Some(peer_id)) =
            (self.mldsa_pubkey_hex.clone(), self.current_peer_id)
            && let Some(signing_key) = self.mldsa_private_key.as_ref()
        {
            let request =
                crate::actor::p2p::netevent::build_friend_offline_request(
                    &pubkey_hex,
                    &peer_id,
                    &[],
                    signing_key,
                );
            if let Some(req) = request {
                for connected in self.connected_peers.keys() {
                    if self.peer_cache.peerid_to_pubkey.contains_key(connected)
                        && let Err(e) = self.p2p_handle.tx.try_send(
                            P2pCommand::SendNetEvent {
                                peer_id: *connected,
                                request: req.clone(),
                            },
                        )
                    {
                        tracing::debug!("FriendOffline 发送失败(停止中): {e:?}");
                    }
                }
                tracing::info!("已向 {} 个已连接好友广播 FriendOffline", self.connected_peers.len());
            }
        }
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

                // 设备下线通知：验证签名后移除该设备并刷新该账户在线状态
                if let crate::p2p::netevent::NetEventRequest::FriendOffline {
                    mldsa_pubkey_hex,
                    peer_id: claimed_peer_id,
                    remaining_peers,
                    signature,
                    ..
                } = &request {
                    self.handle_friend_offline_request(
                        mldsa_pubkey_hex.clone(),
                        claimed_peer_id.clone(),
                        remaining_peers.clone(),
                        signature.clone(),
                        peer,
                        channel,
                    ).await;
                    return;
                }

                // 未知/不支持请求变体：发送 Nack 立即释放流槽，
                // 避免占用协议并发流直到 15s 超时
                tracing::warn!("收到不支持 NetEvent 请求变体: {:?}", request);
                self.send_netevent_response(channel, crate::p2p::netevent::NetEventResponse::Nack {
                    reason: crate::p2p::netevent::NackReason::Other {
                        description: "不支持的请求类型".to_string(),
                    },
                });
            }

            P2pEvent::ConnectionEstablished { peer_id, listen_addrs } => {
                tracing::info!("Connection established with {} ({} listen addrs)", peer_id, listen_addrs.len());
                // 缓存本节点监听地址：ConnectionEstablished 事件由 P2pActor 携带本机
                // listen_addrs 列表；FriendOnline 重发（TimerRefreshPresence 补偿）
                // 需要它做身份声明负载。
                self.own_listen_addrs = listen_addrs.clone();
                // 防御上限：连接事件因通道满偶发丢失时，防止条目无限累积。
                // 注：HashMap 不保序，keys().next() 是任意键而非最旧条目，
                // 此处仅作容量兜底，淘汰任意键即可。
                if !self.connected_peers.contains_key(&peer_id)
                    && self.connected_peers.len() >= MAX_CONNECTED_PEERS
                    && let Some(oldest) = self.connected_peers.keys().next().copied()
                {
                    self.connected_peers.remove(&oldest);
                }
                self.connected_peers.entry(peer_id).and_modify(|c| *c += 1).or_insert(1);

                // 入站限流首窗突发：仅对该 peer 的"首个连接"授予（覆盖重连后的离线积压），
                // 建立全新的突发窗口；非首个连接（同 peer 双连接建立）不重置，
                // 防止反复断连重连绕过稳定限流（40/10s）。
                if self.connected_peers.get(&peer_id) == Some(&1) {
                    self.inbound_msg_windows
                        .insert(peer_id, (std::time::Instant::now(), 1, true));
                }

                // 从 DHT 反向查找该 PeerID 对应的 ML-DSA 公钥
                let store = self.get_dht_store();
                match store.get_pubkey_by_peerid(&peer_id) {
                    Some(pubkey_hex) => {
                        self.peer_cache.peerid_to_pubkey.insert(peer_id, pubkey_hex);
                    }
                    _ => {
                        tracing::debug!(
                            "ConnectionEstablished: 本地未缓存 PeerID {} 对应的公钥",
                            peer_id
                        );
                    }
                }

// === Fix 1: 连接建立后向对方发送 FriendOnline 通知（身份交换）===
                // 向对端发送 FriendOnline。非 OpenWire 节点（如 IPFS 引导节点）
                // 不支持 NetEvent 协议，发送会报 UnsupportedProtocols 错误，在 P2pActor
                // 中静默处理，不影响其他流程。
                self.send_friend_online(&peer_id).await;

                // 触发在线状态更新
                self.send_online_status().await;
                // 注意：不再在 ConnectionEstablished 时立即重试，改由 FriendOnline 到达后重试
                // 这样可以确保身份信息（ML-KEM 公钥、PeerID 映射）已就绪后再发送
            }
            P2pEvent::ConnectionClosed { peer_id } => {
                tracing::info!("Connection closed with {}", peer_id);
                // 使用连接计数：减到 0 才真正移除，防止双连接误判离线
                let removed_pubkey = if let std::collections::hash_map::Entry::Occupied(mut entry) =
                    self.connected_peers.entry(peer_id)
                {
                    *entry.get_mut() = entry.get().saturating_sub(1);
                    if *entry.get() == 0 {
                        entry.remove();
                        let pk = self.peer_cache.peerid_to_pubkey.remove(&peer_id);
                        // 多设备集合维护：设备断开 → 从该账号在线设备列表移除
                        if let Some(pk) = &pk {
                            self.get_dht_store().remove_pubkey_device(pk, &peer_id);
                        }
                        pk
                    } else {
                        None
                    }
                } else {
                    None
                };
                // 定向通知该联系人（下线）状态，含剩余设备集合
                if let Some(pk) = removed_pubkey {
                    self.emit_contact_status(pk).await;
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

                // === 启动自检：查询自己账号设备的 GetProviders 结果 ===
                // 命中 pending_own_device_query 时，providers 是该账号当前在线设备集合
                // （含本机自身，因为本机 start_providing 已注册）。排除本机后：
                // - 有其他设备在线 → 账号整体已在网络，跳过联系人全量发现（设备同步后续处理）
                // - 仅本机/无设备 → 账号冷启动，执行联系人全量 DHT 发现
                if let Some((own_key, own_peer)) = self.pending_own_device_query.take()
                    && own_key == key
                {
                    let other_devices: Vec<&libp2p::PeerId> = providers
                        .iter()
                        .filter(|p| **p != own_peer)
                        .collect();
                    if other_devices.is_empty() {
                        tracing::info!(
                            "启动自检：本账号仅本机在线（冷启动），执行联系人全量发现"
                        );
                        self.discover_all_contacts().await;
                    } else {
                        tracing::info!(
                            "启动自检：本账号另有 {} 台设备在线 {:?}，跳过联系人全量发现（待设备同步）",
                            other_devices.len(),
                            other_devices
                        );
                        // 拨号各在线设备，由 ConnectionEstablished 触发 FriendOnline 互认
                        for provider in other_devices {
                            if !self.connected_peers.contains_key(provider)
                                && let Err(e) = self.p2p_handle.tx.try_send(
                                    P2pCommand::Dial { peer_id: *provider },
                                )
                            {
                                tracing::warn!(
                                    "Failed to dial own device {}: {e:?}",
                                    provider
                                );
                            }
                        }
                    }
                    return;
                }

                if providers.is_empty() {
                    return;
                }

                // 仅用于拨号标签，不写入身份映射（身份映射仅由 FriendOnline 签名验证后建立）
                let actual_pubkey = self.peer_cache.dht_key_to_pubkey.get(&key).cloned();

                // === 拨号每个发现的 provider，建立 P2P 连接 ===
                for provider in &providers {
                    if !self.connected_peers.contains_key(provider) {
                        let label = actual_pubkey.as_ref().map_or(
                            format!("{}..", &key[..16.min(key.len())]),
                            |p| format!("{}..", &p[..16.min(p.len())]),
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

                // 触发待发送消息的重试：优先定向到该 key 对应的联系人，
                // 避免每次 DHT 结果都全表扫描 pending 消息（N 联系人时 O(N×待发) 累积）
                match actual_pubkey.as_deref() {
                    Some(pubkey) => self.retry_pending_messages(Some(pubkey)).await,
                    None => self.retry_pending_messages(None).await,
                }
            }
            P2pEvent::PeerInfoReceived {
                mldsa_pubkey_hex,
                peer_id,
                mlkem_pubkey_hex: _,
            } => {
                tracing::info!("=== DISCOVER_PEER OK: {}.. → {} ===", &mldsa_pubkey_hex[..16.min(mldsa_pubkey_hex.len())], peer_id);
                // PeerInfo 仅作拨号提示：未签名、无法证明持有私钥，直接落加密映射会被恶意中继投毒破坏 E2EE。
                // 加密映射的唯一权威来源是签名验证过的 FriendOnline。
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
            P2pEvent::MessageSent { peer: _, message_hash } => {
                // P2P 层确认消息已发送，标记为已发送
                if let Some(pool) = storage::pool() {
                    let _ = storage::mark_sent_by_hash(pool, &message_hash).await;
                    tracing::debug!("P2P 确认消息 {}.. 发送成功", &message_hash[..16.min(message_hash.len())]);
                }
            }
            P2pEvent::MessageSendFailed { peer: _, message_hash } => {
                tracing::warn!("P2P 发送消息 {}.. 失败（消息已保持待发送状态，等待重试）", &message_hash[..16.min(message_hash.len())]);
            }
            P2pEvent::FileDownloadReject {
                pubkey_hex,
                file_hash,
                reason,
            } => {
                tracing::warn!(
                    "文件发送中止，向下载方回发拒绝: file={}.., reason={reason}",
                    &hex::encode(file_hash)[..16]
                );
                // 复用现有拒绝路径：让下载方立即 cancel，而非等 scan_timeout_transfers 30 分钟
                crate::p2p::send_reject_response(&mut *self, &pubkey_hex, file_hash, &reason).await;
            }
            P2pEvent::IncomingFileChunk { chunk, peer, channel } => {
                // 注意：联系人鉴权、签名验证与解密均在 handle_incoming_file_chunk 内完成
                let accepted = self
                    .handle_incoming_file_chunk(&chunk, &peer)
                    .await;
                // 校验成功才回 Ack（true），失败回 false，给发送方重试/中止依据。
                // 用带短超时的 send().await：通道满时短暂等待而非立即丢弃，
                // 显著降低"接收方繁忙 → ack 丢失 → 发送方重试耗尽后整单中止"的概率。
                let response = P2pCommand::SendFileChunkResponse { channel, accepted };
                match tokio::time::timeout(
                    std::time::Duration::from_secs(5),
                    self.p2p_handle.tx.send(response),
                )
                .await
                {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => {
                        tracing::warn!("回送文件分片确认失败（通道已关闭）: {e:?}");
                    }
                    Err(_) => {
                        tracing::warn!(
                            "回送文件分片确认超时（5s），丢弃确认（发送方将按 rr_file 超时重试）"
                        );
                    }
                }
            }
            P2pEvent::BootstrapReady => {
                self.bootstrap_ready = true;
                tracing::info!("DHT bootstrap ready, retrying pending identity publish");
                // bootstrap 就绪前缓存的身份发布只保留一条，此处重发一次
                if let Some(pubkey) = self.pending_publish_pubkey.take()
                    && let Err(e) = self.core_handle.cmd_tx.try_send(ChatCommand::DhtPublishIdentity {
                        mldsa_pubkey_hex: pubkey,
                    })
                {
                    tracing::warn!("Failed to retry pending identity publish: {e:?}");
                }
                // 启动 Presence 引导：先确认自己账号是否有其他设备在线，
                // 有则跳过全量联系人发现（改由设备同步），无则冷启动全量发现。
                self.discover_own_devices().await;
            }
        }
    }

    /// 处理入站文件分片：联系人鉴权 → 签名验证 + 解密 → 写入。
    ///
    /// 返回是否接受该分片（决定 rr_file 回 Ack 值）。
    /// 无联系人关系、无法解密或写入失败的都返回 false，防止陌生节点写盘。
    async fn handle_incoming_file_chunk(
        &mut self,
        chunk: &crate::message::EncryptedFileChunk,
        peer: &libp2p::PeerId,
    ) -> bool {
        // 0. 每 peer 入站分片滑窗限流：超限即拒绝（Ack=false，发送方按 rr_file 超时重试）。
        // 放在最前：不消耗 ML-DSA 验签/AES 解封/磁盘写入，仅用轻量计数拦截冲刷。
        if self.inbound_file_chunk_rate_limited(peer) {
            tracing::warn!("入站文件分片速率超限，拒绝来自 {} 的分片", peer);
            return false;
        }

        // 1. 通过 DHT 缓存反向查找 PeerID 对应的联系人公钥
        let store = self.get_dht_store();
        let Some(pubkey_hex) = store.get_pubkey_by_peerid(peer) else {
            tracing::warn!("拒绝无公钥映射的节点文件分片（来自 {}）", peer);
            return false;
        };

        // 2. 联系人鉴权：仅接受通讯录内联系人的分片（缓存避免每分片重复 SQL）
        let now = std::time::Instant::now();
        let cached_ok = self
            .contact_verified_cache
            .get(&pubkey_hex)
            .is_some_and(|t| now.duration_since(*t) <= super::super::CONTACT_VERIFIED_TTL);
        let is_known = if cached_ok {
            true
        } else {
            match storage::pool() {
                Some(pool) => {
                    let owner = self.mldsa_identity_id.as_deref().unwrap_or("");
                    storage::is_contact_exists(pool, owner, &pubkey_hex)
                        .await
                        .unwrap_or(false)
                }
                None => false,
            }
        };
        if !is_known {
            tracing::warn!("拒绝未知联系人的文件分片（来自 {}）", peer);
            return false;
        }
        // 屏蔽拦截：忽略被屏蔽联系人的文件推送（返回 false → Ack=false，发送方终止）
        // 走带缓存的 fail-closed 查询，DB 短暂故障时不误放行被屏蔽联系人
        {
            let owner = self
                .mldsa_identity_id
                .clone()
                .unwrap_or_default();
            if self.is_contact_blocked_cached(&owner, &pubkey_hex).await {
                tracing::info!("忽略被屏蔽联系人 {}.. 的文件分片", &pubkey_hex[..16.min(pubkey_hex.len())]);
                return false;
            }
        }
        // 通过鉴权后写入缓存（防膨胀：满时整体清空，简单且可控）
        if self.contact_verified_cache.len() >= super::super::CONTACT_VERIFIED_MAX {
            self.contact_verified_cache.clear();
        }
        self.contact_verified_cache.insert(pubkey_hex.clone(), now);

        // 3. 验证发送者公钥与缓存一致，并解密（含签名验证）
        let Some(decap_key) = &self.mlkem_decap_key else {
            tracing::warn!("ML-KEM 解封装密钥未初始化，拒绝文件分片（来自 {}）", peer);
            return false;
        };
        let expected_pubkey = match hex::decode(&pubkey_hex) {
            Ok(bytes) => bytes,
            Err(e) => {
                tracing::warn!("联系人公钥 hex 解码失败（来自 {}）: {}", peer, e);
                return false;
            }
        };
        let plain_chunk = match chunk.verify_and_open(&expected_pubkey, decap_key) {
            Ok(chunk) => chunk,
            Err(e) => {
                tracing::warn!("文件分片验证/解密失败（来自 {}）: {e}", peer);
                return false;
            }
        };

        // 4. 已取消的传输：拒绝该分片（Ack=false），发送方重试数次后自动中止
        if self
            .file_transfer
            .is_cancelled(&hex::encode(plain_chunk.file_hash))
        {
            tracing::debug!("拒绝已取消传输的分片（来自 {}）", peer);
            return false;
        }

        // 5. 写入（内部校验文件名净化、分片哈希、路径范围、目录可写）
        match self
            .file_transfer
            .handle_file_stream_chunk(plain_chunk, &pubkey_hex)
            .await
        {
            Ok(()) => true,
            Err(e) => {
                tracing::warn!("文件分片处理失败（来自 {}）: {e}", peer);
                self.send_warning_mpsc(format!("文件分片处理失败: {e}"))
                    .await;
                false
            }
        }
    }

    /// 将当前身份发布到 DHT 网络（本地数据库 + 网络发布）
    ///
    /// bootstrap 未就绪时缓存待重试身份，就绪后自动重发；
    /// 缓存只保留一条，新发布覆盖旧值，避免重复与累积。
    pub(crate) fn publish_current_identity_to_dht(&mut self) {
        if let (Some(pubkey), Some(pid)) = (self.mldsa_pubkey_hex.clone(), self.current_peer_id) {
            let store = self.get_dht_store();
            store.set_pubkey_peerid(&pubkey, &pid);
            if !self.bootstrap_ready {
                self.pending_publish_pubkey = Some(pubkey.clone());
            }
            if let Err(e) = self.core_handle.cmd_tx.try_send(ChatCommand::DhtPublishIdentity {
                mldsa_pubkey_hex: pubkey.clone(),
            }) {
                tracing::warn!("Failed to send DhtPublishIdentity: {e:?}");
            }
            tracing::info!("Published current identity to DHT network");
        }
    }

    /// 启动引导：查询自己账号下所有在线设备（多设备 Presence 确认）。
    ///
    /// 通过 `GetProviders(dht_key(自己公钥))` 找到同账号其他设备的 PeerID，
    /// 拨号后由 FriendOnline 签名互认确认哪些设备在线。若本账号有其他设备
    /// 在线（账号整体已在网络），跳过联系人全量 DHT 发现（改由设备同步）；
    /// 否则为账号冷启动，走现有 `discover_all_contacts()` 全量发现。
    ///
    /// 本机自己也是 provider（start_providing 已注册），结果中含本机 PeerID，
    /// 处理时需排除。
    pub(crate) async fn discover_own_devices(&mut self) {
        let Some(own_pubkey) = self.mldsa_pubkey_hex.clone() else {
            self.discover_all_contacts().await;
            return;
        };
        let Some(own_peer) = self.current_peer_id else {
            self.discover_all_contacts().await;
            return;
        };

        let query_key = crate::actor::p2p::swarm_ops::dht_key(&own_pubkey);
        // 预存 key→公钥，供 GetProvidersResult 识别"查询目标是自己"
        self.peer_cache
            .cache_dht_key(query_key.clone(), own_pubkey.clone());
        // 标记"正在查询自己的设备"，结果处理时据此决定是否全量发现
        self.pending_own_device_query = Some((query_key.clone(), own_peer));
        if let Err(e) = self
            .p2p_handle
            .tx
            .try_send(P2pCommand::GetProviders { key: query_key })
        {
            tracing::warn!("Failed to send GetProviders for own devices: {e:?}");
            self.pending_own_device_query = None;
            self.discover_all_contacts().await;
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
                                tracing::warn!("Failed to send DiscoverContact for {}: {e:?}", &contact.mldsa_pubkey_hex[..16.min(contact.mldsa_pubkey_hex.len())]);
                            }
                        }
                        tracing::info!("启动后 DHT 发现命令发送完成，共 {} 位联系人", count);
                    }
                    Err(e) => {
                        tracing::warn!("启动后读取联系人列表失败: {}", e);
                    }
                }
            }
            // 扫码暂存（pending_contacts）同样需要周期性重拨：对方上线/重连后
            // ConnectionEstablished 会重发 FriendOnline，命中摘要自动完成添加。
            // pending 不在 contacts 表，不走 discover_contact 的 DHT 路径，
            // 这里按二维码推荐信息直接重拨（peer_id / 直连地址 / 公共中继地址）。
            if !owner_id.is_empty()
                && let Ok(pendings) = storage::list_pending_contacts(pool, owner_id).await
            {
                for pending in &pendings {
                    // 按摘要经中继反查对端当前 PeerID：peer_id 轮换/直连不可达时
                    // 仍可发现并拨号（中继缓存来自对方 FriendOnline 注册）。
                    let _ = self.p2p_handle.tx.try_send(P2pCommand::DiscoverPeer {
                        mldsa_pubkey_hex: pending.digest_hex.clone(),
                    });
                    if let Some(pid_str) = pending.peer_id.as_deref()
                        && let Ok(pid) = pid_str.parse::<libp2p::PeerId>()
                        && !self.connected_peers.contains_key(&pid)
                        && let Err(e) = self.p2p_handle.tx.try_send(P2pCommand::Dial {
                            peer_id: pid,
                        })
                    {
                        tracing::warn!(
                            "Failed to re-dial pending contact {}..: {e:?}",
                            &pending.digest_hex[..16.min(pending.digest_hex.len())]
                        );
                    }
                    let addrs: Vec<String> =
                        serde_json::from_str(&pending.addrs).unwrap_or_default();
                    for addr_str in addrs {
                        if let Ok(addr) = addr_str.parse::<libp2p::Multiaddr>() {
                            let _ = self.p2p_handle.tx.try_send(P2pCommand::DialAddr { addr });
                        }
                    }
                    let relays: Vec<Vec<String>> =
                        serde_json::from_str(&pending.relay_addrs).unwrap_or_default();
                    for relay in &relays {
                        if relay.len() == 2
                            && let (Ok(pid), Ok(addr)) = (
                                relay[0].parse::<libp2p::PeerId>(),
                                relay[1].parse::<libp2p::Multiaddr>(),
                            )
                        {
                            let _ = self.p2p_handle.tx.try_send(
                                P2pCommand::AddKademliaAddress { peer_id: pid, addr },
                            );
                        }
                    }
                }
            }
        }
    }

    /// 清理过期的 DHT 记录
    pub(crate) fn cleanup_expired_dht_records(&mut self) {
        let store = self.get_dht_store();
        let all_pubkeys = store.get_all_pubkeys();

        let mut stale_keys: Vec<String> = Vec::new();
        let mut stale_peer_ids: Vec<PeerId> = Vec::new();
        for pubkey in &all_pubkeys {
            let Some(peer_id) = store.get_peerid_by_pubkey(pubkey) else {
                continue;
            };
            if !self.connected_peers.contains_key(&peer_id) {
                stale_keys.push(pubkey.clone());
                stale_peer_ids.push(peer_id);
            }
        }

        for key in &stale_keys {
            store.remove_pubkey_peerid(key);
        }
        for pid in &stale_peer_ids {
            self.peer_cache.peerid_to_mlkem.remove(pid);
        }

        // 清理过期的 DHT 查询键映射（保留最近发现的）
        let stale_query_keys: Vec<String> = self.peer_cache.dht_key_to_pubkey
            .iter()
            .filter(|(_, pubkey)| !all_pubkeys.contains(*pubkey))
            .map(|(k, _)| k.clone())
            .collect();
        for key in &stale_query_keys {
            self.peer_cache.dht_key_to_pubkey.remove(key);
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

    // 参数逐一对应 NetEvent FriendOnline 协议字段；聚合结构体需新增无复用价值的中间类型
    #[allow(clippy::too_many_arguments)]
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
        // 限制监听地址数量，签名验证前拒绝超限地址，避免截断后签名验证失败
        if listen_addrs.len() > 32 {
            tracing::warn!("FriendOnline 监听地址过多 ({}), 拒绝", listen_addrs.len());
            self.send_netevent_response(
                channel,
                crate::p2p::netevent::NetEventResponse::Nack {
                    reason: crate::p2p::netevent::NackReason::Other {
                        description: "Too many listen addresses (max 32)".to_string(),
                    },
                },
            );
            return;
        }
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
        if mldsa_pubkey_hex.len() > crate::p2p::netevent::MAX_MLDSA_PUBKEY_HEX_LEN {
            tracing::warn!("FriendOnline 公钥长度异常 ({}), 拒绝", mldsa_pubkey_hex.len());
            self.send_netevent_response(
                channel,
                crate::p2p::netevent::NetEventResponse::Nack {
                    reason: crate::p2p::netevent::NackReason::Other {
                        description: "Invalid mldsa_pubkey_hex length".to_string(),
                    },
                },
            );
            return;
        }
        if let Some(v) = version
            && v != crate::p2p::netevent::NETEVENT_VERSION {
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
        let Some(sig) = &signature else {
            tracing::warn!("FriendOnline 缺少签名: 公钥 {}.. (PeerID={})", &mldsa_pubkey_hex[..16.min(mldsa_pubkey_hex.len())], peer);
            self.send_netevent_response(
                channel,
                crate::p2p::netevent::NetEventResponse::Nack {
                    reason: crate::p2p::netevent::NackReason::SignatureVerificationFailed,
                },
            );
            return;
        };
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
        tracing::debug!("收到有效的 FriendOnline: {}.. (PeerID={})", &mldsa_pubkey_hex[..16.min(mldsa_pubkey_hex.len())], peer);
        let store = self.get_dht_store();
        store.set_pubkey_peerid(&mldsa_pubkey_hex, &peer);
        // 多设备登记：签名验证通过，确认该 PeerID 属于该账号的一台在线设备
        store.add_pubkey_device(&mldsa_pubkey_hex, &peer);
        self.update_peerid_pubkey_mapping(peer, mldsa_pubkey_hex.clone()).await;

        // === 双向密钥交换：对方发来 FriendOnline 时回发自己的 FriendOnline ===
        // 仅当对方携带的 ML-KEM 公钥与本地缓存不同（对方会话密钥刚轮换，可能
        // 还持有我方的旧公钥）才回发，避免连接建立时双方同时在线互发形成环。
        // 回发让对端刷新我方最新 ML-KEM，配合消息附带指纹的过期检测，双向自愈。
        let mlkem_rotated = !mlkem_pubkey_hex.is_empty()
            && self
                .peer_cache
                .peerid_to_mlkem
                .get(&peer)
                .map(|old| *old != mlkem_pubkey_hex)
                .unwrap_or(true);
        if !mlkem_pubkey_hex.is_empty() {
            self.peer_cache.cache_mlkem(peer, mlkem_pubkey_hex.clone());
        }
        if mlkem_rotated {
            tracing::info!(
                "对方 {}.. 的 ML-KEM 公钥已轮换，回发 FriendOnline 完成双向交换",
                &mldsa_pubkey_hex[..16.min(mldsa_pubkey_hex.len())]
            );
            self.send_friend_online(&peer).await;
        }

        // 定向通知该联系人在线（含设备集合），不依赖整体 online-status 刷新
        self.emit_contact_status(mldsa_pubkey_hex.clone()).await;

        // === 扫码暂存匹配：digest 与 DHT 查询键一致（dht_key）===
        // 收到 FriendOnline 携带的完整公钥后计算摘要，命中 pending_contacts
        // 则自动补添加联系人（完整密钥在设备间自行交换完成）。
        if let Some(pool) = storage::pool() {
            let owner_id = self.mldsa_identity_id.as_deref().unwrap_or("");
            if !owner_id.is_empty() {
                let digest = crate::actor::p2p::swarm_ops::dht_key(&mldsa_pubkey_hex);
                if let Ok(Some(pending)) =
                    storage::get_pending_contact_by_digest(pool, owner_id, &digest).await
                {
                    let name = pending.name.clone();
                    let mlkem_bytes = hex::decode(&mlkem_pubkey_hex).unwrap_or_default();
                    let mlkem_opt = if mlkem_bytes.is_empty() {
                        None
                    } else {
                        Some(mlkem_bytes.as_slice())
                    };
                    match storage::upsert_contact(
                        pool,
                        owner_id,
                        &mldsa_pubkey_hex,
                        name.as_deref(),
                        mlkem_opt,
                    )
                    .await
                    {
                        Ok(_) => {
                            if let Err(e) =
                                storage::delete_pending_contact(pool, owner_id, &digest).await
                            {
                                tracing::warn!("删除扫码暂存失败: {e}");
                            }
                            let short =
                                &mldsa_pubkey_hex[..16.min(mldsa_pubkey_hex.len())];
                            tracing::info!("扫码暂存匹配成功，已添加联系人 {}..", short);
                            self.send_log_mpsc(format!("已通过扫码添加好友 {}..", short))
                                .await;
                            // 通知上层刷新联系人列表（扫码暂存自动补添路径）
                            let _ = self
                                .tx_message
                                .try_send(crate::command::MessageEvent::ContactsChanged);
                            // 匹配成功后无条件回发 FriendOnline：对方若也持有本端
                            // pending，即可凭我方完整公钥完成双向添加（弥补一次性
                            // 回发可能丢失的缺口，配合周期重拨双保险）。
                            self.send_friend_online(&peer).await;
                        }
                        Err(e) => {
                            tracing::warn!("扫码匹配后保存联系人失败: {e}");
                            self.send_warning_mpsc(format!("扫码匹配后保存联系人失败: {e}"))
                                .await;
                        }
                    }
                }
            }
        }

        let owner_id = self.mldsa_identity_id.as_deref().unwrap_or("");
        if !owner_id.is_empty()
            && let Some(pool) = storage::pool()
                && storage::is_contact_exists(pool, owner_id, &mldsa_pubkey_hex).await.unwrap_or(false)
                    && let Ok(msgs) = storage::list_pending_by_peer(pool, &mldsa_pubkey_hex).await
                        && !msgs.is_empty() {
                            self.retry_pending_messages(Some(&mldsa_pubkey_hex)).await;
                        }
        for addr_str in &listen_addrs {
            if let Ok(addr) = addr_str.parse::<libp2p::Multiaddr>() {
                let _ = self.p2p_handle.tx.try_send(P2pCommand::AddKademliaAddress { peer_id: peer, addr: addr.clone() });
                let _ = self.p2p_handle.tx.try_send(P2pCommand::DialAddr { addr });
            }
        }
        self.send_netevent_response(channel, crate::p2p::netevent::NetEventResponse::Ack);
    }

    /// 计算某账户当前已连接（在线）的设备 PeerID 列表。
    ///
    /// 设备集 = `peerid_to_pubkey` 中归属该账户、且仍在 `connected_peers` 的 PeerID。
    /// 这是对端离线判定与"剩余设备告知"的唯一权威来源。
    fn remaining_devices_of(&self, mldsa_pubkey_hex: &str) -> Vec<String> {
        self.peer_cache
            .peerid_to_pubkey
            .iter()
            .filter(|(_, pk)| *pk == mldsa_pubkey_hex)
            .filter(|(pid, _)| self.connected_peers.contains_key(pid))
            .map(|(pid, _)| pid.to_string())
            .collect()
    }

    /// 向上层发送单个联系人的在线状态（含设备集合）
    async fn emit_contact_status(&mut self, mldsa_pubkey_hex: String) {
        let devices = self.remaining_devices_of(&mldsa_pubkey_hex);
        let online = !devices.is_empty();
        let _ = self
            .tx_message
            .try_send(crate::command::MessageEvent::ContactOnlineStatus {
                mldsa_pubkey_hex,
                online,
                remaining_devices: devices,
            });
    }

    /// 向指定 peer 发送 FriendOnline（身份交换）。
    ///
    /// 供 ConnectionEstablished、消息指纹过期检测与 FriendOnline 双向回发复用。
    pub(crate) async fn send_friend_online(&mut self, peer_id: &libp2p::PeerId) {
        let Some(mldsa_pubkey_hex) = self.mldsa_pubkey_hex.clone() else {
            return;
        };
        let Some(current_peer_id) = self.current_peer_id else {
            return;
        };
        let Some(mldsa_private_key) = self.mldsa_private_key.as_ref() else {
            return;
        };
        let mlkem = self.mlkem_pubkey_hex.clone().unwrap_or_default();
        let Some(friend_online) =
            crate::actor::p2p::netevent::build_friend_online_request(
                &mldsa_pubkey_hex,
                &current_peer_id,
                &self.own_listen_addrs,
                &mlkem,
                mldsa_private_key,
            )
        else {
            tracing::warn!("FriendOnline 签名失败，跳过发送");
            return;
        };
        if let Err(e) = self.p2p_handle.tx.try_send(P2pCommand::SendNetEvent {
            peer_id: *peer_id,
            request: friend_online,
        }) {
            tracing::warn!("Failed to send FriendOnline NetEvent: {e:?}");
            return;
        }
        tracing::info!(
            "向 {}.. 发送了 FriendOnline 通知",
            &mldsa_pubkey_hex[..16.min(mldsa_pubkey_hex.len())]
        );
    }

    /// 检查消息来源哈希是否已见过（按 `origin_hash` 去重，多设备转发场景）。
    ///
    /// 转发消息密文/哈希与原始不同，但 `origin_hash` 由真源设备生成并逐跳携带，
    /// 是跨直发/转发副本的稳定去重键。首次见返回 false 并记录；重复见返回 true。
    /// seen 表有上限，满时清空（偶发重收由接收端内容哈希兜底，可容忍）。
    pub(crate) fn device_seen_origin_hash_check(&mut self, origin_hash: &str) -> bool {
        const MAX_SEEN: usize = 4096;
        if self.device_seen_seqs.contains(origin_hash) {
            return true;
        }
        self.device_seen_seqs.insert(origin_hash.to_string());
        if self.device_seen_seqs.len() > MAX_SEEN {
            self.device_seen_seqs.clear();
        }
        false
    }

    /// 生成一条新的设备同步 routing（本机为真源/转发源时调用）。
    ///
    /// seq 取自本机单调计数器（(source_device, seq) 账号内唯一）；
    /// origin_hash 基于「源账号公钥‖msgtype‖seq‖明文」派生——同一逻辑消息
    /// 的首发生成一次，转发逐跳原样携带，跨直发/转发副本去重稳定；
    /// 同内容重复发送因 seq 不同而派生不同 origin_hash，不会误判为重复。
    pub(crate) fn new_device_routing(
        &mut self,
        target_identity: &str,
        msgtype: crate::ChatMessageType,
        plaintext: &[u8],
    ) -> crate::DeviceRouting {
        self.device_sync_seq = self.device_sync_seq.wrapping_add(1);
        let seq = self.device_sync_seq;
        let source_device = self
            .current_peer_id
            .map(|p| p.to_string())
            .unwrap_or_default();
        let mut h = sha2::Sha256::new();
        if let Some(own) = self.mldsa_pubkey_hex.as_deref() {
            h.update(own.as_bytes());
        }
        h.update([msgtype as u8]);
        h.update(seq.to_be_bytes());
        h.update(plaintext);
        crate::DeviceRouting {
            seq,
            source_device,
            target_identity: target_identity.to_string(),
            hop: 0,
            origin_hash: hex::encode(h.finalize()),
        }
    }

    /// 处理 FriendOffline（设备下线通知）
    ///
    /// 验证签名后，将该设备从 connected_peers / peerid_to_pubkey 移除（权威离线信号），
    /// 再以本地已连接的该账户设备集合为准刷新其在线状态：
    /// - 集合为空 → 该账户离线；
    /// - 还有设备 → 在线，并把剩余设备告知上层。
    #[allow(clippy::too_many_arguments)]
    async fn handle_friend_offline_request(
        &mut self,
        mldsa_pubkey_hex: String,
        claimed_peer_id: String,
        remaining_peers: Vec<String>,
        signature: Option<Vec<u8>>,
        peer: libp2p::PeerId,
        channel: libp2p::request_response::ResponseChannel<crate::p2p::netevent::NetEventResponse>,
    ) {
        let short = &mldsa_pubkey_hex[..16.min(mldsa_pubkey_hex.len())];
        if mldsa_pubkey_hex.len() > crate::p2p::netevent::MAX_MLDSA_PUBKEY_HEX_LEN {
            // 防御上限：恶意对端发超大公钥 hex 会造成 hex::decode 巨额分配
            tracing::warn!("FriendOffline 公钥长度异常 ({}), 拒绝", mldsa_pubkey_hex.len());
            self.send_netevent_response(
                channel,
                crate::p2p::netevent::NetEventResponse::Nack {
                    reason: crate::p2p::netevent::NackReason::Other {
                        description: "Invalid mldsa_pubkey_hex length".to_string(),
                    },
                },
            );
            return;
        }
        if claimed_peer_id != peer.to_string() {
            tracing::warn!("FriendOffline PeerID 不匹配: 声称={}, 实际={}", claimed_peer_id, peer);
            self.send_netevent_response(
                channel,
                crate::p2p::netevent::NetEventResponse::Nack {
                    reason: crate::p2p::netevent::NackReason::PeerIdMismatch,
                },
            );
            return;
        }
        // 防御上限：签名验证前拒绝超限的剩余设备列表，避免哈希/内存被异常放大
        if remaining_peers.len() > 64 {
            tracing::warn!("FriendOffline 剩余设备过多 ({}), 拒绝", remaining_peers.len());
            self.send_netevent_response(
                channel,
                crate::p2p::netevent::NetEventResponse::Nack {
                    reason: crate::p2p::netevent::NackReason::Other {
                        description: "Too many remaining peers (max 64)".to_string(),
                    },
                },
            );
            return;
        }
        let Some(sig) = &signature else {
            tracing::warn!("FriendOffline 缺少签名: 公钥 {}.. (PeerID={})", short, peer);
            self.send_netevent_response(
                channel,
                crate::p2p::netevent::NetEventResponse::Nack {
                    reason: crate::p2p::netevent::NackReason::SignatureVerificationFailed,
                },
            );
            return;
        };
        if !crate::p2p::netevent::verify_friend_offline_signature(
            &mldsa_pubkey_hex,
            &claimed_peer_id,
            &remaining_peers,
            sig,
        ) {
            tracing::warn!("FriendOffline 签名验证失败: 声称公钥 {}.. (PeerID={})", short, peer);
            self.send_netevent_response(
                channel,
                crate::p2p::netevent::NetEventResponse::Nack {
                    reason: crate::p2p::netevent::NackReason::SignatureVerificationFailed,
                },
            );
            return;
        }

        tracing::info!(
            "收到 FriendOffline: 账户 {}.. 设备 {} 下线（发送方剩余 {:?}）",
            short,
            peer,
            remaining_peers
        );

        // 权威离线信号：直接移除该设备（不依赖 TCP/中继连接关闭）
        self.connected_peers.remove(&peer);
        self.peer_cache.peerid_to_pubkey.remove(&peer);
        // 多设备集合维护：设备离线 → 从该账号在线设备列表移除
        self.get_dht_store().remove_pubkey_device(&mldsa_pubkey_hex, &peer);

        // 账户是否仍在线以本地设备集为准（remaining_peers 仅作参考）
        self.emit_contact_status(mldsa_pubkey_hex.clone()).await;
        self.send_online_status().await;
        self.send_netevent_response(channel, crate::p2p::netevent::NetEventResponse::Ack);
    }
}
