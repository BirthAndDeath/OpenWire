use tokio::sync::mpsc;

use crate::{ChatCommand, ChatMessage, ChatMessageType};

#[derive(Debug, Clone)]
pub struct CoreHandle {
    pub cmd_tx: mpsc::Sender<crate::ChatCommand>,
}
impl CoreHandle {
    pub async fn send_msg(&self, peer_id: &str, message: crate::ChatMessage) -> bool {
        let peer_id = match peer_id.parse() {
            Ok(p) => p,
            Err(e) => return false,
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
}
