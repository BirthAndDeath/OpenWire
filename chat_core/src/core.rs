use std::{num::NonZeroUsize, path::PathBuf, time::Instant};

use futures::StreamExt;
use libp2p::{PeerId, Swarm, identity};
use lru::LruCache;
use tokio::sync::mpsc;
use tokio::try_join;
use std::thread::available_parallelism;

use crate::{
    command::{ChatCommand, ChatcoreEvent, MessageEvent},
    coreconfig::CoreConfig,
    corehandle::CoreHandle,
    identity::load_or_generate_identity,
    log::init_logger,
    message::{ChatMessage, ChatMessageType},
    p2p, storage,
};

/// 聊天核心：管理 P2P 网络、命令处理、消息分发
pub struct ChatCore {
    /// libp2p 网络 swarm，管理所有连接和协议
    pub swarm: Swarm<p2p::MyBehaviour>,
    /// 当前身份私钥，用于对消息签名
    pub identity_keypair: identity::Keypair,
    /// 消息发送通道：向外部（UI）发送事件
    pub tx_message: mpsc::Sender<ChatcoreEvent>,
    /// 消息接收通道：外部可取走事件（Option 用于 run() 时 take）
    pub rx_message: Option<mpsc::Receiver<ChatcoreEvent>>,
    /// 命令接收通道：接收外部控制指令
    pub rx_cmd: mpsc::Receiver<ChatCommand>,
    pub mdns_cache: LruCache<PeerId, Instant>,
    pub data_dir: PathBuf,
    /// 核心句柄：用于外部控制核心
    pub core_handle: CoreHandle,
}

impl ChatCore {
    /// 异步初始化核心
    const MDNS_CACHE_SIZE: usize = 2000; //mdns缓存大小
    pub async fn try_init(cfg: CoreConfig) -> anyhow::Result<Self> {
        if let Err(e) = init_logger(&cfg) {
            return Err(anyhow::anyhow!("Failed to init logger:{}", e));
        };

        // 确保数据目录存在，防止因目录缺失导致身份文件无法读写而反复生成新身份
        if let Err(e) = std::fs::create_dir_all(&cfg.data_dir) {
            tracing::warn!("Failed to create data directory {:?}: {}", cfg.data_dir, e);
        }
        tracing::info!("Initializing chat core with data dir: {:?}", cfg.data_dir);

        let _ = try_join!(storage::init(&cfg))?;

        // 加载或生成身份
        let keypair = load_or_generate_identity(&cfg).await?;
        // 记录当前加载的身份 ID，便于确认是否复用了旧身份
        tracing::info!(
            "Loaded identity with Peer ID: {}",
            keypair.public().to_peer_id()
        );

        let swarm = p2p::swarm_init(&cfg.data_dir, keypair.clone())?;

        /*// 创建并订阅 Gossipsub 话题
        // 注意：需使用相同话题名才能互通
        let topic = gossipsub::IdentTopic::new("test-net");
        swarm.behaviour_mut().gossipsub.subscribe(&topic)?;*/

        // 创建消息通道：容量 32，背压控制防止内存溢出
        let (tx, rx) = mpsc::channel(64);
        let mdns_cache = LruCache::new(NonZeroUsize::new(Self::MDNS_CACHE_SIZE).unwrap());

        let (cmd_tx, cmd_rx) = mpsc::channel::<ChatCommand>(64);
        Ok(ChatCore {
            swarm,
            identity_keypair: keypair,
            tx_message: tx,
            rx_message: Some(rx),
            rx_cmd: cmd_rx,
            mdns_cache,
            data_dir: cfg.data_dir.clone(),
            core_handle: CoreHandle { cmd_tx },
        })
    }

    /// 启动核心事件循环
    ///
    /// # 返回
    /// JoinHandle：可用于等待线程结束或强制终止
    pub fn run(mut self) -> std::thread::JoinHandle<()> {
        std::thread::spawn(move || {
             // 自动获取 CPU 核心数
        let worker_threads = available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1);

        let rt = tokio::runtime::Builder::new_multi_thread()  // 改为多线程。
            .worker_threads(worker_threads)  
            .enable_all()
                .build()
                .expect("Failed to build tokio runtime");

            rt.block_on(async move {
                // 主事件循环：三路 select
                loop {
                    tokio::select! {
                        // 1. 网络事件：swarm 产生（新连接、消息到达等）
                        event = self.swarm.select_next_some() => {
                           p2p::swarm_event(event, &mut self).await;
                        }

                        // 2. 控制命令：外部发送（UI/CLI)
                        Some(cmd) = self.rx_cmd.recv() => {

                            match cmd {
                                ChatCommand::SendMessage {peerid,msgtype,data} => {
                                    match msgtype {
                                        ChatMessageType::Text => {
                                           match self.send_text(peerid, msgtype, data) {
                                        Ok(_) => {

                                            tracing::info!("Message sent to {}", peerid);
                                        }
                                        Err(e) => {
                                        tracing::error!("Failed to build signed message: {e}");
                                    }
                                    }
                                        }
                                        ChatMessageType::FileHash => {
                                            todo!();
                                            tracing::info!("Received SendMessage command for peer {}: File message with {} bytes", peerid, data.len());

                                        }
                                        ChatMessageType::__NonExhaustive=>{}
                                    }
                                    


                                }

                                ChatCommand::GenerateIdentity => {
                                    self.generate_identity().await;
                                }
                                ChatCommand::SelectIdentity { peer_id } => {
                                    self.select_identity(peer_id).await;
                                }
                                ChatCommand::DeleteIdentity { peer_id } => {
                                    self.delete_identity(peer_id).await;
                                }
                                ChatCommand::Shutdown => {
                                    tracing::info!("P2P thread shutting down...");
                                    break;
                                }
                            }
                        }


                    }
                }
            });
        })
    }

    /// 发送消息到网络
    fn send_message(&mut self, peerid: PeerId, message: ChatMessage) {
        self.swarm
            .behaviour_mut()
            .rr_msg
            .send_request(&peerid, message);
    }

    fn build_signed_message(
        &self,
        msgtype: ChatMessageType,
        data: Vec<u8>,
    ) -> anyhow::Result<ChatMessage> {
        ChatMessage::new_signed(msgtype, data, &self.identity_keypair)
    }

    fn send_text(
        &mut self,
        peerid: PeerId,
        msgtype: ChatMessageType,
        data: Vec<u8>,
    ) -> anyhow::Result<()> {
        let message = self.build_signed_message(msgtype, data)?;
        self.send_message(peerid, message);
        Ok(())
    }

    async fn generate_identity(&mut self) {
        // 使用统一的身份生成逻辑
        let temp_cfg = crate::coreconfig::CoreConfig {
            data_dir: self.data_dir.clone(),
            ..Default::default()
        };

        match crate::identity::generate_identity(&temp_cfg).await {
            Ok(keypair) => {
                let peer_id_str = keypair.public().to_peer_id().to_string();
                // 检查是否使用了本地文件存储（keyring失败）
                match storage::set_private_key(
                    &self.data_dir,
                    &peer_id_str,
                    &keypair.to_protobuf_encoding().unwrap(),
                ) {
                    Ok(true) => {
                        // keyring 失败，使用了本地文件
                        let msg =
                            "Keyring 无法保存私钥，已使用本地备份存储，请及时检查权限或配置。"
                                .to_string();
                        tracing::warn!("{}", msg);
                        self.send_warning_mpsc(msg).await;
                    }
                    Ok(false) => {
                        // 成功使用 keyring
                        tracing::info!("Generated new identity: {}", peer_id_str);
                    }
                    Err(e) => {
                        tracing::error!("Failed to verify private key storage: {e}");
                    }
                }
            }
            Err(e) => {
                tracing::error!("Failed to generate identity: {e}");
            }
        }
    }
    ///外部需重启应用
    async fn select_identity(&mut self, peer_id: String) {
        if let Some(pool) = storage::pool() {
            if let Err(e) = storage::set_current_identity(pool, &peer_id).await {
                tracing::error!("Failed to select identity: {e}");
            } else {
                tracing::info!("Selected identity: {}", peer_id);
            }
        }
    }

    async fn delete_identity(&mut self, peer_id: String) {
        if let Some(pool) = storage::pool() {
            if let Err(e) = storage::delete_identity(pool, &self.data_dir, &peer_id).await {
                tracing::error!("Failed to delete identity: {e}");
            } else {
                tracing::info!("Deleted identity: {}", peer_id);
            }
        }
    }

    /// 发送日志事件到外部通道
    async fn send_log_mpsc(&mut self, data: String) {
        let message = ChatcoreEvent {
            event: MessageEvent::Log,
            data,
        };
        // 使用 expect：通道关闭意味着 UI 已退出，核心也应终止
        if let Err(e) = self.tx_message.send(message).await {
            tracing::error!("Failed to send log message: {e}");
        }
    }

    /// 发送警告事件到外部通道
    async fn send_warning_mpsc(&mut self, data: String) {
        let message = ChatcoreEvent {
            event: MessageEvent::Warning,
            data,
        };
        if let Err(e) = self.tx_message.send(message).await {
            tracing::error!("Failed to send warning message: {e}");
        }
    }

    /// 发送新消息事件到外部通道
    pub async fn send_message_mpsc(&mut self, data: String) {
        let message = ChatcoreEvent {
            event: MessageEvent::ReceiveMessage,
            data,
        };
        if let Err(e) = self.tx_message.send(message).await {
            tracing::error!("Failed to send message event: {e}");
        }
    }
}
