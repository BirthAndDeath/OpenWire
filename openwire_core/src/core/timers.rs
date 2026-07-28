//! 独立定时器任务：将周期任务从 ChatCore 主循环中拆分出去。
//!
//! 每个定时器作为一个独立的 `tokio::spawn` 任务运行，
//! 通过 `cmd_tx` 发送 `ChatCommand` 触发 ChatCore 处理。
//! 所有定时器共享 `CancellationToken`，在关闭时统一退出。

use std::time::Duration;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::command::ChatCommand;
use crate::core::DHT_REGISTRATION_INTERVAL_SECS;

/// 启动所有定时器任务
pub fn spawn_all(
    cmd_tx: mpsc::Sender<ChatCommand>,
    shutdown_token: CancellationToken,
) {
    let rt_handle = tokio::runtime::Handle::current();

    // DHT 身份定期注册（5分钟）
    spawn_interval_task(
        &rt_handle,
        "dht_registration",
        Duration::from_secs(DHT_REGISTRATION_INTERVAL_SECS),
        || ChatCommand::TimerPublishIdentity,
        cmd_tx.clone(),
        shutdown_token.clone(),
    );

    // 在线消息重试（10秒间隔，降低 CPU 开销和日志噪声；
    // 对于大多数场景，10s 延迟对用户体验影响可忽略，
    // 且减少重试频率有助于降低 P2pActor 的通道压力）
    spawn_interval_task(
        &rt_handle,
        "online_retry",
        Duration::from_secs(10),
        || ChatCommand::TimerRetryPendingOnline,
        cmd_tx.clone(),
        shutdown_token.clone(),
    );

    // 路由表保存（5分钟）
    spawn_interval_task(
        &rt_handle,
        "routing_table_save",
        Duration::from_secs(300),
        || ChatCommand::TimerSaveRoutingTable,
        cmd_tx.clone(),
        shutdown_token.clone(),
    );

    // DHT 记录清理（1小时）
    spawn_interval_task(
        &rt_handle,
        "dht_cleanup",
        Duration::from_secs(3600),
        || ChatCommand::TimerCleanupDht,
        cmd_tx.clone(),
        shutdown_token.clone(),
    );

    // 已发送文件有效性验证由 handle_file_download_request 按需触发
}

/// 启动一个简单的间隔定时器任务。
///
/// 每次 tick 通过 `mk_cmd` 闭包构造命令并发送到 `cmd_tx`。
/// 首次 tick 跳过，然后每 `interval` 触发一次。
fn spawn_interval_task(
    rt_handle: &tokio::runtime::Handle,
    name: &'static str,
    interval: Duration,
    mk_cmd: fn() -> ChatCommand,
    cmd_tx: mpsc::Sender<ChatCommand>,
    shutdown_token: CancellationToken,
) {
    rt_handle.spawn(async move {
        let mut timer = tokio::time::interval(interval);
        timer.tick().await;

        loop {
            tokio::select! {
                _ = timer.tick() => {
                    if let Err(e) = cmd_tx.try_send(mk_cmd()) {
                        tracing::warn!("Timer {name} failed to send command: {e:?}");
                    }
                }
                _ = shutdown_token.cancelled() => {
                    tracing::debug!("Timer {name} cancelled");
                    return;
                }
            }
        }
    });
}