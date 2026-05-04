use anyhow::Context;
use chat_core::{IncomingMessage, MessageEvent};
use crossterm::event::{Event, KeyCode, KeyEventKind};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use futures::StreamExt;
use ratatui::Terminal;
use ratatui::prelude::CrosstermBackend;
use std::io::stdout;
use std::time::Duration;
use tokio::time::interval;

use crate::{App, Focus};

pub mod event;
pub mod render;

/// TUI 模式
pub async fn tui_run(app: &mut App) -> anyhow::Result<()> {
    let mut event_stream = crossterm::event::EventStream::new();
    enable_raw_mode()?;

    let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;
    terminal.clear()?;
    let mut core = app.core.take().context("Core not initialized")?;
    let mut rx = core.take_rx_message().context("消息通道未初始化")?;
    let mut tick = interval(Duration::from_millis(16));
    let core_joinhandle = core.run();

    loop {
        let should_render;

        tokio::select! {
            msg = rx.recv() => {
                if let Some(msg) = msg {
                    match msg {
                        MessageEvent::ReceiveMessage(msg) => {
                            match msg {
                                IncomingMessage::Text { text, sender } => {
                                    app.push_message_to(&sender, format!("[对方] {}", text));
                                }
                                IncomingMessage::FileShare { filename, file_id, total_size, sender, .. } => {
                                    app.push_message_to(&sender, format!("[文件] {} ({} bytes, id={})", filename, total_size, &file_id[..16]));
                                }
                                IncomingMessage::DeliveryReceipt { peer_id, .. } => {
                                    app.push_message_to(&peer_id, "[系统] 消息已送达 ✓".to_string());
                                }
                                IncomingMessage::OnlineStatus { count } => {
                                    app.online_peers = count;
                                }
                            }
                            should_render = true;
                            continue;
                        }
                        MessageEvent::FileTransferProgress(progress) => {
                            app.push_message(format!(
                                "[文件传输] {} ({}/{}) - {}",
                                progress.filename,
                                progress.received_bytes,
                                progress.total_size,
                                progress.status,
                            ));
                        }
                        MessageEvent::Warning(data) => {
                            app.push_message(format!("[警告] {}", data));
                        }
                        MessageEvent::Log(data) => {
                            app.push_message(format!("[日志] {}", data));
                        }
                        MessageEvent::Error(data) => {
                            app.push_message(format!("[错误] {}", data));
                        }
                    }
                    // 更新选中状态到最新消息
                    let msg_count = app.current_messages().len();
                    if msg_count > 0 {
                        app.message_list_state.select(Some(msg_count - 1));
                    }
                    should_render = true;
                } else {
                    break;
                }
            }
            Some(Ok(event)) = event_stream.next() => {
                if let Event::Key(key) = event
                    && key.kind == KeyEventKind::Press
                {
                    match key.code {
                        KeyCode::Esc => break,
                        KeyCode::Tab => app.current_focus = app.current_focus.next_focus(),
                        KeyCode::F(2) => {
                            app.current_focus = Focus::IdentityArea;
                            app.refresh_identities().await;
                        }
                        _ => {
                            event::handle_event(app, Event::Key(key)).await?;
                        }
                    }
                }
                should_render = true;
            }
            _ = tick.tick() => {
                should_render = true;
            }
        }
        if app.should_quit {
            break;
        }
        if should_render {
            terminal.draw(|frame| render::tui_render(frame, &mut *app))?;
        }
    }

    app.core_handle.shutdown();

    // 使用 spawn_blocking 等待后台线程结束，避免阻塞 tokio 线程
    let result_thread = tokio::task::spawn_blocking(move || core_joinhandle.join()).await;
    match result_thread {
        Ok(join_result) => eprintln!("结束后台线程 结果{:?}", join_result),
        Err(e) => eprintln!("等待后台线程失败: {:?}", e),
    }

    disable_raw_mode()?;
    terminal.clear()?;
    terminal.show_cursor()?;
    Ok(())
}
