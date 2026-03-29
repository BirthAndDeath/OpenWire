use std::{num::NonZeroUsize, time::Instant};

use futures::StreamExt;
use keyring::{Entry, Result};
use libp2p::{PeerId, Swarm, mdns};
use tokio::sync::mpsc;
use tokio::try_join;
mod coreconfig;
mod log;
mod p2p;
use log::init_logger;
mod storage;
pub use coreconfig::CoreConfig;
use lru::LruCache;
use serde::{Deserialize, Serialize};
#[derive(Debug, Serialize, Deserialize)]
#[repr(u8)]
pub enum ChatMessageType {
    Text = 0,
    FileHash = 1,
    #[doc(hidden)]
    __NonExhaustive = 255,
}
#[derive(Debug, Serialize, Deserialize)]
///聊天消息类型
pub struct ChatMessage {
    pub msgtype: ChatMessageType,
    /// 消息内容
    pub data: Vec<u8>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChatResponse {
    timestamp: u64,
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
    /// 警告信息
    Warning,
}
/// 通道消息结构：核心向外部（UI）发送的事件包装
pub struct ChatcoreEvent {
    pub event: MessageEvent,
    pub data: String,
}
/// 初始化：首次运行时执行
fn first_run() {}
/// 聊天核心：管理 P2P 网络、命令处理、消息分发
pub struct ChatCore {
    /// libp2p 网络 swarm，管理所有连接和协议
    pub swarm: Swarm<p2p::MyBehaviour>,
    /*/// 当前订阅的话题（聊天室标识）
    pub topic: gossipsub::IdentTopic,*/
    /// 消息发送通道：向外部（UI）发送事件
    pub tx_message: mpsc::Sender<ChatcoreEvent>,
    /// 消息接收通道：外部可取走事件（Option 用于 run() 时 take）
    pub rx_message: Option<mpsc::Receiver<ChatcoreEvent>>,
    /// 命令接收通道：接收外部控制指令
    pub rx_cmd: mpsc::Receiver<ChatCommand>,
    pub mdns_cache: LruCache<PeerId, Instant>,
}

impl ChatCore {
    /// 异步初始化核心
    const MDNS_CACHE_SIZE: usize =2000;//mdns缓存大小
    pub async fn try_init(cfg: CoreConfig) -> anyhow::Result<Self> {
        if let Err(e) = init_logger(&cfg) {
            return Err(anyhow::anyhow!("Failed to init logger:{}", e));
        };
        let swarm = p2p::swarm_init()?;

        /*// 创建并订阅 Gossipsub 话题
        // 注意：需使用相同话题名才能互通
        let topic = gossipsub::IdentTopic::new("test-net");
        swarm.behaviour_mut().gossipsub.subscribe(&topic)?;*/

        // 创建消息通道：容量 32，背压控制防止内存溢出
        let (tx, rx) = mpsc::channel(32);
        let _ = try_join!(storage::init(&cfg))?;
        let mdns_cache = LruCache::new(NonZeroUsize::new(Self::MDNS_CACHE_SIZE).unwrap());

        Ok(ChatCore {
            swarm,
            tx_message: tx,
            rx_message: Some(rx),
            //topic,
            rx_cmd: cfg.rx_cmd,
            mdns_cache,
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
                           p2p::swarm_event(event, &mut self).await;
                        }

                        // 2. 控制命令：外部发送（UI/CLI）
                        Some(cmd) = self.rx_cmd.recv() => {
                            
                            match cmd {
                                ChatCommand::SendMessage { message} => {
                                    self.send_message(ChatMessage{msgtype:ChatMessageType::Text,data: message.data});
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

    /// 发送消息到 Gossipsub 网络
    ///
    /// - 无确认机制：不保证送达，依赖 Gossipsub 的传播
    pub fn send_message(&mut self, data: ChatMessage) -> anyhow::Result<()> {
        let bytes =
            postcard::to_allocvec(&data).map_err(|e| anyhow::anyhow!("Postcard failed: {}", e))?;
        /*if let Err(e) = self
            .swarm
            .behaviour_mut()
            .gossipsub
            .publish(self.topic.clone(), bytes)
        {
            tracing::error!("Publish error: {e:?}");
        }*/
        Ok(())
    }
    pub fn send_to() {}

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
