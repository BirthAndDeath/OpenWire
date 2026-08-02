use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap};

use crate::{App, Focus};

/// 截断字符串到指定显示宽度（近似字符数），为尾部标记预留空间
fn truncate_for_download(msg: &str, available_width: usize) -> String {
    let marker = " [下载]";
    let max_msg_len = available_width.saturating_sub(marker.len());
    if msg.chars().count() > max_msg_len {
        let truncated: String = msg.chars().take(max_msg_len.saturating_sub(3)).collect();
        format!("{}...", truncated)
    } else {
        msg.to_string()
    }
}

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
            let online_mark = if app.online_contacts.contains(&c.mldsa_pubkey_hex) {
                " ●"
            } else {
                ""
            };
            ListItem::new(Text::from(format!(
                "{}{} ({})",
                online_mark, name, short_pk
            )))
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
            Constraint::Length(3), // 中间：输入框
            Constraint::Length(1), // 右下：状态栏
        ])
        .split(horizontal_chunks[1]);

    let messages_area = right_vertical[0];
    let input_area = right_vertical[1];
    let status_area = right_vertical[2];

    // 渲染消息列表（仅显示当前选中联系人的消息）
    let selected_contact = app
        .contact_list_state
        .selected()
        .and_then(|i| app.contacts.get(i))
        .map(|c| &c.mldsa_pubkey_hex);
    // 可用宽度 = 消息区域宽度 - 边框(2) - 高亮符号(3) - 内边距(3)
    let available_width = messages_area.width.saturating_sub(8) as usize;
    let messages: Vec<ListItem> = app
        .current_messages()
        .iter()
        .enumerate()
        .map(|(idx, m)| {
            let is_file_share = selected_contact
                .and_then(|pk| app.file_shares_by_contact.get(pk))
                .and_then(|shares| shares.get(idx))
                .is_some_and(|s| s.is_some());
            if is_file_share {
                let truncated = truncate_for_download(m, available_width);
                ListItem::new(Line::from(vec![
                    Span::raw(truncated),
                    Span::styled(
                        " [下载]",
                        Style::default()
                            .fg(Color::Green)
                            .add_modifier(Modifier::BOLD),
                    ),
                ]))
            } else {
                ListItem::new(Text::from(m.clone()))
            }
        })
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
            " 模式: 浏览消息 (↑/↓选择, Enter回复, →下载文件){}{}",
            identity_prefix, online_indicator
        ),
        Focus::Input => {
            if app.add_contact_mode {
                format!(
                    " 添加联系人: 粘贴对方 ML-DSA 公钥后按 Enter 添加 (Esc取消){}{}",
                    identity_prefix, online_indicator
                )
            } else if app.file_send_mode {
                format!(
                    " 发送文件: 输入文件路径后按 Enter (Esc取消){}{}",
                    identity_prefix, online_indicator
                )
            } else {
                format!(
                    " 输入消息: Enter发送, Tab切换焦点, Esc退出{}{}",
                    identity_prefix, online_indicator
                )
            }
        }
        Focus::SidebarArea => format!(
            " 联系人列表: ↑/↓选择, Enter聊天, a添加联系人, r刷新, d删除{}{}",
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
    frame.render_widget(status_bar, status_area);

    // 下载对话框覆盖层
    if let Some(info) = &app.download_dialog {
        let area = frame.area();
        let dialog_width = area.width.min(60);
        let dialog_height = 8;
        let dialog_x = area.x + (area.width - dialog_width) / 2;
        let dialog_y = area.y + (area.height - dialog_height) / 2;
        let dialog_area = Rect::new(dialog_x, dialog_y, dialog_width, dialog_height);

        frame.render_widget(Clear, dialog_area);
        let dialog = Paragraph::new(vec![
            Line::from(Span::styled(
                " 下载文件 ",
                Style::default().add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::raw(format!(" 文件: {}", info.filename))),
            Line::from(Span::raw(format!(" 保存路径: {}", app.input))),
            Line::from(Span::raw(format!(
                " 默认: {}/downloads/{} (留空按 Enter)",
                app.data_dir.display(),
                info.filename
            ))),
            Line::from(Span::raw("")),
            Line::from(Span::styled(
                " Enter 确认下载 | C 取消 ",
                Style::default().fg(Color::DarkGray),
            )),
        ])
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan)),
        );
        frame.render_widget(dialog, dialog_area);
    }
}
