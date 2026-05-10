use std::error::Error;
use std::sync::LazyLock;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
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
pub struct ActorHandle<C: Send + 'static> {
    tx: mpsc::Sender<C>,
    cancellation_token: CancellationToken,
}
impl<C: Send> ActorHandle<C> {
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

pub enum ActorCommand<C: Send> {
    Custom(C),
}
#[async_trait]
pub trait Actor: Send + 'static {
    type Command: Send;
    type Event: Send;
    async fn handle(&mut self, cmd: ActorCommand<Self::Command>) -> Vec<Self::Event>;
    async fn on_shutdown(&mut self) -> Vec<Self::Event>;
    fn start(
        mut self,
        channel_size: usize,
        cancellation_token: CancellationToken,
    ) -> Result<ActorHandle<ActorCommand<Self::Command>>, Box<dyn Error>>
    where
        Self: Sized,
    {
        let (tx, mut rx) = mpsc::channel(channel_size);
        // 使用全局 Runtime 生成异步任务
        let ct = cancellation_token.clone();
        RUNTIME.spawn(async move {
            loop {
                tokio::select! {
                    cmd_opt = rx.recv() => {
                        if let Some(cmd) = cmd_opt {
                            let events = self.handle(cmd).await;
                        } else {
                            break;
                        }
                    }
                    _ = cancellation_token.cancelled() => {
                        let events = self.on_shutdown().await;
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
