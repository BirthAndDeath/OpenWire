use crate::error::CliError;
use crossterm::event::{Event, KeyCode, KeyEventKind};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use futures::StreamExt;
use ratatui::Terminal;
use ratatui::prelude::CrosstermBackend;
use std::io::stdout;
use std::time::{Duration, Instant};
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

    // 粘贴检测：记录上次字符输入时间，快速连续输入时跳过中间渲染
    let mut last_char_time = Instant::now();
    let paste_debounce = Duration::from_millis(50);

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
                // 粘贴优化：如果连续字符输入间隔 < 50ms，跳过渲染，由 tick 定时器负责刷新
                if let Event::Key(key) = &event
                    && let KeyCode::Char(_) = key.code
                    && key.kind == KeyEventKind::Press
                {
                    let now = Instant::now();
                    if now - last_char_time < paste_debounce {
                        should_render = false;
                    } else {
                        should_render = true;
                    }
                    last_char_time = now;
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
