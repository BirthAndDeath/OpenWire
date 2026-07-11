pub mod p2p;

use std::{fmt::Display, sync::LazyLock};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
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
/// 全局 Tokio 运行时句柄，用于在非异步上下文中调度任务
pub static RUNTIME_HANDLE: LazyLock<tokio::runtime::Handle> =
    LazyLock::new(|| RUNTIME.handle().clone());

/// Actor 句柄，用于向 Actor 发送命令和控制生命周期
pub struct ActorHandle<C: Send + 'static> {
    /// 命令发送通道
    pub tx: mpsc::Sender<C>,
    /// 取消令牌，用于通知 Actor 优雅退出
    pub cancellation_token: CancellationToken,

    pub join_handle: JoinHandle<()>,
}

impl<C: Send> ActorHandle<C> {
    /// 触发关闭信号，通知 Actor 优雅退出
    pub fn shutdown(&self) {
        self.cancellation_token.cancel();
    }
    /// 异步关闭，得到结果
    pub async fn join(self) -> Result<(), tokio::task::JoinError> {
        self.join_handle.await
    }
    pub async fn shutdown_and_join(self) {
        self.shutdown();
        self.join().await;
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
//返回通道
pub struct Reply<T: Send>(tokio::sync::oneshot::Sender<T>);
impl<T: Send> Reply<T> {
    pub fn send(self, value: T) -> Result<(), T> {
        self.0.send(value)
    }
}
/// Actor 命令包装，支持自定义命令
pub enum ActorCommand<C: Send, R: Send> {
    /// 用户自定义命令
    Custom(C, Option<Reply<R>>), // 让命令携带回复通道
}
/// 创建一个带有 Reply 包装的 oneshot 通道
///
/// 返回 `(Reply<T>, oneshot::Receiver<T>)`，分别用于发送响应和等待响应。
pub fn reply_channel<T: Send>() -> (Reply<T>, oneshot::Receiver<T>) {
    let (tx, rx) = oneshot::channel();
    (Reply(tx), rx)
}
/// 生成一个自定义命令及其回复通道，返回 `(ActorCommand<C, R>, oneshot::Receiver<R>)`
pub fn command_with_reply<C: Send, R: Send>(
    custom: C,
) -> (ActorCommand<C, R>, oneshot::Receiver<R>) {
    let (reply, recv) = reply_channel();
    (ActorCommand::Custom(custom, Some(reply)), recv)
}
/// Actor 特征：事件循环模式的基础抽象
///
/// 在后台独立运行，通过 ActorHandle 接收命令，处理事件。
#[async_trait]
pub trait Actor: Send + 'static {
    /// 命令类型：用于通知 Actor 执行某些操作 类似函数参数
    type Command: Send;
    /// 响应类型：用于 Actor 处理命令后返回结果 类似返回值
    type Response: Send;
    /// 事件类型：用于 Actor 处理命令后生成的事件，供外部系统订阅和处理
    type Event: Send;

    /// 处理收到的命令，返回生成的零个或多个事件
    async fn handle(
        &mut self,
        cmd: ActorCommand<Self::Command, Self::Response>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>>;

    /// 关闭时调用，用于清理资源
    async fn on_shutdown(&mut self) -> Result<(), Box<dyn std::error::Error + Send + Sync>>;

    /// 启动 Actor
    fn start(
        mut self,
        channel_size: usize,
        cancellation_token: CancellationToken,
    ) -> Result<
        ActorHandle<ActorCommand<Self::Command, Self::Response>>,
        Box<dyn std::error::Error + Send + Sync>,
    >
    where
        Self: Sized,
    {
        let (tx, mut rx) = mpsc::channel(channel_size);

        let ct = cancellation_token.clone();
        let join_handle = RUNTIME.spawn(async move {
            loop {
                tokio::select! {
                    cmd_opt = rx.recv() => {
                        if let Some(cmd) = cmd_opt {
                             catch_trace(self.handle(cmd).await);
                        } else {
                            catch_trace(self.on_shutdown().await);
                            break;
                        }
                    }
                    _ = cancellation_token.cancelled() => {
                         catch_trace(self.on_shutdown().await);
                        break;
                    }
                }
            }
        });
        Ok(ActorHandle {
            tx,
            cancellation_token: ct,
            join_handle,
        })
    }
}

fn catch_trace<T, E: Display>(r: Result<T, E>) {
    if let Err(e) = r {
        tracing::error!("{e}")
    }
}
