use std::{num::NonZeroUsize, path::PathBuf, sync::Arc, time::Instant};

use futures::StreamExt;
use libp2p::{PeerId, Swarm};
use lru::LruCache;
use tokio::sync::mpsc;
use tokio::try_join;
use std::thread::available_parallelism;

use crate::{
    command::{ChatCommand, ChatcoreEvent, MessageEvent},
    coreconfig::CoreConfig,
    corehandle::CoreHandle,
    crypto,
    identity,
    log::init_logger,
    message::{ChatMessage, ChatMessageType},
    p2p, storage,
};

/// 聊天核心：管理 P2P 网络、命令处理、消息分发
pub struct ChatCore {
    /// libp2p 网络 swarm，管理所有连接和协议
    pub swarm: Swarm<p2p::MyBehaviour>,
    /// DHT 挑战验证器
    pub validator: Arc<std::sync::RwLock<p2p::ChallengeValidator>>,
    /// 当前身份私钥，用于对消息签名
    pub identity_keypair: libp2p::identity::Keypair,
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
    /// 保存 ML-KEM 公钥十六进制字符串，用于后续 DHT 注册
    pub mlkem_pubkey_hex: Option<String>,
    /// 保存当前临时 PeerID，用于后续 DHT 注册
    pub current_peer_id: Option<PeerId>,
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

        // 加载或生成ML-KEM身份（持久化身份）
        let mlkem_public_key= identity::load_or_generate_mlkem_identity(&cfg).await?;
        //注意：私钥使用handle去处理
        let mlkem_pubkey_hex = hex::encode(&mlkem_public_key);
        tracing::info!(
            "Loaded ML-KEM identity: {}",
            mlkem_pubkey_hex
        );

        // 为当前会话生成临时的 libp2p PeerID（不持久化）
        let keypair = identity::generate_temporary_peerid()?;
        let peer_id = keypair.public().to_peer_id();
        tracing::info!(
            "Generated temporary PeerID for transport: {}",
            peer_id
        );

        let p2p::SwarmWithValidator { swarm, validator } = p2p::swarm_init(&cfg.data_dir, keypair.clone())?;

        

        // 创建消息通道：容量 32，背压控制防止内存溢出
        let (tx, rx) = mpsc::channel(64);
        let mdns_cache = LruCache::new(NonZeroUsize::new(Self::MDNS_CACHE_SIZE).unwrap());

        let (cmd_tx, cmd_rx) = mpsc::channel::<ChatCommand>(64);
        
        // 保存 ML-KEM pubkey 用于后续 DHT 注册
        let mlkem_pubkey_hex_for_dht = mlkem_pubkey_hex.clone();
        let peer_id_for_dht = peer_id;
        
        Ok(ChatCore {
            swarm,
            validator,
            identity_keypair: keypair,
            tx_message: tx,
            rx_message: Some(rx),
            rx_cmd: cmd_rx,
            mdns_cache,
            data_dir: cfg.data_dir.clone(),
            core_handle: CoreHandle { cmd_tx },
            mlkem_pubkey_hex: Some(mlkem_pubkey_hex_for_dht),
            current_peer_id: Some(peer_id_for_dht),
        })
    }
    pub fn handler(&self) -> CoreHandle {
        self.core_handle.clone()
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

            let rt = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(worker_threads)
                .enable_all()
                .build()
                .expect("Failed to build tokio runtime");

            rt.block_on(async move {
                // 启动 DHT 定期注册任务（优化版本：复用数据库连接）
                let mlkem_pubkey = self.mlkem_pubkey_hex.clone();
                let peer_id = self.current_peer_id;
                let data_dir = self.data_dir.clone();
                
                if let (Some(pubkey), Some(pid)) = (mlkem_pubkey, peer_id) {
                    tokio::spawn(async move {
                        // 打开一次数据库，复用连接
                        let dht_path = data_dir.join("dht.redb");
                        let db = match redb::Database::create(&dht_path) {
                            Ok(db) => Arc::new(db),
                            Err(e) => {
                                tracing::error!("Failed to open DHT database: {}", e);
                                return; // 退出任务
                            }
                        };
                        
                        let mut interval = tokio::time::interval(std::time::Duration::from_secs(300)); // 每5分钟注册一次
                        loop {
                            interval.tick().await;
                            
                            let store = p2p::dht::RedbRecordStore::new(db.clone());
                            if let Err(e) = store.set_pubkey_peerid(&pubkey, &pid) {
                                tracing::warn!("Failed to refresh DHT registration: {}", e);
                            } else {
                                tracing::debug!("Refreshed DHT registration: {} -> {}", pubkey, pid);
                            }
                        }
                    });
                }
                
                // 启动 Validator 定期清理任务
                let validator = self.validator.clone();
                tokio::spawn(async move {
                    let mut interval = tokio::time::interval(std::time::Duration::from_secs(60)); // 每分钟清理一次
                    loop {
                        interval.tick().await;
                        if let Ok(mut v) = validator.write() {
                            v.cleanup_expired_challenges();
                        }
                    }
                });
                
                // 主事件循环：处理网络事件和控制命令
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
                                            match self.send_text(peerid, msgtype, data).await {
                                                Ok(_) => {
                                                    tracing::info!("Message sent to {}", peerid);
                                                }
                                                Err(e) => {
                                                    tracing::error!("Failed to send message: {e}");
                                                }
                                            }
                                        }
                                        ChatMessageType::FileHash => {
                                            tracing::info!("Received SendMessage command for peer {}: File message with {} bytes", peerid, data.len());
                                            // TODO: Implement file hash message handling
                                        }
                                        ChatMessageType::__NonExhaustive=>{}
                                    }
                                }

                                ChatCommand::AddContact { peer_id, public_key, name } => {
                                    self.add_contact(peer_id, public_key, name).await;
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
                                    tracing::info!("P2P core shutting down...");
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

    async fn send_text(
        &mut self,
        peerid: PeerId,
        msgtype: ChatMessageType,
        data: Vec<u8>,
    ) -> anyhow::Result<()> {
        // 从数据库获取接收方的公钥
        let recipient_public_key = if let Some(pool) = storage::pool() {
            match storage::get_contact_public_key(pool, &peerid.to_string()).await {
                Ok(Some(pubkey)) => pubkey,
                Ok(None) => {
                    return Err(anyhow::anyhow!(
                        "未找到联系人 {} 的公钥，请先添加好友",
                        peerid
                    ));
                }
                Err(e) => {
                    return Err(anyhow::anyhow!("查询联系人公钥失败: {}", e));
                }
            }
        } else {
            return Err(anyhow::anyhow!("数据库连接不可用"));
        };

        // 加密消息数据（使用接收方公钥）
        let encrypted_data = crypto::encrypt_message(
            &data,
            &recipient_public_key,
        )?;

        // 构建签名的消息（包含加密后的数据）
        let message = self.build_signed_message(msgtype, encrypted_data)?;
        self.send_message(peerid, message);
        Ok(())
    }

    async fn generate_identity(&mut self) {
        // 使用统一的 ML-KEM 身份生成逻辑
        let temp_cfg = crate::coreconfig::CoreConfig {
            data_dir: self.data_dir.clone(),
            ..Default::default()
        };

        match crate::identity::generate_mlkem_identity(&temp_cfg).await {
            Ok(public_key ) => {
                let identity_id = hex::encode(&public_key);
                tracing::info!("Generated new ML-KEM identity: {}", identity_id);
                
                let msg = format!("已生成新的 ML-KEM 身份: {}", &identity_id[..16]);
                self.send_log_mpsc(msg).await;
            }
            Err(e) => {
                tracing::error!("Failed to generate ML-KEM identity: {e}");
                let msg = format!("生成 ML-KEM 身份失败: {}", e);
                self.send_warning_mpsc(msg).await;
            }
        }
    }
    
    /// 选择当前 ML-KEM 身份（热切换身份待实现？）
    async fn select_identity(&mut self, identity_id: String) {
        if let Some(pool) = storage::pool() {
            if let Err(e) = storage::set_current_mlkem_identity(pool, &identity_id).await {
                tracing::error!("Failed to select identity: {e}");
            } else {
                tracing::info!("Selected identity: {}", identity_id);
            }
        }
    }

    async fn delete_identity(&mut self, identity_id: String) {
        if let Some(pool) = storage::pool() {
            if let Err(e) = storage::delete_mlkem_identity(pool, &self.data_dir, &identity_id).await {
                tracing::error!("Failed to delete identity: {e}");
            } else {
                tracing::info!("Deleted identity: {}", identity_id);
            }
        }
    }

    async fn add_contact(&mut self, peer_id: String, public_key: Vec<u8>, name: Option<String>) {
        // 注意：这里的 public_key 应该是联系人的 ML-KEM 公钥（用于端到端加密）
        // PeerID 是临时的传输层标识，不需要与 ML-KEM 公钥匹配
        
        // 验证公钥长度是否符合 ML-KEM-768 标准（1184 字节）
        let expected_mlkem_pubkey_size = 1184;
        if public_key.len() != expected_mlkem_pubkey_size {
            tracing::warn!(
                "Invalid ML-KEM public key length: expected {}, got {}",
                expected_mlkem_pubkey_size,
                public_key.len()
            );
            let msg = format!(
                "无效的 ML-KEM 公钥长度: 期望 {} 字节，实际 {} 字节",
                expected_mlkem_pubkey_size,
                public_key.len()
            );
            self.send_warning_mpsc(msg).await;
            return;
        }

        // 保存到数据库（ML-KEM 公钥用于后续加密消息）
        if let Some(pool) = storage::pool() {
            match storage::upsert_contact(
                pool,
                &peer_id,
                name.as_deref(),
                Some(&public_key),
            )
            .await
            {
                Ok(_) => {
                    tracing::info!("Successfully added contact: {}", peer_id);
                    let msg = format!("好友 {} 添加成功", peer_id);
                    self.send_log_mpsc(msg).await;
                }
                Err(e) => {
                    tracing::error!("Failed to save contact: {e}");
                    let msg = format!("保存好友信息失败: {}", e);
                    self.send_warning_mpsc(msg).await;
                }
            }
        } else {
            tracing::error!("Database pool not available");
            self.send_warning_mpsc("数据库不可用".to_string()).await;
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
