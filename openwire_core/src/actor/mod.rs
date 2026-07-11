pub mod p2p;

use std::error::Error;
use std::sync::LazyLock;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// 全局 Tokio 运行时（多线程，自动检测 CPU 核心数）
pub static RUNTIME: LazyLock<tokio::runtime::Runtime> = LazyLock::new(|| {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(
            std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(1),
        )
        .enable_all()
        .build()
        .expect("Failed to build tokio runtime")
});

/// Actor 句柄，用于向 Actor 发送命令和控制生命周期
pub struct ActorHandle<C: Send + 'static> {
    /// 命令发送通道
    pub tx: mpsc::Sender<C>,
    /// 取消令牌，用于通知 Actor 优雅退出
    pub cancellation_token: CancellationToken,
}

impl<C: Send> ActorHandle<C> {
    /// 触发关闭信号，通知 Actor 优雅退出
    pub fn shutdown(&self) {
        self.cancellation_token.cancel();
    }

    /// 同步发送（会阻塞当前线程直到通道有空位）
    pub fn send_blocking(&self, cmd: C) -> Result<(), mpsc::error::SendError<C>> {
        self.tx.blocking_send(cmd)
    }

    /// 异步发送（需要在异步上下文中使用）
    pub async fn send(&self, cmd: C) -> Result<(), mpsc::error::SendError<C>> {
        self.tx.send(cmd).await
    }
}

use async_trait::async_trait;

/// Actor 命令包装，支持自定义命令
pub enum ActorCommand<C: Send> {
    /// 用户自定义命令
    Custom(C),
}

/// Actor 特征：事件循环模式的基础抽象
///
/// 在后台独立运行，通过 ActorHandle 接收命令，处理事件。
#[async_trait]
pub trait Actor: Send + 'static {
    /// 命令类型
    type Command: Send;
    /// 事件类型
    type Event: Send;

    /// 处理收到的命令，返回生成的零个或多个事件
    async fn handle(&mut self, cmd: ActorCommand<Self::Command>) -> Vec<Self::Event>;

    /// 关闭时调用，用于清理资源
    async fn on_shutdown(&mut self) -> Vec<Self::Event>;
    /// 启动 Actor
    fn start(
        mut self,
        channel_size: usize,
        cancellation_token: CancellationToken,
    ) -> Result<ActorHandle<ActorCommand<Self::Command>>, Box<dyn Error>>
    where
        Self: Sized,
    {
        let (tx, mut rx) = mpsc::channel(channel_size);
        let ct = cancellation_token.clone();
        RUNTIME.spawn(async move {
            loop {
                tokio::select! {
                    cmd_opt = rx.recv() => {
                        if let Some(cmd) = cmd_opt {
                            let _events = self.handle(cmd).await;
                        } else {
                            break;
                        }
                    }
                    _ = cancellation_token.cancelled() => {
                        let _events = self.on_shutdown().await;
                        break;
                    }
                }
            }
        });
        Ok(ActorHandle {
            tx,
            cancellation_token: ct,
        })
    }
}
