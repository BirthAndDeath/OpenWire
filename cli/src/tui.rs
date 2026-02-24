use chat_core::ChatCommand;
use crossterm::event::{Event, KeyCode, KeyEventKind};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use futures::StreamExt;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::prelude::CrosstermBackend;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Text;
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph, Wrap};
use ratatui::{Frame, Terminal};
use std::io::stdout;
use std::time::Duration;
use tokio::time::interval;

use crate::{App, Focus};
fn getcontacts(contacts: &mut Vec<ListItem>) {
    let list = vec!["a".to_string(), "b".to_string()];
    *contacts = list
        .iter()
        .map(|m| ListItem::new(Text::from(m.clone())))
        .collect::<Vec<ListItem>>()
        .clone();
}
fn tui_render(frame: &mut Frame, app: &App) {
    // 创建布局
    // 水平切分（左右）
    let horizontal_chunks = Layout::default()
        .direction(Direction::Horizontal) // 水平方向：左 | 右
        .constraints([
            Constraint::Percentage(25), // 左侧：联系人/频道列表（25%宽度）
            Constraint::Percentage(75), // 右侧：主区域（75%宽度）
        ])
        .split(frame.area());

    let sidebar_area = horizontal_chunks[0]; // 左侧区域

    // 将右侧区域再垂直切分（上下）
    let right_vertical = Layout::default()
        .direction(Direction::Vertical) // 垂直方向：上 | 下
        .constraints([
            Constraint::Min(5),    // 右上：消息列表（至少5行，占据剩余空间）
            Constraint::Length(3), // 右下：输入框（固定3行）
        ])
        .split(horizontal_chunks[1]);

    let messages_area = right_vertical[0]; // 消息区
    let input_area = right_vertical[1]; // 输入区

    //  渲染消息列表（List 组件）感谢ai帮我写注释（）
    let messages: Vec<ListItem> = app
        .messages
        .iter()
        .map(|m| ListItem::new(Text::from(m.clone())))
        .collect();

    let message_list = List::new(messages)
        .block(
            Block::default()
                .title(" 消息列表 ")
                .borders(Borders::ALL)
                .border_style(match app.current_focus {
                    Focus::Messages => Style::default().fg(Color::Yellow),
                    _ => Style::default(),
                }),
        )
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol(">> ");

    // 渲染带有状态的List
    frame.render_stateful_widget(
        message_list,
        messages_area,
        &mut app.message_list_state.clone(),
    );
    let mut contacts: Vec<ListItem> = vec![];
    getcontacts(&mut contacts);

    let contact_list = List::new(contacts)
        .block(
            Block::default()
                .title(" 联系人列表 ")
                .borders(Borders::ALL)
                .border_style(match app.current_focus {
                    Focus::SidebarArea => Style::default().fg(Color::Yellow),
                    _ => Style::default(),
                }),
        )
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol(">> ");
    frame.render_stateful_widget(
        contact_list,
        sidebar_area,
        &mut app.contact_list_state.clone(),
    );

    // 渲染输入框
    let input = Paragraph::new(app.input.as_str())
        .block(
            Block::default()
                .title(" 输入框 ")
                .borders(Borders::ALL)
                .border_style(match app.current_focus {
                    Focus::Input => Style::default().fg(Color::Yellow),
                    _ => Style::default(),
                }),
        )
        .wrap(Wrap { trim: true }); // 自动换行

    frame.render_widget(input, input_area);

    // 如果焦点在输入框，设置光标位置
    if let Focus::Input = app.current_focus {
        frame.set_cursor_position((input_area.x + app.input.len() as u16 + 1, input_area.y + 1));
    }

    //  渲染状态栏
    let status = match app.current_focus {
        Focus::Messages => format!(" 模式: 浏览消息 (↑/↓选择)"),
        Focus::Input => format!(" 模式: 输入文本 (Enter发送，Tab切换焦点，Esc退出应用)"),
        Focus::SidebarArea => format!(" 模式:选择聊天对象↑/↓选择"),
    };
    let status_bar = Paragraph::new(status).block(Block::default().borders(Borders::TOP));
    frame.render_widget(status_bar, messages_area);
}
fn handle_event(app: &mut App, event: Event) -> std::io::Result<()> {
    match event {
        //状态机
        Event::Key(key) if key.kind == KeyEventKind::Press => match app.current_focus {
            Focus::Messages => handle_messages_focus(app, key.code),
            Focus::Input => handle_input_focus(app, key.code),
            Focus::SidebarArea => handle_sidebar_area_focus(app, key.code),
        },
        _ => {}
    }
    Ok(())
}
fn handle_sidebar_area_focus(_app: &mut App, _key_code: KeyCode) {
    /*let list_len = app.conta.len();
    match key_code {
        KeyCode::Up => {
            if list_len > 0 {
                let i = app.message_list_state.selected().unwrap_or(0);
                app.message_list_state.select(Some(i.saturating_sub(1)));
            }
        }
        KeyCode::Down => {
            if list_len > 0 {
                let i = app.message_list_state.selected().unwrap_or(0);
                app.message_list_state
                    .select(Some((i + 1).min(list_len - 1)));
            }
        }

        KeyCode::Enter => {
            // 回复选中的消息
            if let Some(i) = app.message_list_state.selected() {
                if let Some(msg) = app.messages.get(i) {
                    app.input = format!("回复「{}」: ", msg);
                    app.current_focus = Focus::Input;
                }
            }
        }
        _ => {}*/
}
fn handle_messages_focus(app: &mut App, key_code: KeyCode) {
    let list_len = app.messages.len();
    match key_code {
        KeyCode::Up => {
            if list_len > 0 {
                let i = app.message_list_state.selected().unwrap_or(0);
                app.message_list_state.select(Some(i.saturating_sub(1)));
            }
        }
        KeyCode::Down => {
            if list_len > 0 {
                let i = app.message_list_state.selected().unwrap_or(0);
                app.message_list_state
                    .select(Some((i + 1).min(list_len - 1)));
            }
        }

        KeyCode::Enter => {
            // 回复选中的消息
            if let Some(i) = app.message_list_state.selected() {
                if let Some(msg) = app.messages.get(i) {
                    app.input = format!("回复「{}」: ", msg);
                    app.current_focus = Focus::Input;
                }
            }
        }
        _ => {}
    }
}

fn handle_input_focus(app: &mut App, key_code: KeyCode) {
    match key_code {
        KeyCode::Enter => {
            // 发送消息
            if !app.input.trim().is_empty() {
                app.messages.push(app.input.clone());
                app.app_data.cmd_tx.try_send(ChatCommand::SendMessage {
                    message: app.input.clone(),
                });
                //send
                app.input.clear();
                // 自动滚动到最新消息
                app.message_list_state.select(Some(app.messages.len() - 1));
            }
        }

        KeyCode::Char(c) => app.input.push(c),
        KeyCode::Backspace => {
            app.input.pop();
        }

        _ => {}
    }
}
impl Focus {
    fn next_focus(self) -> Self {
        match self {
            Focus::Input => Focus::Messages,
            Focus::Messages => Focus::SidebarArea,
            Focus::SidebarArea => Focus::Input, //懒得写复杂轮换了……就这样凑合着吧或者谁来帮我写一下方向轮换
        }
    }
}

///tui模式
pub async fn tui_run(app: &mut App) -> anyhow::Result<()> {
    let mut event_stream = crossterm::event::EventStream::new();
    enable_raw_mode()?;

    // 初始化终端
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;
    terminal.clear()?;
    let mut core = app.core.take().unwrap();
    let mut rx = core
        .rx_message
        .take()
        .ok_or("消息通道问题")
        .expect("消息通道问题");
    let mut tick = interval(Duration::from_millis(16));
    let core_joinhandle = core.run();

    loop {
        tokio::select! {
            Some(msg)=rx.recv()=>{
                let text = msg.data.as_str();
                 app.messages.push(format!("\n[网络]  {}",
                 text));
                 // 自动滚动到最新消息
                app.message_list_state.select(Some(app.messages.len() - 1));
                 terminal.draw(|frame| tui_render(frame, &app))?;

            }
            Some(Ok(event)) = event_stream.next() => {
                if let Event::Key(key)=event
                &&key.kind == KeyEventKind::Press{
                match  key.code {
                    KeyCode::Esc=> break,
                    KeyCode::Tab =>app.current_focus= app.current_focus.next_focus(),
                    _=>{

                    },
                }}
                handle_event(app, event)?;
            }



            _ = tick.tick() => {terminal.draw(|frame| tui_render(frame, &app))?;}
            else =>{
                break;
            }

        }
        if app.should_quit {
            break;
        }
        terminal.draw(|frame| tui_render(frame, &app))?;
    }
    let result_cmd = app.app_data.cmd_tx.try_send(ChatCommand::Shutdown);
    print!("结束通道命令结果{:?}", result_cmd);
    let result_thread = core_joinhandle.join();
    print!("结束后台线程 结果{:?}", result_thread);
    disable_raw_mode()?;
    terminal.clear()?;
    terminal.show_cursor()?;
    // 恢复终端
    Ok(())
}
