use crate::{actor::p2p::P2pCommand, command::ChatCommand, core::ChatCore, error::CoreError, message::ChatMessageType, storage};

impl ChatCore {
    /// 处理单个控制命令
    pub(crate) async fn handle_command(&mut self, cmd: ChatCommand) {
        match cmd {
            ChatCommand::SendMessage {
                mldsa_pubkey_hex,
                msgtype,
                data,
            } => match self.send_text(&mldsa_pubkey_hex, msgtype, data, true).await {
                Ok(message_hash) => {
                    tracing::info!(
                        "{:?} message sent to {}, hash={}..",
                        msgtype,
                        &mldsa_pubkey_hex[..16.min(mldsa_pubkey_hex.len())],
                        &message_hash[..16.min(message_hash.len())]
                    );
                    // 通知前端消息已发送，附带消息哈希用于匹配送达回执
                    self.send_message_mpsc(crate::command::IncomingMessage::MessageSent {
                        message_hash,
                        peer_id: mldsa_pubkey_hex.clone(),
                    })
                    .await;
                }
                Err(e) => {
                    // 如果错误是"联系人离线"或"ML-KEM 未缓存"，消息已由 send_text_impl
                    // 自动保存到离线队列并发送了 Log 事件，这里不再重复发送 Warning。
                    // 只对真正的错误（如数据库不可用、加密失败）发送 Warning。
                    match &e {
                        CoreError::ContactOffline(_) | CoreError::MlKemKeyNotCached(_) => {
                            tracing::info!(
                                "{:?} message queued for {}: {}",
                                msgtype,
                                &mldsa_pubkey_hex[..16.min(mldsa_pubkey_hex.len())],
                                e
                            );
                        }
                        _ => {
                            tracing::error!("Failed to send {:?} message: {e}", msgtype);
                            let err_msg = format!("发送消息失败: {}", e);
                            self.send_warning_mpsc(err_msg).await;
                        }
                    }
                }
            },
            ChatCommand::RetryPendingMessages => {
                self.retry_pending_messages(None).await;
            }
            ChatCommand::ResumeFileTransfers => {
                self.resume_pending_downloads().await;
            }
            ChatCommand::CancelFileTransfer { file_id_hex, resp } => {
                let cancelled = self.file_transfer.cancel_transfer(&file_id_hex);
                if cancelled {
                    self.send_log_mpsc(format!(
                        "已取消文件传输（{}..）",
                        &file_id_hex[..16.min(file_id_hex.len())]
                    ))
                    .await;
                }
                let _ = resp.send(cancelled);
            }
            ChatCommand::UpdateContactMeta {
                mldsa_pubkey_hex,
                name,
                notes,
                group_name,
                blocked,
                resp,
            } => {
                let owner_id = self.mldsa_identity_id.as_deref().unwrap_or("");
                let ok = if owner_id.is_empty() {
                    false
                } else {
                    match storage::pool() {
                        Some(pool) => storage::update_contact_meta(
                            pool,
                            owner_id,
                            &mldsa_pubkey_hex,
                            name.as_deref(),
                            notes.as_deref(),
                            group_name.as_deref(),
                            blocked,
                        )
                        .await
                        .unwrap_or(false),
                        None => false,
                    }
                };
                if ok {
                    // 屏蔽状态已变更：失效缓存，避免陈旧结果继续放行/拦截
                    self.invalidate_blocked_cache(&mldsa_pubkey_hex);
                    if blocked == Some(true) {
                        self.send_log_mpsc(format!("已屏蔽联系人 {}..", &mldsa_pubkey_hex[..16.min(mldsa_pubkey_hex.len())])).await;
                    } else if blocked == Some(false) {
                        self.send_log_mpsc(format!("已取消屏蔽 {}..", &mldsa_pubkey_hex[..16.min(mldsa_pubkey_hex.len())])).await;
                    }
                }
                let _ = resp.send(ok);
            }
            ChatCommand::SearchMessages { query, resp } => {
                let json = self.search_messages_json(&query).await;
                let _ = resp.send(json);
            }
            ChatCommand::GetStorageStats { resp } => {
                let json = self.storage_stats_json().await;
                let _ = resp.send(json);
            }
            ChatCommand::DeleteContactHistory {
                mldsa_pubkey_hex,
                resp,
            } => {
                let owner = self.mldsa_identity_id.as_deref().unwrap_or("");
                let ok = if owner.is_empty() {
                    false
                } else {
                    match storage::pool() {
                        Some(pool) => match storage::delete_messages_by_peer(
                            pool,
                            owner,
                            &mldsa_pubkey_hex,
                        )
                        .await
                        {
                            Ok(n) => {
                                self.send_log_mpsc(format!(
                                    "已删除联系人 {} 的 {n} 条聊天记录",
                                    &mldsa_pubkey_hex[..16.min(mldsa_pubkey_hex.len())]
                                ))
                                .await;
                                true
                            }
                            Err(e) => {
                                tracing::warn!("删除聊天记录失败: {e}");
                                false
                            }
                        },
                        None => false,
                    }
                };
                let _ = resp.send(ok);
            }
            ChatCommand::DeleteContactFiles {
                mldsa_pubkey_hex,
                resp,
            } => {
                let freed = match storage::pool() {
                    Some(pool) => {
                        match storage::delete_sent_files_by_recipient(pool, &mldsa_pubkey_hex)
                            .await
                        {
                            Ok(paths) => {
                                let mut freed = 0i64;
                                for p in &paths {
                                    if let Ok(md) = std::fs::metadata(p) {
                                        freed += md.len() as i64;
                                    }
                                    if let Err(e) = std::fs::remove_file(p) {
                                        tracing::warn!("删除本地文件失败 {:?}: {e}", p);
                                    }
                                }
                                if !paths.is_empty() {
                                    self.send_log_mpsc(format!(
                                        "已删除联系人 {} 的 {} 个本地文件，释放 {freed} 字节",
                                        &mldsa_pubkey_hex[..16.min(mldsa_pubkey_hex.len())],
                                        paths.len()
                                    ))
                                    .await;
                                }
                                Some(freed)
                            }
                            Err(e) => {
                                tracing::warn!("删除联系人文件记录失败: {e}");
                                None
                            }
                        }
                    }
                    None => None,
                };
                let _ = resp.send(freed);
            }
            ChatCommand::AddContact {
                mldsa_pubkey_hex,
                mlkem_public_key,
                name,
                resp,
            } => {
                let result = self
                    .add_contact(mldsa_pubkey_hex, mlkem_public_key, name)
                    .await;
                let _ = resp.send(result);
            }
            ChatCommand::AddContactByScan { info, resp } => {
                let result = self.add_contact_by_digest(&info).await;
                let _ = resp.send(result);
            }
            ChatCommand::AddContactByScanBinary { bytes, resp } => {
                let result = match Self::decode_identity_qr_v2(&bytes) {
                    Some(info) => {
                        let digest = info.digest_hex.clone();
                        if self.add_contact_by_digest(&info).await {
                            Ok(digest)
                        } else {
                            Err(())
                        }
                    }
                    None => Err(()),
                };
                let _ = resp.send(result);
            }
            ChatCommand::GenerateIdentity => self.generate_identity().await,
            ChatCommand::SelectIdentity { identity_id } => self.select_identity(identity_id).await,
            ChatCommand::DeleteIdentity { identity_id } => self.delete_identity(identity_id).await,
            ChatCommand::RequestFileDownload {
                sender_mldsa_pubkey_hex,
                file_hash,
                save_path,
            } => {
                let data = match self
                    .file_transfer
                    .try_start_download(file_hash, &save_path, &sender_mldsa_pubkey_hex)
                {
                    Ok((_, data)) => data,
                    Err(msg) => {
                        self.send_warning_mpsc(msg).await;
                        return;
                    }
                };
                let hash_hex = hex::encode(file_hash);
                if let Err(e) = self
                    .send_text(&sender_mldsa_pubkey_hex, ChatMessageType::FileDownloadRequest, data, false)
                    .await
                {
                    self.file_transfer.cancel_download(&hash_hex);
                    self.send_warning_mpsc(format!("文件下载请求发送失败: {e}")).await;
                }
            }
            ChatCommand::DhtPublishIdentity { mldsa_pubkey_hex } => {
                self.publish_identity_to_dht(&mldsa_pubkey_hex).await;
            }
            ChatCommand::DiscoverContact {
                mldsa_pubkey_hex,
                name,
            } => {
                self.discover_contact(&mldsa_pubkey_hex, name).await;
            }
            ChatCommand::SetPaidNetworkMode(mode) => {
                if let Err(e) = self
                    .p2p_handle
                    .tx
                    .try_send(
                        P2pCommand::SetPaidNetworkMode { mode },
                    )
                {
                    tracing::warn!("Failed to send SetPaidNetworkMode: {e:?}");
                }
            }
            ChatCommand::SetRelayRole(role) => {
                if let Err(e) = self
                    .p2p_handle
                    .tx
                    .try_send(P2pCommand::SetRelayRole { role })
                {
                    tracing::warn!("Failed to send SetRelayRole: {e:?}");
                }
            }
            // ===== 定时器事件处理 =====
            ChatCommand::TimerSaveRoutingTable => {
                if let Err(e) = self
                    .p2p_handle
                    .tx
                    .try_send(
                        P2pCommand::SaveRoutingTable,
                    )
                {
                    tracing::warn!("Failed to send periodic SaveRoutingTable: {e:?}");
                }
            }
            ChatCommand::TimerDiscoverAllContacts => {
                // 兜底：启动自检（自己设备查询）长时间无结果（DHT 无响应）时，
                // 清除挂起标记并执行联系人全量发现，避免一直跳过发现。
                if self.pending_own_device_query.take().is_some() {
                    tracing::warn!(
                        "启动自检未在期限内返回结果，回退到联系人全量发现"
                    );
                }
                self.discover_all_contacts().await;
            }
            ChatCommand::TimerCleanupDht => {
                self.cleanup_expired_dht_records();
            }
            ChatCommand::TimerPublishIdentity => {
                self.publish_current_identity_to_dht();
            }
            ChatCommand::TimerRefreshRoutingTable => {
                if let Err(e) = self
                    .p2p_handle
                    .tx
                    .try_send(
                        P2pCommand::RefreshRoutingTable,
                    )
                {
                    tracing::warn!("Failed to send RefreshRoutingTable: {e:?}");
                }
            }
            ChatCommand::TimerScanFileTransfers => {
                self.file_transfer.scan_timeout_transfers();
            }
            ChatCommand::TimerRefreshPresence => {
                // 周期对账：向 UI 重发当前权威在线状态（补偿偶发丢失的事件）。
                // ML-KEM 会话密钥过期已由消息附带指纹 + FriendOnline 双向回发
                // 按需自愈，无需全量周期重发。
                self.send_online_status().await;
            }
            ChatCommand::OnForeground => {
                // 后台→前台：重建网络连接层（重拨中继 + 重跑 bootstrap + 补发暂存查询）。
                if let Err(e) = self
                    .p2p_handle
                    .tx
                    .try_send(P2pCommand::OnForeground)
                {
                    tracing::warn!("Failed to send OnForeground to P2pActor: {e:?}");
                }
            }
            ChatCommand::GetNetworkStatus { resp } => {
                let (p2p_tx, p2p_rx) = tokio::sync::oneshot::channel();
                if let Err(e) = self.p2p_handle.tx.try_send(P2pCommand::GetNetworkStatus { resp: p2p_tx }) {
                    tracing::warn!("Failed to send GetNetworkStatus to P2pActor: {e:?}");
                    let _ = resp.send(
                        crate::command::NetworkStatusData::error_json("p2p_channel_closed", &format!("P2pActor command channel full/closed: {}", e))
                    );
                    return;
                }
                // 限时等待 P2pActor：ChatCore 事件循环若在此无限阻塞，会与 P2pActor 的
                // 关键事件阻塞发送（背压）互相等待形成死锁，整核冻结
                match tokio::time::timeout(std::time::Duration::from_secs(5), p2p_rx).await {
                    Ok(Ok(json)) => { let _ = resp.send(json); }
                    Ok(Err(_)) => {
                        let _ = resp.send(
                            crate::command::NetworkStatusData::error_json("p2p_no_response", "P2pActor did not respond to status query")
                        );
                    }
                    Err(_) => {
                        let _ = resp.send(
                            crate::command::NetworkStatusData::error_json("p2p_timeout", "P2pActor status query timed out")
                        );
                    }
                }
            }
            ChatCommand::GetIdentityShareInfo { resp } => {
                let bytes = self.build_identity_share_info().await;
                let _ = resp.send(bytes);
            }
            ChatCommand::ExportRoutingTable { resp } => {
                let (p2p_tx, p2p_rx) = tokio::sync::oneshot::channel();
                if let Err(e) = self.p2p_handle.tx.try_send(P2pCommand::ExportRoutingTable { resp: p2p_tx }) {
                    tracing::warn!("Failed to send ExportRoutingTable to P2pActor: {e:?}");
                    let _ = resp.send(serde_json::json!({ "version": 0, "peers": [], "error": e.to_string() }).to_string());
                    return;
                }
                match tokio::time::timeout(std::time::Duration::from_secs(5), p2p_rx).await {
                    Ok(Ok(json)) => { let _ = resp.send(json); }
                    _ => { let _ = resp.send(serde_json::json!({ "version": 0, "peers": [], "error": "P2pActor did not respond" }).to_string()); }
                }
            }
            ChatCommand::DialPeer { peer_id, addr } => {
                let peer_id = match peer_id.parse::<libp2p::PeerId>() {
                    Ok(pid) => pid,
                    Err(e) => {
                        tracing::warn!("Invalid PeerId for dial: {peer_id}: {e}");
                        return;
                    }
                };
                let multiaddr = match addr.parse::<libp2p::Multiaddr>() {
                    Ok(a) => a,
                    Err(e) => {
                        tracing::warn!("Invalid Multiaddr for dial: {addr}: {e}");
                        return;
                    }
                };
                // 先添加到 Kademlia 路由表，再拨号
                if let Err(e) = self.p2p_handle.tx.try_send(P2pCommand::AddKademliaAddress {
                    peer_id,
                    addr: multiaddr.clone(),
                }) {
                    tracing::warn!("Failed to send AddKademliaAddress: {e:?}");
                }
                let dial_addr = multiaddr.with(libp2p::multiaddr::Protocol::P2p(peer_id));
                if let Err(e) = self.p2p_handle.tx.try_send(P2pCommand::DialAddr { addr: dial_addr }) {
                    tracing::warn!("Failed to send DialAddr: {e:?}");
                }
            }
            ChatCommand::ImportRoutingTable { data, resp } => {
                let (p2p_tx, p2p_rx) = tokio::sync::oneshot::channel();
                if let Err(e) = self.p2p_handle.tx.try_send(P2pCommand::ImportRoutingTable { data, resp: p2p_tx }) {
                    tracing::warn!("Failed to send ImportRoutingTable to P2pActor: {e:?}");
                    let _ = resp.send(serde_json::json!({ "imported": 0, "error": e.to_string() }).to_string());
                    return;
                }
                match tokio::time::timeout(std::time::Duration::from_secs(5), p2p_rx).await {
                    Ok(Ok(json)) => { let _ = resp.send(json); }
                    _ => { let _ = resp.send(serde_json::json!({ "imported": 0, "error": "P2pActor did not respond" }).to_string()); }
                }
            }
            // Shutdown 由 run_inner 直接拦截处理，不进入 handle_command
            ChatCommand::Shutdown => {}
        }
    }

    /// 按关键词搜索所有联系人的消息（JSON 数组，供前端"按消息搜索"）
    async fn search_messages_json(&mut self, query: &str) -> String {
        let q = query.trim();
        if q.is_empty() {
            return serde_json::json!([]).to_string();
        }
        let owner = self.mldsa_identity_id.as_deref().unwrap_or("");
        let Some(pool) = storage::pool() else {
            return serde_json::json!([]).to_string();
        };
        match storage::search_messages_by_content(pool, owner, q, 200).await {
            Ok(hits) => serde_json::json!(
                hits
                    .iter()
                    .map(|h| {
                        serde_json::json!({
                            "peer_pubkey_hex": h.peer_pubkey_hex,
                            "name": h.name,
                            "content": h.content,
                            "is_outgoing": h.is_outgoing,
                            "ts": h.ts,
                        })
                    })
                    .collect::<Vec<_>>()
            )
            .to_string(),
            Err(e) => {
                tracing::warn!("消息搜索失败: {e}");
                serde_json::json!([]).to_string()
            }
        }
    }

    /// 存储管理统计：每位联系人的消息/文件占用（JSON 数组，按占用降序）
    async fn storage_stats_json(&mut self) -> String {
        use std::collections::HashMap;
        let owner = self.mldsa_identity_id.as_deref().unwrap_or("");
        let Some(pool) = storage::pool() else {
            return serde_json::json!([]).to_string();
        };
        let contacts = storage::list_contacts(pool, owner).await.unwrap_or_default();
        let stats = storage::contact_messages_stat(pool, owner).await.unwrap_or_default();
        let mut stat_map: HashMap<String, (i64, i64, i64)> = HashMap::new();
        for s in &stats {
            stat_map.insert(
                s.peer_pubkey_hex.clone(),
                (s.msg_count, s.msg_bytes, s.file_count),
            );
        }
        let mut rows: Vec<serde_json::Value> = Vec::with_capacity(contacts.len());
        for c in &contacts {
            let (msg_count, msg_bytes, file_count) =
                stat_map.get(&c.mldsa_pubkey_hex).copied().unwrap_or((0, 0, 0));
            let file_bytes = storage::sum_sent_bytes_by_recipient(pool, &c.mldsa_pubkey_hex)
                .await
                .unwrap_or(0);
            let name = c
                .name
                .clone()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| storage::contact_display_name(&c.mldsa_pubkey_hex));
            let total = msg_bytes + file_bytes;
            rows.push(serde_json::json!({
                "peer_pubkey_hex": c.mldsa_pubkey_hex,
                "name": name,
                "msg_count": msg_count,
                "msg_bytes": msg_bytes,
                "file_count": file_count,
                "file_bytes": file_bytes,
                "total_bytes": total,
            }));
        }
        rows.sort_by(|a, b| {
            b["total_bytes"]
                .as_i64()
                .unwrap_or(0)
                .cmp(&a["total_bytes"].as_i64().unwrap_or(0))
        });
        serde_json::json!(rows).to_string()
    }
}
