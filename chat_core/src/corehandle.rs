use tokio::sync::mpsc;

use crate::{ChatCommand, ChatMessageType};

#[derive(Debug, Clone)]
pub struct CoreHandle {
    pub cmd_tx: mpsc::Sender<crate::ChatCommand>,
}
impl CoreHandle {
    pub async fn send_msg(&self, peer_id: &str, message: crate::ChatMessage) -> bool {
        let peer_id = match peer_id.parse() {
            Ok(p) => p,
            Err(_) => return false,
        };
        let result = self
            .cmd_tx
            .send(ChatCommand::SendMessage {
                peerid: peer_id,
                message,
            })
            .await;

        match result {
            Ok(_) => true,
            Err(e) => {
                tracing::info!("Failed to send message: {e}");
                false
            }
        }
    }

    pub async fn send_text(&self, peer_id: &str, text: &str) -> bool {
        let peer_id = match peer_id.parse() {
            Ok(p) => p,
            Err(_) => return false,
        };
        let result = self
            .cmd_tx
            .send(ChatCommand::SendText {
                peerid: peer_id,
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

    pub async fn select_identity(&self, peer_id: String) -> bool {
        let result = self
            .cmd_tx
            .send(ChatCommand::SelectIdentity { peer_id })
            .await;
        match result {
            Ok(_) => true,
            Err(e) => {
                tracing::info!("Failed to send select identity: {e}");
                false
            }
        }
    }

    pub async fn delete_identity(&self, peer_id: String) -> bool {
        let result = self
            .cmd_tx
            .send(ChatCommand::DeleteIdentity { peer_id })
            .await;
        match result {
            Ok(_) => true,
            Err(e) => {
                tracing::info!("Failed to send delete identity: {e}");
                false
            }
        }
    }
}
