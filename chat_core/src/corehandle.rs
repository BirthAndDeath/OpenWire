use std::path::PathBuf;
use tokio::sync::mpsc;

use crate::{ChatCommand, ChatMessageType};

#[derive(Debug, Clone)]
pub struct CoreHandle {
    pub cmd_tx: mpsc::Sender<crate::ChatCommand>,
}
impl CoreHandle {
    /// 发送文本消息到指定联系人（通过 ML-DSA 公钥 hex 标识）
    pub async fn send_msg(&self, mldsa_pubkey_hex: &str, text: &str) -> bool {
        let result = self
            .cmd_tx
            .send(ChatCommand::SendMessage {
                mldsa_pubkey_hex: mldsa_pubkey_hex.to_string(),
                msgtype: ChatMessageType::Text,
                data: text.as_bytes().to_vec(),
            })
            .await;

        match result {
            Ok(_) => true,
            Err(e) => {
                tracing::info!("Failed to send text message: {e}");
                false
            }
        }
    }

    pub async fn generate_identity(&self) -> bool {
        let result = self.cmd_tx.send(ChatCommand::GenerateIdentity).await;
        match result {
            Ok(_) => true,
            Err(e) => {
                tracing::info!("Failed to send generate identity: {e}");
                false
            }
        }
    }

    pub async fn select_identity(&self, identity_id: String) -> bool {
        let result = self
            .cmd_tx
            .send(ChatCommand::SelectIdentity { identity_id })
            .await;
        match result {
            Ok(_) => true,
            Err(e) => {
                tracing::info!("Failed to send select identity: {e}");
                false
            }
        }
    }

    pub async fn delete_identity(&self, identity_id: String) -> bool {
        let result = self
            .cmd_tx
            .send(ChatCommand::DeleteIdentity { identity_id })
            .await;
        match result {
            Ok(_) => true,
            Err(e) => {
                tracing::info!("Failed to send delete identity: {e}");
                false
            }
        }
    }

    /// 请求下载文件（接收方发起）
    ///
    /// 安全说明：下载目录由 SetDownloadDir 命令统一管理，
    /// 不从请求中接受 download_dir 参数，防止路径遍历攻击。
    pub async fn request_file_download(
        &self,
        sender_mldsa_pubkey_hex: &str,
        file_id: [u8; 32],
        _download_dir: Option<PathBuf>,
    ) -> bool {
        let result = self
            .cmd_tx
            .send(ChatCommand::RequestFileDownload {
                sender_mldsa_pubkey_hex: sender_mldsa_pubkey_hex.to_string(),
                file_id,
            })
            .await;
        match result {
            Ok(_) => true,
            Err(e) => {
                tracing::info!("Failed to send file download request: {e}");
                false
            }
        }
    }

    /// 设置下载目录
    pub async fn set_download_dir(&self, path: PathBuf) -> bool {
        let result = self.cmd_tx.send(ChatCommand::SetDownloadDir { path }).await;
        match result {
            Ok(_) => true,
            Err(e) => {
                tracing::info!("Failed to set download dir: {e}");
                false
            }
        }
    }

    /// 注册文件供下载（发送方在发送 FileHash 后调用）
    /// 记录 file_id -> 本地文件路径 的映射，以便收到下载请求时能找到文件
    pub async fn register_file_for_download(&self, file_id: [u8; 32], file_path: PathBuf) -> bool {
        let result = self
            .cmd_tx
            .send(ChatCommand::RegisterFileForDownload { file_id, file_path })
            .await;
        match result {
            Ok(_) => true,
            Err(e) => {
                tracing::info!("Failed to register file for download: {e}");
                false
            }
        }
    }

    /// 添加联系人
    /// 通过 DHT 发现并添加联系人
    ///
    /// 在 DHT 网络中查询联系人的 PeerID 和 ML-KEM 公钥，
    /// 如果找到则自动添加联系人。
    pub async fn discover_contact(&self, mldsa_pubkey_hex: &str, name: Option<String>) -> bool {
        let result = self
            .cmd_tx
            .send(ChatCommand::DiscoverContact {
                mldsa_pubkey_hex: mldsa_pubkey_hex.to_string(),
                name,
            })
            .await;
        match result {
            Ok(_) => true,
            Err(e) => {
                tracing::info!("Failed to discover contact: {e}");
                false
            }
        }
    }

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

    /// 发送关闭命令（非阻塞，使用 try_send 避免在清理/析构场景中阻塞）
    pub fn shutdown(&self) {
        let _ = self.cmd_tx.try_send(ChatCommand::Shutdown);
    }

    /// 发送文件（计算文件 hash、注册文件路径、发送 FileHash 消息）
    pub async fn send_file(&self, mldsa_pubkey_hex: &str, file_path: &std::path::Path) -> bool {
        // 计算文件 hash
        let file_hash = match crate::transfer::compute_file_hash(file_path).await {
            Ok(hash) => hash,
            Err(e) => {
                tracing::info!("Failed to compute file hash: {e}");
                return false;
            }
        };
        let file_id = file_hash;

        // 获取文件元数据
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

        // 构建 FileHashInfo
        let file_info = crate::message::FileHashInfo::new(filename, total_size, file_hash, file_id);
        let file_info_bytes = match postcard::to_allocvec(&file_info) {
            Ok(b) => b,
            Err(e) => {
                tracing::info!("Failed to serialize FileHashInfo: {e}");
                return false;
            }
        };

        // 先注册文件路径
        if !self
            .register_file_for_download(file_id, file_path.to_path_buf())
            .await
        {
            return false;
        }

        // 发送 FileHash 消息
        let result = self
            .cmd_tx
            .send(ChatCommand::SendMessage {
                mldsa_pubkey_hex: mldsa_pubkey_hex.to_string(),
                msgtype: ChatMessageType::FileHash,
                data: file_info_bytes,
            })
            .await;
        match result {
            Ok(_) => true,
            Err(e) => {
                tracing::info!("Failed to send file message: {e}");
                false
            }
        }
    }
}
