use std::path::PathBuf;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::{ChatCommand, ChatMessageType, ScanContactInfo};

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

    /// 限时等待核心对某条命令的响应：核心卡死/关停时返回 None，
    /// 避免上层（Tauri 命令 → UI）无限挂起。
    async fn await_resp<T>(rx: tokio::sync::oneshot::Receiver<T>) -> Option<T> {
        tokio::time::timeout(std::time::Duration::from_secs(10), rx)
            .await
            .ok()
            .and_then(|r| r.ok())
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
        let Some(owner_identity_id) = crate::storage::current_identity_id(pool).await else {
            tracing::info!("No current identity, cannot delete contact");
            return false;
        };
        match crate::storage::delete_contact(pool, &owner_identity_id, mldsa_pubkey_hex).await {
            Ok(rows) if rows > 0 => {
                tracing::info!("Deleted contact: {}", &mldsa_pubkey_hex[..16.min(mldsa_pubkey_hex.len())]);
                true
            }
            Ok(_) => {
                tracing::info!("Contact not found: {}", &mldsa_pubkey_hex[..16.min(mldsa_pubkey_hex.len())]);
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
            Ok(_) => Self::await_resp(resp_rx).await.unwrap_or_else(|| {
                tracing::info!("Failed to receive add_contact response (timeout)");
                false
            }),
            Err(e) => {
                tracing::info!("Failed to add contact: {e}");
                false
            }
        }
    }

    /// 扫码添加联系人（仅凭 SHA256 摘要；完整公钥经 FriendOnline 匹配后自动完成添加）
    pub async fn add_contact_by_scan(&self, info: ScanContactInfo) -> bool {
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        let result = self
            .cmd_tx
            .send(ChatCommand::AddContactByScan {
                info,
                resp: resp_tx,
            })
            .await;
        match result {
            Ok(_) => Self::await_resp(resp_rx).await.unwrap_or_else(|| {
                tracing::info!("Failed to receive add_contact_by_scan response (timeout)");
                false
            }),
            Err(e) => {
                tracing::info!("Failed to add contact by scan: {e}");
                false
            }
        }
    }

    /// 查询身份分享信息（v2 二进制载荷，供前端生成二维码）
    pub async fn get_identity_share_info(&self) -> Option<Vec<u8>> {
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        if self
            .cmd_tx
            .send(ChatCommand::GetIdentityShareInfo { resp: resp_tx })
            .await
            .is_err()
        {
            return None;
        }
        Self::await_resp(resp_rx).await
    }

    /// 取消文件传输（hex 的 file_hash）
    pub async fn cancel_file_transfer(&self, file_id_hex: &str) -> bool {
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        let result = self
            .cmd_tx
            .send(ChatCommand::CancelFileTransfer {
                file_id_hex: file_id_hex.to_string(),
                resp: resp_tx,
            })
            .await;
        match result {
            Ok(_) => Self::await_resp(resp_rx).await.unwrap_or_else(|| {
                tracing::info!("Failed to receive cancel_file_transfer response (timeout)");
                false
            }),
            Err(e) => {
                tracing::warn!("Failed to cancel file transfer: {e}");
                false
            }
        }
    }

    /// 按关键词搜索所有联系人的消息内容（返回 JSON 数组字符串）
    pub async fn search_messages(&self, query: &str) -> Option<String> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        if self
            .cmd_tx
            .send(ChatCommand::SearchMessages { query: query.to_string(), resp: tx })
            .await
            .is_err()
        {
            return None;
        }
        Self::await_resp(rx).await
    }

    /// 存储管理：每位联系人的消息/文件占用统计（返回 JSON 数组字符串）
    pub async fn get_storage_stats(&self) -> Option<String> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        if self
            .cmd_tx
            .send(ChatCommand::GetStorageStats { resp: tx })
            .await
            .is_err()
        {
            return None;
        }
        Self::await_resp(rx).await
    }

    /// 删除指定联系人的聊天记录
    pub async fn delete_contact_history(&self, mldsa_pubkey_hex: &str) -> bool {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let _ = self
            .cmd_tx
            .send(ChatCommand::DeleteContactHistory {
                mldsa_pubkey_hex: mldsa_pubkey_hex.to_string(),
                resp: tx,
            })
            .await;
        Self::await_resp(rx).await.unwrap_or(false)
    }

    /// 删除指定联系人的已发送文件（记录 + 本地文件），返回释放字节数
    pub async fn delete_contact_files(&self, mldsa_pubkey_hex: &str) -> Option<i64> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let _ = self
            .cmd_tx
            .send(ChatCommand::DeleteContactFiles {
                mldsa_pubkey_hex: mldsa_pubkey_hex.to_string(),
                resp: tx,
            })
            .await;
        Self::await_resp(rx).await.flatten()
    }

    /// 发送关闭命令（非阻塞）
    pub fn shutdown(&self) {
        let _ = self.try_send_cmd(ChatCommand::Shutdown);
    }

    /// 更新联系人元数据（自定义昵称/备注/分组/屏蔽；None=不修改，Some("")=清空）
    #[allow(clippy::too_many_arguments)]
    pub async fn update_contact_meta(
        &self,
        mldsa_pubkey_hex: &str,
        name: Option<String>,
        notes: Option<String>,
        group_name: Option<String>,
        blocked: Option<bool>,
    ) -> bool {
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        let result = self
            .cmd_tx
            .send(ChatCommand::UpdateContactMeta {
                mldsa_pubkey_hex: mldsa_pubkey_hex.to_string(),
                name,
                notes,
                group_name,
                blocked,
                resp: resp_tx,
            })
            .await;
        match result {
            Ok(_) => Self::await_resp(resp_rx).await.unwrap_or(false),
            Err(e) => {
                tracing::info!("Failed to send update_contact_meta: {e}");
                false
            }
        }
    }

    /// 发送文件（校验 → 计算 hash → 记录 sent_files → 发送 FileHash 消息）。
    /// 与 Tauri 命令层共用 `transfer::prepare_and_send_file`，避免两套实现漂移。
    pub async fn send_file(&self, mldsa_pubkey_hex: &str, file_path: &std::path::Path) -> bool {
        crate::transfer::prepare_and_send_file(&self.cmd_tx, mldsa_pubkey_hex, file_path)
            .await
            .is_ok()
    }
}
