use crate::{command::ChatCommand, core::ChatCore, error::CoreError};

impl ChatCore {
    /// 处理单个控制命令
    pub(crate) async fn handle_command(&mut self, cmd: ChatCommand) {
        match cmd {
            ChatCommand::SendMessage {
                mldsa_pubkey_hex,
                msgtype,
                data,
            } => match self.send_text(&mldsa_pubkey_hex, msgtype, data).await {
                Ok(message_hash) => {
                    tracing::info!(
                        "{:?} message sent to {}, hash={}..",
                        msgtype,
                        &mldsa_pubkey_hex[..16],
                        &message_hash[..16]
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
                                &mldsa_pubkey_hex[..16],
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
                self.retry_pending_messages().await;
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
                file_id,
            } => {
                self.handle_file_download_request(&sender_mldsa_pubkey_hex, file_id, None)
                    .await;
            }
            ChatCommand::SetDownloadDir { path } => {
                self.handle_set_download_dir(path);
            }
            ChatCommand::RegisterFileForDownload { file_id, file_path } => {
                let file_id_hex = hex::encode(file_id);
                tracing::info!(
                    "Registering file for download: file_id={}.., path={:?}",
                    &file_id_hex[..16],
                    file_path
                );
                self.file_path_map.insert(file_id, file_path);
            }
            ChatCommand::DhtPublishIdentity {
                mldsa_pubkey_hex,
                peer_id,
                mlkem_pubkey_hex,
            } => {
                self.publish_identity_to_dht(&mldsa_pubkey_hex, &peer_id, &mlkem_pubkey_hex)
                    .await;
            }
            ChatCommand::DiscoverContact {
                mldsa_pubkey_hex,
                name,
            } => {
                self.discover_contact(&mldsa_pubkey_hex, name).await;
            }
            ChatCommand::Shutdown => {
                tracing::warn!(
                    "Shutdown command reached handle_command (should be handled in run_inner)"
                );
            }
        }
    }

    /// 安全设置下载目录：确保路径在 data_dir 内，防止任意路径写入
    fn handle_set_download_dir(&mut self, path: std::path::PathBuf) {
        let canonical_data = match self.data_dir.canonicalize() {
            Ok(d) => d,
            Err(e) => {
                tracing::error!("无法规范化 data_dir {:?}: {}", self.data_dir, e);
                return;
            }
        };
        let canonical_path = match path.canonicalize() {
            Ok(p) => p,
            Err(_) => {
                if let Err(e) = std::fs::create_dir_all(&path) {
                    tracing::error!("无法创建下载目录 {:?}: {}", path, e);
                    return;
                }
                match path.canonicalize() {
                    Ok(p) => p,
                    Err(e) => {
                        tracing::error!("无法规范化下载路径 {:?}: {}", path, e);
                        return;
                    }
                }
            }
        };
        if !canonical_path.starts_with(&canonical_data) {
            tracing::error!(
                "拒绝设置下载目录: {:?} 不在 data_dir {:?} 内",
                canonical_path,
                canonical_data
            );
            return;
        }
        self.download_dir = canonical_path;
        tracing::info!("Download directory set to {:?}", self.download_dir);
    }
}
