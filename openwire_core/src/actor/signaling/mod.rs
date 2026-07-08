//! WebSocket 信令 Actor
//!
//! 连接到 Cloudflare Workers 的信令房间，交换 PeerId + Multiaddr，
//! 帮助 NAT 后的节点发现彼此的地址，然后通过 libp2p 直接 dial。
//!
//! Workers 只做地址信令交换，不中转任何数据。

use std::time::Duration;

use futures::{SinkExt, StreamExt};
use tokio::sync::mpsc;
use tokio::sync::watch;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;
use tokio_util::sync::CancellationToken;

use crate::actor::p2p::P2pCommand;
use crate::actor::ActorCommand;
use libp2p::{Multiaddr, PeerId};

/// 信令事件：通知 ChatCore 信令层的状态变化
#[derive(Debug, Clone)]
pub enum SignalingEvent {
    /// 发现新对端节点
    PeerDiscovered {
        /// 发现的节点 PeerId
        peer_id: PeerId,
        /// 该节点已知的监听地址列表
        addrs: Vec<Multiaddr>,
    },
    /// 对端节点离开
    PeerLost {
        /// 离开的节点 PeerId
        peer_id: PeerId,
    },
    /// 已连接到信令服务器
    Connected,
    /// 与信令服务器断开连接
    Disconnected,
    /// 发生错误
    Error(String),
}

/// WebSocket 信令 Actor
pub struct SignalingActor {
    /// 信令服务器主机名
    server_host: String,
    /// 信令房间名
    room: String,
    /// 本节点 PeerId
    peer_id: PeerId,
    /// 监听地址更新通道发送端
    listen_addrs_tx: watch::Sender<Vec<Multiaddr>>,
    /// P2pActor 命令通道发送端
    p2p_cmd_tx: mpsc::Sender<ActorCommand<P2pCommand>>,
    /// 信令事件通道发送端
    event_tx: mpsc::Sender<SignalingEvent>,
    /// 关闭信号 token
    shutdown_token: CancellationToken,
}

impl SignalingActor {
    /// 创建新的 SignalingActor
    pub fn new(
        server_host: impl Into<String>,
        room: impl Into<String>,
        peer_id: PeerId,
        p2p_cmd_tx: mpsc::Sender<ActorCommand<P2pCommand>>,
        event_tx: mpsc::Sender<SignalingEvent>,
        shutdown_token: CancellationToken,
    ) -> Self {
        let (tx, _) = watch::channel(Vec::new());
        Self {
            server_host: server_host.into(),
            room: room.into(),
            peer_id,
            listen_addrs_tx: tx,
            p2p_cmd_tx,
            event_tx,
            shutdown_token,
        }
    }

    /// 获取监听地址更新发送端
    pub fn listen_addrs_updater(&self) -> watch::Sender<Vec<Multiaddr>> {
        self.listen_addrs_tx.clone()
    }

    /// 启动 Actor（在全局运行时上 spawn）
    pub fn start(self) {
        let ws_url = format!("wss://{}/api/signal/{}", self.server_host, self.room);
        crate::actor::RUNTIME.spawn(async move { self.run(&ws_url).await; });
    }

    async fn run(&self, ws_url: &str) {
        loop {
            tokio::select! {
                _ = self.shutdown_token.cancelled() => {
                    tracing::info!("SignalingActor shutting down");
                    return;
                }
                result = self.connect_and_loop(ws_url) => {
                    if let Err(e) = &result {
                        tracing::warn!("SignalingActor disconnected (will retry): {e}");
                        let _ = self.event_tx.try_send(SignalingEvent::Error(e.to_string()));
                    }
                    let _ = self.event_tx.try_send(SignalingEvent::Disconnected);
                    tokio::select! {
                        _ = tokio::time::sleep(Duration::from_secs(5)) => {}
                        _ = self.shutdown_token.cancelled() => return,
                    }
                }
            }
        }
    }

    async fn connect_and_loop(&self, ws_url: &str) -> anyhow::Result<()> {
        tracing::info!("SignalingActor connecting to {ws_url}");
        let (ws_stream, _) = connect_async(ws_url).await?;
        let (mut write, mut read) = ws_stream.split();

        let _ = self.event_tx.try_send(SignalingEvent::Connected);

        let addrs: Vec<String> = self.listen_addrs_tx.borrow().iter().map(|a| a.to_string()).collect();
        let register = serde_json::json!({
            "type": "register",
            "peer_id": self.peer_id.to_string(),
            "addrs": addrs,
        });
        write.send(Message::Text(register.to_string())).await?;

        let mut addr_rx = self.listen_addrs_tx.subscribe();
        let write_for_addrs = {
            let peer_id = self.peer_id;
            let mut write = write;
            tokio::spawn(async move {
                loop {
                    if addr_rx.changed().await.is_err() { break; }
                    let addrs: Vec<String> = addr_rx.borrow().iter().map(|a| a.to_string()).collect();
                    let update = serde_json::json!({
                        "type": "register",
                        "peer_id": peer_id.to_string(),
                        "addrs": addrs,
                    });
                    if write.send(Message::Text(update.to_string())).await.is_err() { break; }
                }
            })
        };

        loop {
            tokio::select! {
                _ = self.shutdown_token.cancelled() => {
                    write_for_addrs.abort();
                    return Ok(());
                }
                msg = read.next() => {
                    match msg {
                        Some(Ok(Message::Text(text))) => self.handle_message(&text).await,
                        Some(Ok(Message::Close(_))) => {
                            write_for_addrs.abort();
                            anyhow::bail!("WebSocket closed by server");
                        }
                        Some(Err(e)) => {
                            write_for_addrs.abort();
                            anyhow::bail!(e);
                        }
                        None => {
                            write_for_addrs.abort();
                            anyhow::bail!("WebSocket stream ended");
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    async fn handle_message(&self, text: &str) {
        let data: serde_json::Value = match serde_json::from_str(text) {
            Ok(v) => v,
            Err(_) => return,
        };

        match data["type"].as_str() {
            Some("peer") => {
                let peer_id_str = match data["peer_id"].as_str() {
                    Some(s) => s,
                    None => return,
                };
                let peer_id = match peer_id_str.parse::<PeerId>() {
                    Ok(id) => id,
                    Err(_) => return,
                };
                let addrs: Vec<Multiaddr> = data["addrs"]
                    .as_array()
                    .map(|arr| arr.iter().filter_map(|v| v.as_str()?.parse().ok()).collect())
                    .unwrap_or_default();

                let _ = self.event_tx.try_send(SignalingEvent::PeerDiscovered {
                    peer_id,
                    addrs: addrs.clone(),
                });

                for addr in addrs {
                    let full = addr.clone().with_p2p(peer_id).unwrap_or(addr.clone());
                    let _ = self.p2p_cmd_tx.try_send(
                        ActorCommand::Custom(P2pCommand::DialAddr { addr: full }),
                    );
                }
            }
            Some("peer_left") => {
                if let Some(pid_str) = data["peer_id"].as_str() {
                    if let Ok(peer_id) = pid_str.parse::<PeerId>() {
                        let _ = self.event_tx.try_send(SignalingEvent::PeerLost { peer_id });
                    }
                }
            }
            Some("signal") => {
                if let Some(from_str) = data["from"].as_str() {
                    if let Ok(from) = from_str.parse::<PeerId>() {
                        let _ = self.event_tx.try_send(SignalingEvent::PeerDiscovered {
                            peer_id: from,
                            addrs: vec![],
                        });
                    }
                }
            }
            _ => {}
        }
    }
}