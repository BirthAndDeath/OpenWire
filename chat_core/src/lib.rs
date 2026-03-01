use futures::StreamExt;
use libp2p::kad::{self, Mode, store::MemoryStore};
use libp2p::kad::{Config as KadConfig, QueryResult};
use libp2p::{PeerId, StreamProtocol};
use libp2p::{
    Swarm, dcutr, gossipsub, identify, mdns, noise, ping, relay,
    swarm::{NetworkBehaviour, SwarmEvent},
    tcp, yamux,
};
use serde::{Deserialize, Serialize};
use std::num::NonZero;
use std::{
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
    time::Duration,
};
use tokio::sync::mpsc;

use tokio::try_join;
mod coreconfig;
mod log;
use log::init_logger;
mod storage;
pub use coreconfig::CoreConfig;

use rootcell::{
    AuthAssertion, HardwareSecurityModule, RootOfTrust, SessionManager, SoftwareTokenHsm,
    TrustError,
};

/// ==================== 新增：网络消息类型 ====================
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum NetworkMessage {
    /// 正常加密消息
    Encrypted(rootcell::EncryptedMessage),

    /// 密钥交换消息（首次建立会话）
    KeyExchange {
        /// 发送者的 WebAuthn 凭证 ID
        credential_id: Vec<u8>,
        /// 发送者的 X25519 公钥
        public_key: Vec<u8>,
    },

    /// 密钥交换确认
    KeyExchangeAck {
        /// 对方的凭证 ID
        credential_id: Vec<u8>,
    },

    /// 心跳/保活
    Ping,

    /// 心跳响应
    Pong,
}

/// ==================== 新增：Peer 信息缓存 ====================
struct PeerInfo {
    /// Peer 的 libp2p PeerId
    peer_id: PeerId,
    /// Peer 的 WebAuthn 凭证 ID（如果有）
    credential_id: Option<Vec<u8>>,
    /// Peer 的 X25519 公钥（如果有）
    public_key: Option<Vec<u8>>,
    /// 最后活动时间
    last_seen: std::time::Instant,
    /// 是否已建立会话
    session_established: bool,
}

impl PeerInfo {
    fn new(peer_id: PeerId) -> Self {
        Self {
            peer_id,
            credential_id: None,
            public_key: None,
            last_seen: std::time::Instant::now(),
            session_established: false,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
///聊天消息类型
pub struct ChatMessage {
    /// 消息接收者
    pub receiver: Vec<u8>,

    /// 消息内容
    pub data: String,
}

/// 控制命令：外部向核心发送的指令
#[derive(Debug)]
pub enum ChatCommand {
    /// 发送消息到网络
    SendMessage { message: ChatMessage },
    /// 优雅关闭核心
    Shutdown,
}
/// 消息事件类型：用于向外部（UI）通知状态
pub enum MessageEvent {
    /// 收到新消息
    NewMessage,
    /// 发生错误
    Error,
    /// 日志信息（连接状态等）
    Log,
}
/// 通道消息结构：核心向外部（UI）发送的事件包装
pub struct ChatcoreEvent {
    pub event: MessageEvent,
    pub data: String,
}

/// libp2p 网络行为组合：Gossipsub（消息广播）+ mDNS（局域网发现）
///
/// 设计选择：
/// - Gossipsub: 去中心化消息传播，适合群聊/广播场景
/// - mDNS: 零配置局域网发现，无需中心服务器
#[derive(NetworkBehaviour)]
pub struct MyBehaviour {
    /// Gossipsub 协议：发布/订阅消息广播
    gossipsub: gossipsub::Behaviour,
    /// mDNS 协议：局域网内自动发现对等节点
    mdns: mdns::tokio::Behaviour,
    /// Kademlia 协议：分布式哈希表，用于节点定位和路由
    kademlia: kad::Behaviour<MemoryStore>,
    //Ping 协议（连接保活/延迟检测）
    ping: ping::Behaviour,
    // Identify 协议（地址/协议交换）
    identify: identify::Behaviour,

    // Relay 协议（NAT 穿透，可选）
    relay: relay::Behaviour,

    // DCUtR 协议（直连升级，配合 Relay）
    dcutr: dcutr::Behaviour,
}

/// 聊天核心：管理 P2P 网络、命令处理、消息分发
pub struct ChatCore {
    /// libp2p 网络 swarm，管理所有连接和协议
    pub swarm: Swarm<MyBehaviour>,
    /// 当前订阅的话题（聊天室标识）
    pub topic: gossipsub::IdentTopic,
    /// 消息发送通道：向外部（UI）发送事件
    pub tx_message: mpsc::Sender<ChatcoreEvent>,
    /// 消息接收通道：外部可取走事件（Option 用于 run() 时 take）
    pub rx_message: Option<mpsc::Receiver<ChatcoreEvent>>,
    /// 命令接收通道：接收外部控制指令
    pub rx_cmd: mpsc::Receiver<ChatCommand>,

    /// 多会话管理（每个 peer 独立 SecurityCore）
    pub sessions_manager: SessionManager<SoftwareTokenHsm>,

    /// ==================== 新增：Peer 信息缓存 ====================
    peer_cache: std::collections::HashMap<PeerId, PeerInfo>,

    /// ==================== 新增：等待建立会话的消息队列 ====================
    pending_messages: std::collections::HashMap<Vec<u8>, Vec<rootcell::EncryptedMessage>>,
}

impl ChatCore {
    pub async fn add_friend(&mut self, peer_id: PeerId) -> Result<(), TrustError> {
        // 可选：确保 peer 已被添加到 Gossipsub 的显式节点列表（增加消息可达性）
        self.swarm
            .behaviour_mut()
            .gossipsub
            .add_explicit_peer(&peer_id);

        // 发送密钥交换消息
        self.send_key_exchange(&peer_id).await
    }
    /// 异步初始化核心
    ///
    /// # 流程
    /// 1. 初始化 libp2p swarm（网络层）
    /// 2. 订阅默认话题 "test-net"
    /// 3. 初始化日志系统和存储层（并发执行）
    /// 4. 创建消息通道
    pub async fn try_init(cfg: CoreConfig) -> anyhow::Result<Self> {
        let mut swarm = swarm_init()?;

        // 创建并订阅 Gossipsub 话题
        // 注意：所有节点需使用相同话题名才能互通
        let topic = gossipsub::IdentTopic::new("test-net");
        swarm.behaviour_mut().gossipsub.subscribe(&topic)?;

        // 创建消息通道：容量 32，背压控制防止内存溢出
        let (tx, rx) = mpsc::channel(32);

        // 并发初始化日志和存储，任一失败则整体失败
        let (_, _) = try_join!(init_logger(&cfg), storage::init(&cfg))?;
        let hsm = SoftwareTokenHsm::new().unwrap();
        let root = RootOfTrust::with_hsm(hsm).await.unwrap();
        let manager = SessionManager::new(root);

        Ok(ChatCore {
            swarm,
            tx_message: tx,
            rx_message: Some(rx),
            topic,
            rx_cmd: cfg.rx_cmd,

            sessions_manager: manager,
            peer_cache: std::collections::HashMap::new(),
            pending_messages: std::collections::HashMap::new(),
        })
    }

    /// 启动核心事件循环（阻塞，在新线程运行）
    ///
    /// # 设计决策
    /// - 使用独立线程 + tokio current_thread runtime：避免与 UI 线程冲突
    /// - 单线程足够：libp2p 使用异步，无需多线程竞争
    ///
    /// # 返回
    /// JoinHandle：可用于等待线程结束或强制终止
    pub fn run(mut self) -> std::thread::JoinHandle<()> {
        std::thread::spawn(move || {
            // 创建 tokio runtime：current_thread 模式足够，无需多线程调度
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("Failed to build tokio runtime");

            rt.block_on(async move {
                // 主事件循环：三路 select
                loop {
                    tokio::select! {
                        // 1. 网络事件：swarm 产生（新连接、消息到达等）
                        event = self.swarm.select_next_some() => {
                           swarm_event(event, &mut self).await;
                        }

                        // 2. 控制命令：外部发送（UI/CLI）
                        Some(cmd) = self.rx_cmd.recv() => {
                            match cmd {
                                ChatCommand::SendMessage { message} => {
                                    self.send_message(message.data,&message.receiver);
                                }
                                ChatCommand::Shutdown => {
                                    tracing::info!("P2P thread shutting down...");
                                    break;
                                }
                            }
                        }

                        // 3. 心跳/超时：防止 select 永久阻塞，可用于定期任务
                        // 当前 100ms 空转，后续可改为定时清理、状态同步等
                        _ = tokio::time::sleep(Duration::from_millis(100)) => {}
                    }
                }
            });
        })
    }

    /// 发送消息到 Gossipsub 网络
    ///
    /// - 无确认机制：不保证送达，依赖 Gossipsub 的传播
    pub fn send_message(&mut self, data: impl Serialize, peer_credential: &[u8]) {
        let plaintext = rootcell::serialize(&data);
        let result = self
            .sessions_manager
            .encrypt_to(peer_credential, &plaintext);

        if let Ok(encrypted) = result {
            let msg = NetworkMessage::Encrypted(encrypted);
            let bytes = rootcell::serialize(&msg);
            if let Err(e) = self
                .swarm
                .behaviour_mut()
                .gossipsub
                .publish(self.topic.clone(), bytes)
            {
                tracing::error!("Publish error: {e:?}");
            }
        } else if let Err(e) = result {
            tracing::error!("Encrypt error: {e:?}");
        }
    }

    /// ==================== 新增：发送密钥交换消息 ====================
    async fn send_key_exchange(&mut self, peer_id: &PeerId) -> Result<(), TrustError> {
        let my_credential = self.sessions_manager.root.credential_id().to_vec();
        let my_public = self.sessions_manager.root.public_key().as_bytes().to_vec();

        let msg = NetworkMessage::KeyExchange {
            credential_id: my_credential,
            public_key: my_public,
        };

        let bytes = rootcell::serialize(&msg);

        // 直接发送给特定 peer（用 Gossipsub 或直接发送？）
        // 这里先用 Gossipsub 广播，生产环境可考虑用直接流
        if let Err(e) = self
            .swarm
            .behaviour_mut()
            .gossipsub
            .publish(self.topic.clone(), bytes)
        {
            tracing::error!("Failed to send key exchange: {}", e);
            return Err(TrustError::WebAuthn(e.to_string()));
        }

        Ok(())
    }

    /// ==================== 新增：处理密钥交换消息 ====================
    async fn handle_key_exchange(
        &mut self,
        peer_id: PeerId,
        credential_id: Vec<u8>,
        public_key: Vec<u8>,
    ) -> Result<(), TrustError> {
        // 1. 更新缓存
        let info = self
            .peer_cache
            .entry(peer_id)
            .or_insert_with(|| PeerInfo::new(peer_id));
        info.credential_id = Some(credential_id.clone());
        info.public_key = Some(public_key.clone());
        info.last_seen = std::time::Instant::now();

        // 2. 转换公钥
        let peer_public = x25519_dalek::PublicKey::from(
            <[u8; 32]>::try_from(public_key.as_slice())
                .map_err(|_| TrustError::KeyAgreement("Invalid public key length".into()))?,
        );
        // 3. 建立会话
        self.sessions_manager
            .establish_session(&credential_id, &peer_public)
            .await?;

        info.session_established = true;

        // 4. 发送确认
        let ack = NetworkMessage::KeyExchangeAck {
            credential_id: self.sessions_manager.root.credential_id().to_vec(),
        };
        let bytes = rootcell::serialize(&ack);
        let _ = self
            .swarm
            .behaviour_mut()
            .gossipsub
            .publish(self.topic.clone(), bytes);

        // 5. 处理等待中的消息
        if let Some(pending) = self.pending_messages.remove(&credential_id) {
            for encrypted in pending {
                if let Ok(plaintext) = self
                    .sessions_manager
                    .decrypt_from(&credential_id, &encrypted)
                {
                    let text = String::from_utf8_lossy(&plaintext);
                    self.send_message_mpsc(format!(
                        "[Delayed] From: {} | Content: '{}'",
                        hex::encode(&credential_id),
                        text
                    ))
                    .await;
                }
            }
        }

        self.send_log_mpsc(format!(
            "Session established with {}",
            hex::encode(&credential_id)
        ))
        .await;

        Ok(())
    }

    /// ==================== 处理加密消息（带自动会话建立） ====================
    async fn handle_encrypted_message(
        &mut self,
        peer_id: PeerId,
        mut encrypted: rootcell::EncryptedMessage,
    ) {
        let mut retry_count = 0;
        const MAX_RETRIES: u32 = 3;

        loop {
            let sender_credential = encrypted.sender_credential.clone();

            match self
                .sessions_manager
                .decrypt_from(&sender_credential, &encrypted)
            {
                Ok(plaintext) => {
                    // 成功解密
                    let text = String::from_utf8_lossy(&plaintext);
                    self.send_message_mpsc(format!(
                        "From: {} | Content: '{}'",
                        hex::encode(&sender_credential),
                        text
                    ))
                    .await;

                    if let Some(info) = self.peer_cache.get_mut(&peer_id) {
                        info.credential_id = Some(sender_credential);
                        info.last_seen = std::time::Instant::now();
                        info.session_established = true;
                    }
                    break; // 成功退出循环
                }
                Err(TrustError::NoSession) if retry_count < MAX_RETRIES => {
                    retry_count += 1;

                    // 处理会话建立逻辑
                    if retry_count == 1 {
                        // 第一次重试时尝试建立会话
                        self.pending_messages
                            .entry(sender_credential.clone())
                            .or_insert_with(Vec::new)
                            .push(encrypted.clone());

                        if let Some(info) = self.peer_cache.get(&peer_id) {
                            if let (Some(credential), Some(pub_key)) =
                                (&info.credential_id, &info.public_key)
                            {
                                if credential == &sender_credential {
                                    if let Ok(peer_public) =
                                        <[u8; 32]>::try_from(pub_key.as_slice())
                                            .map_err(|_| {
                                                TrustError::KeyAgreement(
                                                    "Invalid public key length".into(),
                                                )
                                            })
                                            .map(x25519_dalek::PublicKey::from)
                                    {
                                        if let Ok(()) = self
                                            .sessions_manager
                                            .establish_session(credential, &peer_public)
                                            .await
                                        {
                                            continue; // 继续循环，重新尝试解密
                                        }
                                    }
                                }
                            } else {
                                let _ = self.send_key_exchange(&peer_id).await;
                            }
                        } else {
                            self.peer_cache.insert(peer_id, PeerInfo::new(peer_id));
                            let _ = self.send_key_exchange(&peer_id).await;
                        }
                    }

                    // 等待一小段时间再重试
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                Err(e) => {
                    tracing::error!("Decrypt error from {}: {:?}", peer_id, e);
                    break;
                }
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

    /// 发送新消息事件到外部通道
    async fn send_message_mpsc(&mut self, data: String) {
        let message = ChatcoreEvent {
            event: MessageEvent::NewMessage,
            data,
        };
        if let Err(e) = self.tx_message.send(message).await {
            tracing::error!("Failed to send message event: {e}");
        }
    }
}

/// 初始化 libp2p Swarm
///
/// # 网络栈配置
/// - 传输：TCP + Noise 加密 + Yamux 多路复用
/// - 补充：QUIC（原生 TLS 1.3，性能更优）
/// - 发现：mDNS 局域网自动发现
/// - 消息：Gossipsub 广播
fn swarm_init() -> anyhow::Result<Swarm<MyBehaviour>> {
    let mut swarm = libp2p::SwarmBuilder::with_new_identity()
        .with_tokio()
        // TCP 传输层：Noise 加密 + Yamux 流多路复用
        .with_tcp(
            tcp::Config::default(),
            noise::Config::new,     // XX 握手模式
            yamux::Config::default, // 可靠流复用
        )?
        // QUIC 传输层：内置 TLS 1.3，0-RTT，更好 NAT 穿透
        .with_quic()
        .with_behaviour(|key| {
            let peer_id = key.public().to_peer_id();
            // --- Gossipsub 配置 ---

            // 消息 ID 生成：基于内容哈希，相同内容不重复传播
            let message_id_fn = |message: &gossipsub::Message| {
                let mut s = DefaultHasher::new();
                message.data.hash(&mut s);
                gossipsub::MessageId::from(s.finish().to_string())
            };

            let gossipsub_config = gossipsub::ConfigBuilder::default()
                // 心跳间隔：10 秒，调试友好（生产可缩短至 1 秒）
                .heartbeat_interval(Duration::from_secs(10))
                // 严格验证：强制消息签名，防伪造
                .validation_mode(gossipsub::ValidationMode::Strict)
                // 内容寻址：相同数据不产生重复传播
                .message_id_fn(message_id_fn)
                .build()
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

            // 创建 Gossipsub：使用 Ed25519 签名消息
            let gossipsub = gossipsub::Behaviour::new(
                gossipsub::MessageAuthenticity::Signed(key.clone()),
                gossipsub_config,
            )?;

            // --- mDNS 配置 ---
            let mdns =
                mdns::tokio::Behaviour::new(mdns::Config::default(), key.public().to_peer_id())?;

            // 创建 Kademlia 行为实例
            let kademlia = create_kademlia(peer_id);
            let identify_config =
                identify::Config::new("/rootcell/identify/1.0.0".to_string(), key.public())
                    .with_agent_version(format!("rootcell/{}", env!("CARGO_PKG_VERSION")))
                    .with_cache_size(100);
            let identify = identify::Behaviour::new(identify_config);
            let ping = ping::Behaviour::new(ping::Config::new());
            let relay = relay::Behaviour::new(key.public().to_peer_id(), Default::default());
            let dcutr = dcutr::Behaviour::new(key.public().to_peer_id());

            Ok(MyBehaviour {
                gossipsub,
                mdns,
                kademlia,
                ping,
                identify,
                relay,
                dcutr,
            })
        })?
        .build();

    // 监听所有接口：IPv4/IPv6 + TCP/QUIC
    // 端口 0 表示系统自动分配
    swarm.listen_on("/ip4/0.0.0.0/udp/0/quic-v1".parse()?)?;
    swarm.listen_on("/ip4/0.0.0.0/tcp/0".parse()?)?;
    swarm.listen_on("/ip6/::/udp/0/quic-v1".parse()?)?;
    swarm.listen_on("/ip6/::/tcp/0".parse()?)?;

    Ok(swarm)
}

/// 创建 Kademlia 行为
fn create_kademlia(peer_id: PeerId) -> kad::Behaviour<MemoryStore> {
    // 存储：内存型（重启丢失），生产可用 PersistentStore
    let store = MemoryStore::new(peer_id);

    // 配置Kademlia网络参数
    let mut config = KadConfig::new(StreamProtocol::new("/rootcell/kad/0.0.1"));
    let _ = &mut config
        .set_query_timeout(Duration::from_secs(60))
        .set_replication_factor(NonZero::new(20).unwrap())
        .set_parallelism(NonZero::new(3).unwrap())
        .set_periodic_bootstrap_interval(Some(Duration::from_secs(300)))
        .set_provider_record_ttl(Some(Duration::from_secs(24 * 60 * 60)))
        .set_publication_interval(Some(Duration::from_secs(60 * 60)));

    let mut kademlia = kad::Behaviour::with_config(peer_id, store, config);
    kademlia.set_mode(Some(Mode::Server));
    kademlia
}

/// 处理 Swarm 网络事件
///
/// # 事件分类处理
/// - mDNS：局域网节点发现/过期
/// - Gossipsub：消息接收
/// - 连接管理：建立、关闭、错误
pub async fn swarm_event(event: SwarmEvent<MyBehaviourEvent>, core: &mut ChatCore) {
    match event {
        // --- mDNS 发现 ---
        SwarmEvent::Behaviour(MyBehaviourEvent::Mdns(mdns::Event::Discovered(list))) => {
            for (peer_id, multiaddr) in list {
                if &peer_id == core.swarm.local_peer_id() {
                    continue; // 过滤自己
                }
                core.send_log_mpsc(format!("mDNS discovered: {peer_id}"))
                    .await;

                // 添加到 Gossipsub
                core.swarm
                    .behaviour_mut()
                    .gossipsub
                    .add_explicit_peer(&peer_id);
                // 添加到 Kademlia
                core.swarm
                    .behaviour_mut()
                    .kademlia
                    .add_address(&peer_id, multiaddr);
            }
        }

        // mDNS 节点过期
        SwarmEvent::Behaviour(MyBehaviourEvent::Mdns(mdns::Event::Expired(list))) => {
            for (peer_id, _multiaddr) in list {
                core.send_log_mpsc(format!("mDNS expired: {peer_id}")).await;
                core.swarm
                    .behaviour_mut()
                    .gossipsub
                    .remove_explicit_peer(&peer_id);
            }
        }

        // --- Gossipsub 消息 ---
        SwarmEvent::Behaviour(MyBehaviourEvent::Gossipsub(gossipsub::Event::Message {
            propagation_source: peer_id,
            message_id: id,
            message,
        })) => {
            // ==================== 修改：使用 NetworkMessage 枚举 ====================
            match rootcell::deserialize::<NetworkMessage>(&message.data) {
                Ok(NetworkMessage::Encrypted(encrypted)) => {
                    core.handle_encrypted_message(peer_id, encrypted).await;
                }

                Ok(NetworkMessage::KeyExchange {
                    credential_id,
                    public_key,
                }) => {
                    core.send_log_mpsc(format!(
                        "Key exchange from {}",
                        hex::encode(&credential_id)
                    ))
                    .await;
                    let _ = core
                        .handle_key_exchange(peer_id, credential_id, public_key)
                        .await;
                }

                Ok(NetworkMessage::KeyExchangeAck { credential_id }) => {
                    core.send_log_mpsc(format!(
                        "Key exchange ack from {}",
                        hex::encode(&credential_id)
                    ))
                    .await;
                    // 更新状态
                    if let Some(info) = core.peer_cache.get_mut(&peer_id) {
                        info.credential_id = Some(credential_id);
                        info.session_established = true;
                    }
                }

                Ok(NetworkMessage::Ping) => {
                    // 回复 Pong
                    let pong = NetworkMessage::Pong;
                    let bytes = rootcell::serialize(&pong);
                    let _ = core
                        .swarm
                        .behaviour_mut()
                        .gossipsub
                        .publish(core.topic.clone(), bytes);
                }

                Ok(NetworkMessage::Pong) => {
                    // 收到 Pong，更新最后活动时间
                    if let Some(info) = core.peer_cache.get_mut(&peer_id) {
                        info.last_seen = std::time::Instant::now();
                    }
                }

                Err(e) => {
                    tracing::error!("Deserialize error: {:?}", e);

                    if let Ok(encrypted) =
                        rootcell::deserialize::<rootcell::EncryptedMessage>(&message.data)
                    {
                        core.handle_encrypted_message(peer_id, encrypted).await;
                    } else {
                        let text = String::from_utf8_lossy(&message.data);
                        core.send_message_mpsc(format!(
                            "[UNKNOWN] From: {} | Content: '{}'",
                            peer_id, text
                        ))
                        .await;
                    }
                }
            }
        }

        // --- Identify 事件（获取 peer 信息）---
        SwarmEvent::Behaviour(MyBehaviourEvent::Identify(identify::Event::Received {
            peer_id,
            info,
            connection_id,
        })) => {
            core.send_log_mpsc(format!(
                "Identified {} with {} protocols",
                peer_id,
                info.protocols.len()
            ))
            .await;

            // 可以在这里更新 peer 信息
            core.peer_cache
                .entry(peer_id)
                .or_insert_with(|| PeerInfo::new(peer_id));
        }

        // --- 网络状态 ---
        SwarmEvent::NewListenAddr { address, .. } => {
            core.send_log_mpsc(format!("Listening on: {address}")).await;
        }

        // 连接建立
        SwarmEvent::ConnectionEstablished { peer_id, .. } => {
            core.send_log_mpsc(format!("Connection established with {}", peer_id))
                .await;
        }

        // 连接关闭
        SwarmEvent::ConnectionClosed { peer_id, .. } => {
            core.send_log_mpsc(format!("Connection closed with {}", peer_id))
                .await;
            // 可以保留缓存，但标记为离线
            if let Some(info) = core.peer_cache.get_mut(&peer_id) {
                info.session_established = false;
            }
        }

        // 外拨连接失败
        SwarmEvent::OutgoingConnectionError { peer_id, error, .. } => {
            if let Some(pid) = peer_id {
                core.send_log_mpsc(format!("Connect failed to {pid}: {error:?}"))
                    .await;
            }
        }

        // 入站连接错误
        SwarmEvent::IncomingConnectionError {
            local_addr, error, ..
        } => {
            core.send_log_mpsc(format!("Incoming error on {local_addr:?}: {error:?}"))
                .await;
        }

        // 拨号中
        SwarmEvent::Dialing { peer_id, .. } => {
            tracing::debug!("Dialing: {peer_id:?}");
        }

        // 监听器关闭
        SwarmEvent::ListenerClosed {
            addresses, reason, ..
        } => {
            tracing::warn!("Listener closed: {addresses:?}, reason: {reason:?}");
        }

        // 监听器错误
        SwarmEvent::ListenerError { error, .. } => {
            tracing::error!("Listener error: {error}");
        }

        // 发现对等节点新外部地址
        SwarmEvent::NewExternalAddrOfPeer { peer_id, address } => {
            core.send_log_mpsc(format!("Peer {peer_id} new addr: {address}"))
                .await;
        }

        // 监听地址过期
        SwarmEvent::ExpiredListenAddr { address, .. } => {
            core.send_log_mpsc(format!("Address expired: {address}"))
                .await;
        }

        // 其他事件
        _ => {}
    }
}
