//! Actor 模块：提供 P2pActor 后台任务和全局运行时。
//!
//! P2pActor 拥有 libp2p Swarm 的所有权，在网络事件循环中处理 P2P 消息。
//! ChatCore 通过 `P2pActorHandle` 向 P2pActor 发送命令。

pub mod p2p;

use std::sync::LazyLock;

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

/// 全局 Tokio 运行时句柄
pub static RUNTIME_HANDLE: LazyLock<tokio::runtime::Handle> =
    LazyLock::new(|| RUNTIME.handle().clone());