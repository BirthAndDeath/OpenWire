use crate::{actor::p2p::P2pCommand, command::ChatCommand, core::ChatCore, error::CoreError, message::ChatMessageType};

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
            ChatCommand::GenerateIdentity => self.generate_identity().await,
            ChatCommand::SelectIdentity { identity_id } => self.select_identity(identity_id).await,
            ChatCommand::DeleteIdentity { identity_id } => self.delete_identity(identity_id).await,
            ChatCommand::RequestFileDownload {
                sender_mldsa_pubkey_hex,
                file_hash,
                save_path,
            } => {
                let data = match self.file_transfer.try_start_download(file_hash, &save_path) {
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
            ChatCommand::GetNetworkStatus { resp } => {
                let (p2p_tx, p2p_rx) = tokio::sync::oneshot::channel();
                if let Err(e) = self.p2p_handle.tx.try_send(P2pCommand::GetNetworkStatus { resp: p2p_tx }) {
                    tracing::warn!("Failed to send GetNetworkStatus to P2pActor: {e:?}");
                    let _ = resp.send(
                        crate::command::NetworkStatusData::error_json("p2p_channel_closed", &format!("P2pActor command channel full/closed: {}", e))
                    );
                    return;
                }
                match p2p_rx.await {
                    Ok(json) => { let _ = resp.send(json); }
                    Err(_) => {
                        let _ = resp.send(
                            crate::command::NetworkStatusData::error_json("p2p_no_response", "P2pActor did not respond to status query")
                        );
                    }
                }
            }
            ChatCommand::ExportRoutingTable { resp } => {
                let (p2p_tx, p2p_rx) = tokio::sync::oneshot::channel();
                if let Err(e) = self.p2p_handle.tx.try_send(P2pCommand::ExportRoutingTable { resp: p2p_tx }) {
                    tracing::warn!("Failed to send ExportRoutingTable to P2pActor: {e:?}");
                    let _ = resp.send(serde_json::json!({ "version": 0, "peers": [], "error": e.to_string() }).to_string());
                    return;
                }
                match p2p_rx.await {
                    Ok(json) => { let _ = resp.send(json); }
                    Err(_) => { let _ = resp.send(serde_json::json!({ "version": 0, "peers": [], "error": "P2pActor did not respond" }).to_string()); }
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
                match p2p_rx.await {
                    Ok(json) => { let _ = resp.send(json); }
                    Err(_) => { let _ = resp.send(serde_json::json!({ "imported": 0, "error": "P2pActor did not respond" }).to_string()); }
                }
            }
            // Shutdown 由 run_inner 直接拦截处理，不进入 handle_command
            ChatCommand::Shutdown => {}
        }
    }
}
