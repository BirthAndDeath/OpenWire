use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Text;
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap};

use crate::{App, Focus};

fn getcontacts(app: &App, contacts_list: &mut Vec<ListItem>) {
    *contacts_list = app
        .contacts
        .iter()
        .map(|c| {
            let name = c.name.as_deref().unwrap_or("(未命名)");
            let short_pk = if c.mldsa_pubkey_hex.len() > 16 {
                format!("{}...", &c.mldsa_pubkey_hex[..16])
            } else {
                c.mldsa_pubkey_hex.clone()
            };
            ListItem::new(Text::from(format!("{} ({})", name, short_pk)))
        })
        .collect::<Vec<ListItem>>();
}

fn get_identity_items(app: &App) -> Vec<ListItem<'static>> {
    app.identities
        .iter()
        .map(|id| {
            let short_id = if id.identity_id.len() > 16 {
                format!("{}...", &id.identity_id[..16])
            } else {
                id.identity_id.clone()
            };
            let current_mark = if id.is_current == 1 { " ✓" } else { "" };
            ListItem::new(Text::from(format!("{}{}", short_id, current_mark)))
        })
        .collect::<Vec<ListItem>>()
}

/// 在身份面板下方渲染选中身份的详细信息
fn render_identity_detail(frame: &mut Frame, area: Rect, app: &App) {
    if let Some(i) = app.identity_list_state.selected()
        && let Some(identity) = app.identities.get(i)
    {
        let is_current = app
            .current_identity()
            .map(|c| c.identity_id == identity.identity_id)
            .unwrap_or(false);
        let current_tag = if is_current { " [当前]" } else { "" };
        let detail_text = format!(" ML-DSA公钥: {}{}", identity.identity_id, current_tag,);
        let detail = Paragraph::new(detail_text)
            .block(
                Block::default()
                    .title(" 身份详情 (按 c 复制到剪贴板) ")
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Cyan)),
            )
            .wrap(Wrap { trim: false });
        frame.render_widget(Clear, area);
        frame.render_widget(detail, area);
    }
}

pub fn tui_render(frame: &mut Frame, app: &mut App) {
    // 创建布局
    // 水平切分（左右）
    let horizontal_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(25), // 左侧：联系人/身份列表
            Constraint::Percentage(75), // 右侧：主区域
        ])
        .split(frame.area());

    let sidebar_area = horizontal_chunks[0];

    // 将左侧区域再垂直切分（上中下）
    // 身份详情区域只在身份面板获得焦点时显示
    let left_vertical = if app.current_focus == Focus::IdentityArea {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Percentage(40), // 联系人列表
                Constraint::Percentage(30), // 身份列表
                Constraint::Percentage(30), // 身份详情
            ])
            .split(sidebar_area)
    } else {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Percentage(60), // 联系人列表
                Constraint::Percentage(40), // 身份列表
            ])
            .split(sidebar_area)
    };

    let contacts_area = left_vertical[0];
    let identity_area = left_vertical[1];

    // 将右侧区域再垂直切分（上下）
    let right_vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(5),    // 右上：消息列表
            Constraint::Length(3), // 右下：输入框
        ])
        .split(horizontal_chunks[1]);

    let messages_area = right_vertical[0];
    let input_area = right_vertical[1];

    // 渲染消息列表（仅显示当前选中联系人的消息）
    let messages: Vec<ListItem> = app
        .current_messages()
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

    frame.render_stateful_widget(message_list, messages_area, &mut app.message_list_state);

    // 渲染联系人列表
    let mut contacts: Vec<ListItem> = vec!["(无联系人)"]
        .into_iter()
        .map(|s| ListItem::new(Text::from(s.to_string())))
        .collect();

    getcontacts(app, &mut contacts);

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
    frame.render_stateful_widget(contact_list, contacts_area, &mut app.contact_list_state);

    // 渲染身份列表
    let identity_items = get_identity_items(app);
    let identity_list = List::new(identity_items)
        .block(
            Block::default()
                .title(" 身份管理 (F2) ")
                .borders(Borders::ALL)
                .border_style(match app.current_focus {
                    Focus::IdentityArea => Style::default().fg(Color::Yellow),
                    _ => Style::default(),
                }),
        )
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol(">> ");
    let identity_list_state = &mut app.identity_list_state;
    frame.render_stateful_widget(identity_list, identity_area, identity_list_state);

    // 渲染身份详情（仅在身份面板获得焦点时）
    if app.current_focus == Focus::IdentityArea && left_vertical.len() > 2 {
        let detail_area = left_vertical[2];
        render_identity_detail(frame, detail_area, app);
    }

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
        .wrap(Wrap { trim: true });

    frame.render_widget(input, input_area);

    // 如果焦点在输入框，设置光标位置
    if let Focus::Input = app.current_focus {
        frame.set_cursor_position((input_area.x + app.input.len() as u16 + 1, input_area.y + 1));
    }

    // 渲染状态栏
    let current_identity_short = app.current_identity().map(|current| {
        if current.identity_id.len() > 16 {
            format!("{}...", &current.identity_id[..16])
        } else {
            current.identity_id.clone()
        }
    });
    let identity_prefix = match current_identity_short {
        Some(ref id) => format!(" | 当前身份: {}", id),
        None => String::new(),
    };

    let online_indicator = if app.online_peers > 0 {
        format!(" | 在线: {} 个节点", app.online_peers)
    } else {
        String::new()
    };

    let status = match app.current_focus {
        Focus::Messages => format!(
            " 模式: 浏览消息 (↑/↓选择, Enter回复){}{}",
            identity_prefix, online_indicator
        ),
        Focus::Input => format!(
            " 模式: 输入文本 (Enter发送, Tab切换焦点, Esc退出){}{}",
            identity_prefix, online_indicator
        ),
        Focus::SidebarArea => format!(
            " 模式: 选择联系人 (↑/↓选择, Enter确认){}{}",
            identity_prefix, online_indicator
        ),
        Focus::IdentityArea => {
            let mut s = format!(
                " 模式: 身份管理 (↑/↓选择, Enter切换, g生成, d删除, c复制){}{}",
                identity_prefix, online_indicator
            );
            if !app.status_message.is_empty() {
                s.push_str(&format!(" | {}", app.status_message));
            }
            s
        }
    };
    let status_bar = Paragraph::new(status).block(Block::default().borders(Borders::TOP));
    frame.render_widget(status_bar, messages_area);
}
