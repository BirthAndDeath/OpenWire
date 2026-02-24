use futures::StreamExt;
use libp2p::{
    Swarm,
    futures::io,
    gossipsub, mdns, noise,
    swarm::{NetworkBehaviour, SwarmEvent},
    tcp, yamux,
};
use std::{
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
    path::PathBuf,
    time::Duration,
};
use tokio::sync::mpsc;
use tokio::try_join;
mod log;
mod storage;
use log::init_logger;
pub struct CoreConfig {
    /// example: "/path/to/database.db"
    database_path: PathBuf,
    // 命令
    rx_cmd: mpsc::Receiver<ChatCommand>,
    path_to_log: Option<PathBuf>,
    log_level: Option<String>,
}
impl CoreConfig {
    pub fn new(
        database_path: impl Into<PathBuf>,
        rx_cmd: mpsc::Receiver<ChatCommand>,
        path_to_log: Option<impl Into<PathBuf>>,
        log_level: Option<impl Into<String>>,
    ) -> Self {
        if let None = path_to_log {
            return Self {
                database_path: database_path.into(),
                rx_cmd,
                path_to_log: None,
                log_level: None,
            };
        }
        Self {
            database_path: database_path.into(),
            rx_cmd,
            path_to_log: Some(path_to_log.unwrap().into()),
            log_level: log_level.map(|s| s.into()),
        }
    }
}
pub enum MessageEvent {
    NewMessage,
    Error,
    Log,
}
#[derive(Debug)]
pub enum ChatCommand {
    SendMessage { message: String },
    Shutdown,
}
pub struct ChatMeassage {
    pub event: MessageEvent,
    pub data: String,
}

#[derive(NetworkBehaviour)]
pub struct MyBehaviour {
    gossipsub: gossipsub::Behaviour,
    mdns: mdns::tokio::Behaviour,
}
pub struct ChatCore {
    pub swarm: Swarm<MyBehaviour>,
    pub topic: gossipsub::IdentTopic,
    pub tx_message: tokio::sync::mpsc::Sender<ChatMeassage>,
    pub rx_message: Option<tokio::sync::mpsc::Receiver<ChatMeassage>>,
    pub rx_cmd: mpsc::Receiver<ChatCommand>, // 命令
}
impl ChatCore {
    pub async fn try_init(cfg: CoreConfig) -> anyhow::Result<Self> {
        let mut swarm = swarm_init()?;

        // Create a Gossipsub topic
        let topic = gossipsub::IdentTopic::new("test-net");
        // subscribes to our topic
        swarm.behaviour_mut().gossipsub.subscribe(&topic)?;
        let (tx, rx) = mpsc::channel(32);
        try_join!(init_logger(&cfg), storage::init(&cfg))?;

        Ok(ChatCore {
            swarm,
            tx_message: tx,
            rx_message: Some(rx),
            topic,
            rx_cmd: cfg.rx_cmd,
        })
    }
    pub fn run(mut self) -> std::thread::JoinHandle<()> {
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();

            rt.block_on(async move {
                loop {
                    tokio::select! {
                        // Swarm 网络事件
                        event = self.swarm.select_next_some() => {
                           swarm_event(event, &mut self).await;
                        }
                        Some(cmd) = self.rx_cmd.recv() => {
                            match cmd {
                                ChatCommand::SendMessage { message } => {
                                    self.send_message(message);
                                }
                                ChatCommand::Shutdown => {
                                    tracing::info!("p2p thread shutting down...");
                                    break;
                                }
                            }
                        }// 心跳
                        _ = tokio::time::sleep(std::time::Duration::from_millis(100)) => {}
                    }
                }
            });
        })
    }
    pub fn send_message(&mut self, data: String) {
        if let Err(e) = self
            .swarm
            .behaviour_mut()
            .gossipsub
            .publish(self.topic.clone(), data.as_bytes())
        {
            tracing::error!("Publish error: {e:?}");
        }
    }
    async fn sendlog_mpsc(&mut self, data: String) {
        let message = ChatMeassage {
            event: MessageEvent::Log,
            data,
        };
        let _ = &self
            .tx_message
            .send(message)
            .await
            .expect("falied send message:tx_message ");
    }
    async fn sendmessage_mpsc(&mut self, data: String) {
        let message = ChatMeassage {
            event: MessageEvent::NewMessage,
            data,
        };
        let _ = &self
            .tx_message
            .send(message)
            .await
            .expect("falied send message:tx_message ");
    }
}
fn swarm_init() -> anyhow::Result<Swarm<MyBehaviour>> {
    let mut swarm = libp2p::SwarmBuilder::with_new_identity()
        .with_tokio()
        .with_tcp(
            tcp::Config::default(),
            noise::Config::new,
            yamux::Config::default,
        )?
        .with_quic()
        .with_behaviour(|key| {
            // To content-address message, we can take the hash of message and use it as an ID.
            let message_id_fn = |message: &gossipsub::Message| {
                let mut s = DefaultHasher::new();
                message.data.hash(&mut s);
                gossipsub::MessageId::from(s.finish().to_string())
            };

            // Set a custom gossipsub configuration
            let gossipsub_config = gossipsub::ConfigBuilder::default()
                .heartbeat_interval(Duration::from_secs(10)) // This is set to aid debugging by not cluttering the log space
                .validation_mode(gossipsub::ValidationMode::Strict) // This sets the kind of message validation. The default is Strict (enforce message
                // signing)
                .message_id_fn(message_id_fn) // content-address messages. No two messages of the same content will be propagated.
                .build()
                .map_err(io::Error::other)?; // Temporary hack because `build` does not return a proper `std::error::Error`.

            // build a gossipsub network behaviour
            let gossipsub = gossipsub::Behaviour::new(
                gossipsub::MessageAuthenticity::Signed(key.clone()),
                gossipsub_config,
            )?;

            let mdns =
                mdns::tokio::Behaviour::new(mdns::Config::default(), key.public().to_peer_id())?;
            Ok(MyBehaviour { gossipsub, mdns })
        })?
        .build();
    swarm.listen_on("/ip4/0.0.0.0/udp/0/quic-v1".parse()?)?;
    swarm.listen_on("/ip4/0.0.0.0/tcp/0".parse()?)?;
    swarm.listen_on("/ip6/::/udp/0/quic-v1".parse()?)?;
    swarm.listen_on("/ip6/::/tcp/0".parse()?)?;

    Ok(swarm)
}
pub async fn swarm_event(event: SwarmEvent<MyBehaviourEvent>, core: &mut ChatCore) {
    match event {
        SwarmEvent::Behaviour(MyBehaviourEvent::Mdns(mdns::Event::Discovered(list))) => {
            for (peer_id, _multiaddr) in list {
                core.sendlog_mpsc(format!("mDNS discovered a new peer: {peer_id}"))
                    .await;
                //eprintln!(">>> DISCOVERED: {} ", peer_id);
                core.swarm
                    .behaviour_mut()
                    .gossipsub
                    .add_explicit_peer(&peer_id);
            }
        }
        SwarmEvent::Behaviour(MyBehaviourEvent::Mdns(mdns::Event::Expired(list))) => {
            for (peer_id, _multiaddr) in list {
                core.sendlog_mpsc(format!("mDNS discover peer has expired: {peer_id}"))
                    .await;
                core.swarm
                    .behaviour_mut()
                    .gossipsub
                    .remove_explicit_peer(&peer_id);
            }
        }
        SwarmEvent::Behaviour(MyBehaviourEvent::Gossipsub(gossipsub::Event::Message {
            propagation_source: peer_id,
            message_id: id,
            message,
        })) => {
            core.sendmessage_mpsc(format!(
                " peerid: {peer_id} id:{id} '{}'",
                String::from_utf8_lossy(&message.data)
            ))
            .await;
        }
        SwarmEvent::NewListenAddr { address, .. } => {
            core.sendlog_mpsc(format!("Local node is listening on {address}"))
                .await;
        }
        SwarmEvent::ConnectionEstablished { peer_id, .. } => {}
        SwarmEvent::ConnectionClosed { peer_id, .. } => {}
        SwarmEvent::OutgoingConnectionError { peer_id, .. } => {
            if peer_id.is_some() {
                core.sendlog_mpsc(format!("OutgoingConnectionError: {peer_id:?}"))
                    .await;
            }
        }
        SwarmEvent::Dialing {
            peer_id,
            connection_id,
        } => {}
        SwarmEvent::IncomingConnectionError { local_addr, .. } => {
            core.sendlog_mpsc(format!("IncomingConnectionError: {local_addr:?}"))
                .await;
        }
        SwarmEvent::ListenerClosed {
            listener_id,
            addresses,
            reason,
        } => {}
        SwarmEvent::ListenerError { listener_id, error } => {}
        SwarmEvent::NewExternalAddrOfPeer { peer_id, address } => {
            core.sendlog_mpsc(format!("NewExternalAddrOfPeer: {peer_id:?} {address:?}"))
                .await;
        }

        SwarmEvent::ExpiredListenAddr {
            listener_id,
            address,
        } => {
            core.sendlog_mpsc(format!("ExpiredListenAddr: {listener_id:?} {address:?}"))
                .await;
        }
        _ => {}
    }
}
