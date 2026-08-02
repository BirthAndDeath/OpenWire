use std::path::PathBuf;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::{ChatCommand, ChatMessageType};

/// 核心句柄，封装向核心发送命令的通道与优雅关闭信号
#[derive(Debug, Clone)]
pub struct CoreHandle {
    /// 命令发送通道，用于向核心发送 ChatCommand
    pub cmd_tx: mpsc::Sender<crate::ChatCommand>,
    /// 关闭信号 token，用于在 Drop 时优雅停止后台任务（如 DHT 注册循环）
    pub shutdown_token: CancellationToken,
}

impl CoreHandle {
    /// 发送命令到核心（异步），返回是否发送成功
    async fn send_cmd(&self, cmd: ChatCommand) -> bool {
        match self.cmd_tx.send(cmd).await {
            Ok(_) => true,
            Err(e) => {
                tracing::info!("Failed to send command: {e}");
                false
            }
        }
    }

    /// 发送命令到核心（非阻塞），返回是否发送成功
    fn try_send_cmd(&self, cmd: ChatCommand) -> bool {
        match self.cmd_tx.try_send(cmd) {
            Ok(_) => true,
            Err(e) => {
                tracing::info!("Failed to send command: {e:?}");
                false
            }
        }
    }

    /// 发送文本消息到指定联系人
    pub async fn send_msg(&self, mldsa_pubkey_hex: &str, text: &str) -> bool {
        self.send_cmd(ChatCommand::SendMessage {
            mldsa_pubkey_hex: mldsa_pubkey_hex.to_string(),
            msgtype: ChatMessageType::Text,
            data: text.as_bytes().to_vec(),
        })
        .await
    }

    /// 生成新身份
    pub async fn generate_identity(&self) -> bool {
        self.send_cmd(ChatCommand::GenerateIdentity).await
    }

    /// 选择当前身份
    pub async fn select_identity(&self, identity_id: String) -> bool {
        self.send_cmd(ChatCommand::SelectIdentity { identity_id })
            .await
    }

    /// 删除身份
    pub async fn delete_identity(&self, identity_id: String) -> bool {
        self.send_cmd(ChatCommand::DeleteIdentity { identity_id })
            .await
    }

    /// 请求下载文件（接收方发起）
    pub async fn request_file_download(
        &self,
        sender_mldsa_pubkey_hex: &str,
        file_hash: [u8; 32],
        save_path: PathBuf,
    ) -> bool {
        self.send_cmd(ChatCommand::RequestFileDownload {
            sender_mldsa_pubkey_hex: sender_mldsa_pubkey_hex.to_string(),
            file_hash,
            save_path,
        })
        .await
    }

    /// 删除联系人
    pub async fn delete_contact(&self, mldsa_pubkey_hex: &str) -> bool {
        let pool = match crate::storage::pool() {
            Some(p) => p,
            None => {
                tracing::info!("Database pool not available, cannot delete contact");
                return false;
            }
        };
        let owner_identity_id = match crate::storage::get_current_identity(pool).await {
            Ok(Some(id)) => id,
            _ => {
                tracing::info!("No current identity, cannot delete contact");
                return false;
            }
        };
        match crate::storage::delete_contact(pool, &owner_identity_id, mldsa_pubkey_hex).await {
            Ok(rows) if rows > 0 => {
                tracing::info!("Deleted contact: {}", &mldsa_pubkey_hex[..16]);
                true
            }
            Ok(_) => {
                tracing::info!("Contact not found: {}", &mldsa_pubkey_hex[..16]);
                false
            }
            Err(e) => {
                tracing::info!("Failed to delete contact: {e}");
                false
            }
        }
    }

    /// 通过 DHT 发现联系人
    pub async fn discover_contact(&self, mldsa_pubkey_hex: &str, name: Option<String>) -> bool {
        self.send_cmd(ChatCommand::DiscoverContact {
            mldsa_pubkey_hex: mldsa_pubkey_hex.to_string(),
            name,
        })
        .await
    }

    /// 添加联系人（通过公钥直接添加）
    pub async fn add_contact(&self, mldsa_pubkey_hex: &str, name: Option<String>) -> bool {
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        let result = self
            .cmd_tx
            .send(ChatCommand::AddContact {
                mldsa_pubkey_hex: mldsa_pubkey_hex.to_string(),
                mlkem_public_key: Vec::new(),
                name,
                resp: resp_tx,
            })
            .await;
        match result {
            Ok(_) => match resp_rx.await {
                Ok(success) => success,
                Err(_) => {
                    tracing::info!("Failed to receive add_contact response");
                    false
                }
            },
            Err(e) => {
                tracing::info!("Failed to add contact: {e}");
                false
            }
        }
    }

    /// 发送关闭命令（非阻塞）
    pub fn shutdown(&self) {
        let _ = self.try_send_cmd(ChatCommand::Shutdown);
    }

    /// 设置是否允许启用中继服务（计费网络检测）
    pub fn set_relay_server_allowed(&self, allowed: bool) {
        self.try_send_cmd(ChatCommand::SetRelayServerAllowed(allowed));
    }

    /// 发送文件（计算文件 hash、注册文件路径、发送 FileHash 消息）
    pub async fn send_file(&self, mldsa_pubkey_hex: &str, file_path: &std::path::Path) -> bool {
        let file_hash = match crate::transfer::compute_file_hash(file_path).await {
            Ok(hash) => hash,
            Err(e) => {
                tracing::info!("Failed to compute file hash: {e}");
                return false;
            }
        };
        let file_id = file_hash;

        let metadata = match tokio::fs::metadata(file_path).await {
            Ok(m) => m,
            Err(e) => {
                tracing::info!("Failed to get file metadata: {e}");
                return false;
            }
        };
        let total_size = metadata.len();
        let filename = file_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown")
            .to_string();

        // 记录到已发送文件历史（sent_files 表）
        if let Some(pool) = crate::storage::pool()
            && let Err(e) = crate::storage::add_sent_file(
                pool,
                &file_hash,
                file_path.to_str().unwrap_or(""),
                &filename,
                total_size,
            )
            .await
            {
                tracing::warn!("记录已发送文件失败: {e}");
            }

        let file_info = crate::message::FileHashInfo::new(filename, total_size, file_hash, file_id);
        let file_info_bytes = match postcard::to_allocvec(&file_info) {
            Ok(b) => b,
            Err(e) => {
                tracing::info!("Failed to serialize FileHashInfo: {e}");
                return false;
            }
        };

        self.send_cmd(ChatCommand::SendMessage {
            mldsa_pubkey_hex: mldsa_pubkey_hex.to_string(),
            msgtype: ChatMessageType::FileHash,
            data: file_info_bytes,
        })
        .await
    }
}
