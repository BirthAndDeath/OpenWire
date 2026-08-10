use crate::error::CliError;
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
pub async fn tui_run(app: &mut App) -> Result<(), CliError> {
    let mut event_stream = crossterm::event::EventStream::new();
    enable_raw_mode()?;

    let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;
    terminal.clear()?;
    let mut core = app.core.take().ok_or(CliError::CoreNotInitialized)?;
    let mut rx = core
        .take_rx_message()
        .ok_or(CliError::ChannelNotInitialized)?;
    let mut tick = interval(Duration::from_millis(16));
    let core_joinhandle = core.run();

    loop {
        let should_render;

        tokio::select! {
            msg = rx.recv() => {
                if let Some(msg) = msg {
                    app.handle_message_event(msg);
                    should_render = true;
                } else {
                    break;
                }
            }
            Some(Ok(event)) = event_stream.next() => {
                if let Event::Key(key) = event
                    && key.kind == KeyEventKind::Press
                {
                    let is_ctrl = key.modifiers.contains(crossterm::event::KeyModifiers::CONTROL);
                    match key.code {
                        // Ctrl+C 退出
                        KeyCode::Char('c') if is_ctrl => break,
                        // Esc 退出（下载对话框打开时不触发，由对话框内部处理）
                        KeyCode::Esc if app.download_dialog.is_none() => break,
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
                // 字符输入（打字/粘贴）不触发渲染，由 60 FPS tick 定时器负责刷新。
                // 渲染上限锁定在 16ms 间隔，用户感知不到延迟（人眼阈值约 30ms），
                // 但避免了 3904 字符 ML-DSA 公钥粘贴时逐字重绘的性能爆炸。
                if let Event::Key(key) = &event
                    && key.kind == KeyEventKind::Press
                {
                    let is_char_input = matches!(key.code, KeyCode::Char(_) | KeyCode::Backspace);
                    should_render = !is_char_input;
                } else {
                    should_render = true;
                }
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

    // 恢复终端状态（在等待后台线程之前执行，确保即使 join 阻塞也能恢复）
    let _ = disable_raw_mode();
    let _ = terminal.show_cursor();

    // 使用 spawn_blocking 等待后台线程结束，添加超时防止永久阻塞
    let result_thread = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        tokio::task::spawn_blocking(move || core_joinhandle.join()),
    )
    .await;
    match result_thread {
        Ok(Ok(join_result)) => eprintln!("结束后台线程 结果{:?}", join_result),
        Ok(Err(e)) => eprintln!("等待后台线程失败: {:?}", e),
        Err(_) => eprintln!("后台线程关闭超时（强制退出）"),
    }

    let _ = terminal.clear();
    Ok(())
}
