use crate::{App, Focus};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind};
use hex;

/// 分发事件到对应的焦点处理器
pub async fn handle_event(app: &mut App, event: Event) -> std::io::Result<()> {
    match event {
        Event::Key(key) if key.kind == KeyEventKind::Press => {
            // 下载对话框捕获所有键盘事件
            if app.download_dialog.is_some() {
                handle_download_dialog(app, key).await;
            } else {
                match app.current_focus {
                    Focus::Messages => handle_messages_focus(app, key.code).await,
                    Focus::Input => handle_input_focus(app, key).await,
                    Focus::SidebarArea => handle_sidebar_area_focus(app, key.code).await,
                    Focus::IdentityArea => handle_identity_area_focus(app, key.code).await,
                }
            }
        }
        _ => {}
    }
    Ok(())
}

/// 处理侧边栏（联系人列表）焦点事件
async fn handle_sidebar_area_focus(app: &mut App, key_code: KeyCode) {
    let list_len = app.contacts.len();
    match key_code {
        KeyCode::Up if list_len > 0 => {
            let i = app.contact_list_state.selected().unwrap_or(0);
            app.contact_list_state.select(Some(i.saturating_sub(1)));
        }
        KeyCode::Down if list_len > 0 => {
            let i = app.contact_list_state.selected().unwrap_or(0);
            app.contact_list_state
                .select(Some((i + 1).min(list_len - 1)));
        }
        KeyCode::Enter => {
            if let Some(i) = app.contact_list_state.selected()
                && let Some(contact) = app.contacts.get(i)
            {
                // 切换到输入框前，确保该联系人的最新消息已加载到内存
                if !app
                    .messages_by_contact
                    .contains_key(&contact.mldsa_pubkey_hex)
                {
                    if let Some(pool) = openwire_core::storage::pool() {
                        let owner = openwire_core::storage::get_current_identity(pool)
                            .await
                            .ok()
                            .flatten()
                            .unwrap_or_default();
                        if let Ok(msgs) = openwire_core::storage::get_messages(
                            pool,
                            &owner,
                            &contact.mldsa_pubkey_hex,
                            None,
                            50,
                        )
                        .await
                        {
                            let entry = app
                                .messages_by_contact
                                .entry(contact.mldsa_pubkey_hex.clone())
                                .or_default();
                            let shares = app
                                .file_shares_by_contact
                                .entry(contact.mldsa_pubkey_hex.clone())
                                .or_default();
                            let name = contact.name.as_deref().unwrap_or("(未命名)");
                            entry.push(format!("--- 与 {} 的聊天记录 ---", name));
                            shares.push(None);
                            for msg in msgs.iter().rev() {
                                let prefix = if msg.is_outgoing == 1 {
                                    "[我]"
                                } else {
                                    "[对方]"
                                };
                                let text = format!("{} {}", prefix, msg.content);
                                entry.push(text);
                                shares.push(crate::detect_file_share(&msg));
                            }
                        }
                    }
                }
                // 每次切换联系人都滚动到最新消息
                if let Some(msgs) = app.messages_by_contact.get(&contact.mldsa_pubkey_hex) {
                    let msg_count = msgs.len();
                    if msg_count > 0 {
                        app.message_list_state.select(Some(msg_count - 1));
                    }
                }
                app.current_focus = Focus::Input;
            }
        }
        // a 添加联系人
        KeyCode::Char('a') => {
            app.add_contact_mode = true;
            app.status_message =
                "添加联系人模式：请粘贴对方 ML-DSA 公钥到输入框，然后按 Ctrl+Enter 确认添加"
                    .to_string();
            app.current_focus = Focus::Input;
            app.input.clear();
        }
        // d 删除联系人
        KeyCode::Char('d') => {
            let selected_index = app.contact_list_state.selected();
            let pubkey_to_delete = selected_index
                .and_then(|i| app.contacts.get(i))
                .map(|c| c.mldsa_pubkey_hex.clone());
            if let Some(pubkey) = pubkey_to_delete {
                let pubkey_short = pubkey[..16.min(pubkey.len())].to_string();
                app.push_message(format!("正在删除联系人: {}..", pubkey_short));
                let ok = app.core_handle.delete_contact(&pubkey).await;
                if ok {
                    app.push_message(format!("✅ 已删除联系人: {}..", pubkey_short));
                    app.status_message = format!("已删除联系人: {}..", pubkey_short);
                } else {
                    app.push_message(format!("❌ 删除联系人失败: {}..", pubkey_short));
                    app.status_message = "删除联系人失败".to_string();
                }
                app.refresh_contacts().await;
                // 如果删除后联系人列表为空，重置选中状态
                if app.contacts.is_empty() {
                    app.contact_list_state.select(None);
                } else if let Some(i) = selected_index
                    && i >= app.contacts.len()
                {
                    app.contact_list_state
                        .select(Some(app.contacts.len().saturating_sub(1)));
                }
            }
        }
        // r 刷新联系人列表
        KeyCode::Char('r') => {
            app.refresh_contacts().await;
            app.status_message = "联系人列表已刷新".to_string();
        }
        _ => {}
    }
}

/// 处理消息列表焦点事件
async fn handle_messages_focus(app: &mut App, key_code: KeyCode) {
    let msgs = app.current_messages();
    let list_len = msgs.len();
    match key_code {
        KeyCode::Up if list_len > 0 => {
            let i = app.message_list_state.selected().unwrap_or(0);
            app.message_list_state.select(Some(i.saturating_sub(1)));
        }
        KeyCode::Down if list_len > 0 => {
            let i = app.message_list_state.selected().unwrap_or(0);
            app.message_list_state
                .select(Some((i + 1).min(list_len - 1)));
        }
        KeyCode::Enter => {
            if let Some(i) = app.message_list_state.selected()
                && let Some(msg) = msgs.get(i)
            {
                app.input = format!("回复「{}」: ", msg);
                app.current_focus = Focus::Input;
            }
        }
        KeyCode::Right => {
            if let Some(i) = app.message_list_state.selected()
                && let Some(_msg) = msgs.get(i)
            {
                let contact_pk = app
                    .contact_list_state
                    .selected()
                    .and_then(|ci| app.contacts.get(ci))
                    .map(|c| c.mldsa_pubkey_hex.clone());
                if let Some(pk) = contact_pk {
                    if let Some(shares) = app.file_shares_by_contact.get(&pk) {
                        if let Some(Some(info)) = shares.get(i) {
                            app.download_dialog = Some(info.clone());
                            // 预填默认下载路径，用户可编辑修改
                            let default_dir = app.data_dir.join("downloads");
                            app.input = default_dir
                                .join(&info.filename)
                                .to_string_lossy()
                                .to_string();
                            app.push_message(format!(
                                "按 Enter 下载到默认目录，或编辑路径后按 Enter"
                            ));
                            return;
                        }
                    }
                }
            }
        }
        _ => {}
    }
}

/// 处理下载对话框的键盘事件
async fn handle_download_dialog(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Enter if !app.input.trim().is_empty() => {
            let info = app.download_dialog.take().unwrap();
            let save_path = app.input.trim().to_string();
            app.input.clear();
            let file_hash_bytes = match hex::decode(&info.file_hash) {
                Ok(b) if b.len() == 32 => b,
                _ => {
                    app.push_message("错误：文件哈希无效".to_string());
                    return;
                }
            };
            let mut file_hash = [0u8; 32];
            file_hash_bytes
                .iter()
                .enumerate()
                .for_each(|(i, &b)| file_hash[i] = b);
            app.push_message(format!("正在下载到: {}", save_path));
            let ok = app
                .core_handle
                .request_file_download(
                    &info.sender,
                    file_hash,
                    std::path::PathBuf::from(&save_path),
                )
                .await;
            if ok {
                app.push_message(format!("✅ 下载请求已发送: {}", save_path));
            } else {
                app.push_message("❌ 下载请求失败".to_string());
            }
            let msg_count = app.current_messages().len();
            if msg_count > 0 {
                app.message_list_state.select(Some(msg_count - 1));
            }
        }
        // Esc 取消下载（主循环已确保 Esc 不会误退出程序）
        KeyCode::Esc => {
            app.download_dialog = None;
            app.input.clear();
        }
        // 'c' 键在输入为空时也可取消（首次打开对话框时方便操作）
        KeyCode::Char('c') if app.input.is_empty() => {
            app.download_dialog = None;
        }
        KeyCode::Char(c) => app.input.push(c),
        KeyCode::Backspace => {
            app.input.pop();
        }
        _ => {}
    }
}

/// 处理输入框焦点事件
async fn handle_input_focus(app: &mut App, key_event: KeyEvent) {
    let is_ctrl = key_event
        .modifiers
        .contains(crossterm::event::KeyModifiers::CONTROL);
    match key_event.code {
        KeyCode::Enter if !app.input.trim().is_empty() => {
            // Ctrl+Enter 或添加联系人模式：添加联系人
            if is_ctrl || app.add_contact_mode {
                let pubkey_hex = app.input.trim().to_string();
                if !pubkey_hex.is_empty() {
                    // 验证公钥格式
                    if !openwire_core::validate_mldsa_pubkey_hex(&pubkey_hex) {
                        app.push_message(
                            "错误: ML-DSA 公钥格式不正确（应为3904字符的hex编码）".to_string(),
                        );
                        app.input.clear();
                        app.add_contact_mode = false;
                        app.status_message.clear();
                        return;
                    }
                    app.push_message(format!(
                        "正在添加联系人: {}... (通过 DHT 网络查询身份绑定)",
                        &pubkey_hex[..16.min(pubkey_hex.len())]
                    ));
                    let ok = app.core_handle.add_contact(&pubkey_hex, None).await;
                    if ok {
                        app.push_message(format!(
                            "✅ 已添加联系人: {}..",
                            &pubkey_hex[..16.min(pubkey_hex.len())]
                        ));
                        app.refresh_contacts().await;
                    } else {
                        app.push_message(
                            "❌ 添加联系人失败：无法验证身份绑定或联系人已存在".to_string(),
                        );
                    }
                    app.input.clear();
                    app.add_contact_mode = false;
                    app.status_message.clear();
                    let msg_count = app.current_messages().len();
                    if msg_count > 0 {
                        app.message_list_state.select(Some(msg_count - 1));
                    }
                }
                return;
            }

            // 文件发送模式：输入文件路径后按 Enter 发送文件
            if app.file_send_mode {
                let selected_contact_index = app.contact_list_state.selected().unwrap_or(0);
                let contact_pubkey = app
                    .contacts
                    .get(selected_contact_index)
                    .map(|c| c.mldsa_pubkey_hex.clone());
                if let Some(ref pubkey) = contact_pubkey {
                    let file_path = app.input.trim().to_string();
                    if !file_path.is_empty() {
                        app.push_message(format!("正在发送文件: {}", file_path));
                        let sent = app
                            .core_handle
                            .send_file(pubkey, std::path::Path::new(&file_path))
                            .await;
                        if sent {
                            app.push_message(format!("文件发送完成: {}", file_path));
                        } else {
                            app.push_message(format!("文件发送失败: {}", file_path));
                        }
                    }
                } else {
                    app.push_message("错误: 未选择联系人".to_string());
                }
                app.file_send_mode = false;
                app.status_message.clear();
                app.input.clear();
                let msg_count = app.current_messages().len();
                if msg_count > 0 {
                    app.message_list_state.select(Some(msg_count - 1));
                }
                return;
            }

            // 普通 Enter：发送消息
            // 获取当前选中的联系人（先克隆公钥避免借用冲突）
            let selected_contact_index = app.contact_list_state.selected().unwrap_or(0);
            let contact_pubkey = app
                .contacts
                .get(selected_contact_index)
                .map(|c| c.mldsa_pubkey_hex.clone());
            if let Some(ref pubkey) = contact_pubkey {
                // 先显示发送中的消息
                app.push_message(format!("[我] {}", app.input));
                let sent = app.core_handle.send_msg(pubkey, &app.input).await;
                if !sent {
                    app.push_message("错误: 消息发送失败".to_string());
                }
            } else {
                app.push_message("错误: 未选择联系人".to_string());
            }

            app.input.clear();
            let msg_count = app.current_messages().len();
            if msg_count > 0 {
                app.message_list_state.select(Some(msg_count - 1));
            }
        }
        // Ctrl+F: 发送文件（打开文件选择对话框）
        KeyCode::Char('f') if is_ctrl => {
            let selected_contact_index = app.contact_list_state.selected().unwrap_or(0);
            if app.contacts.get(selected_contact_index).is_some() {
                // 使用 dialog 选择文件（如果可用），否则提示输入路径
                app.push_message("请输入文件路径后按 Enter 发送文件: ".to_string());
                app.status_message = "文件发送模式：输入文件路径后按 Enter".to_string();
                // 标记输入框处于文件路径输入模式
                app.file_send_mode = true;
                app.input.clear();
            } else {
                app.push_message("错误: 未选择联系人".to_string());
            }
            let msg_count = app.current_messages().len();
            if msg_count > 0 {
                app.message_list_state.select(Some(msg_count - 1));
            }
        }
        // Esc: 取消添加联系人/文件发送模式，或退出输入框
        KeyCode::Esc => {
            if app.download_dialog.is_some() {
                app.download_dialog = None;
                app.input.clear();
            } else if app.add_contact_mode {
                app.add_contact_mode = false;
                app.status_message = "已取消添加联系人".to_string();
            } else if app.file_send_mode {
                app.file_send_mode = false;
                app.status_message = "已取消文件发送".to_string();
            } else {
                app.current_focus = Focus::SidebarArea;
            }
            app.input.clear();
        }
        KeyCode::Char(c) => app.input.push(c),
        KeyCode::Backspace => {
            app.input.pop();
        }
        _ => {}
    }
}

/// 处理身份管理焦点事件
async fn handle_identity_area_focus(app: &mut App, key_code: KeyCode) {
    let list_len = app.identities.len();
    match key_code {
        KeyCode::Up if list_len > 0 => {
            let i = app.identity_list_state.selected().unwrap_or(0);
            app.identity_list_state.select(Some(i.saturating_sub(1)));
            app.status_message.clear();
        }
        KeyCode::Down if list_len > 0 => {
            let i = app.identity_list_state.selected().unwrap_or(0);
            app.identity_list_state
                .select(Some((i + 1).min(list_len - 1)));
            app.status_message.clear();
        }
        // Enter 切换身份
        KeyCode::Enter => {
            if let Some(i) = app.identity_list_state.selected()
                && let Some(identity) = app.identities.get(i)
            {
                if identity.is_current == 0 {
                    let ok = app
                        .core_handle
                        .select_identity(identity.identity_id.clone())
                        .await;
                    if ok {
                        app.status_message = format!(
                            "已切换到身份: {}",
                            &identity.identity_id[..16.min(identity.identity_id.len())]
                        );
                        // 切换身份后刷新联系人和消息列表（新身份有不同的联系人）
                        app.refresh_contacts().await;
                        app.refresh_identities().await;
                        app.messages_by_contact.clear();
                        app.file_shares_by_contact.clear();
                        // 重新加载历史消息
                        if let Some(pool) = openwire_core::storage::pool() {
                            let owner = openwire_core::storage::get_current_identity(pool)
                                .await
                                .ok()
                                .flatten()
                                .unwrap_or_default();
                            for contact in &app.contacts.clone() {
                                if let Ok(msgs) = openwire_core::storage::get_messages(
                                    pool,
                                    &owner,
                                    &contact.mldsa_pubkey_hex,
                                    None,
                                    50,
                                )
                                .await
                                {
                                    let entry = app
                                        .messages_by_contact
                                        .entry(contact.mldsa_pubkey_hex.clone())
                                        .or_default();
                                    let shares = app
                                        .file_shares_by_contact
                                        .entry(contact.mldsa_pubkey_hex.clone())
                                        .or_default();
                                    let name = contact.name.as_deref().unwrap_or("(未命名)");
                                    entry.push(format!("--- 与 {} 的聊天记录 ---", name));
                                    shares.push(None);
                                    for msg in msgs.iter().rev() {
                                        let prefix = if msg.is_outgoing == 1 {
                                            "[我]"
                                        } else {
                                            "[对方]"
                                        };
                                        let text = format!("{} {}", prefix, msg.content);
                                        entry.push(text);
                                        shares.push(crate::detect_file_share(&msg));
                                    }
                                }
                            }
                        }
                    } else {
                        app.status_message = "错误: 切换身份失败".to_string();
                    }
                } else {
                    app.status_message = "已经是当前身份".to_string();
                }
            }
        }
        // g 生成新身份
        KeyCode::Char('g') => {
            let ok = app.core_handle.generate_identity().await;
            if ok {
                app.status_message = "已生成新身份，请按 r 刷新列表".to_string();
            } else {
                app.status_message = "错误: 生成身份失败".to_string();
            }
            app.refresh_identities().await;
        }
        // d 删除身份
        KeyCode::Char('d') => {
            if let Some(i) = app.identity_list_state.selected()
                && let Some(identity) = app.identities.get(i)
            {
                if identity.is_current == 0 {
                    let ok = app
                        .core_handle
                        .delete_identity(identity.identity_id.clone())
                        .await;
                    if ok {
                        app.status_message = format!(
                            "已删除身份: {}",
                            &identity.identity_id[..16.min(identity.identity_id.len())]
                        );
                    } else {
                        app.status_message = "错误: 删除身份失败".to_string();
                    }
                    app.refresh_identities().await;
                } else {
                    app.status_message = "不能删除当前身份".to_string();
                }
            }
        }
        // r 刷新身份列表
        KeyCode::Char('r') => {
            app.refresh_identities().await;
            app.status_message = "身份列表已刷新".to_string();
        }
        // c 复制选中身份的 ML-DSA 公钥到剪贴板
        KeyCode::Char('c') => {
            if let Some(i) = app.identity_list_state.selected()
                && let Some(identity) = app.identities.get(i)
            {
                let copy_text = identity.identity_id.clone();

                // 使用复用的剪贴板实例
                let copied = copy_to_clipboard(app, &copy_text);

                if copied {
                    app.status_message =
                        format!("已复制身份数据到剪贴板 ({} 字符)", copy_text.len());
                } else {
                    // 如果剪贴板不可用，将数据写入临时文件
                    let tmp_path = std::env::temp_dir().join("chat_identity_data.txt");
                    if std::fs::write(&tmp_path, &copy_text).is_ok() {
                        app.status_message =
                            format!("剪贴板不可用，数据已保存到: {}", tmp_path.display());
                    } else {
                        app.status_message = "复制失败，请手动查看下方详情面板中的数据".to_string();
                    }
                }
            }
        }
        _ => {}
    }
}

/// 使用 App 中复用的剪贴板实例复制文本
fn copy_to_clipboard(app: &mut App, text: &str) -> bool {
    if app.clipboard.is_none() {
        match arboard::Clipboard::new() {
            Ok(c) => app.clipboard = Some(c),
            Err(e) => {
                eprintln!("Warning: Failed to create clipboard: {e}");
                return false;
            }
        }
    }
    app.clipboard.as_mut().unwrap().set_text(text.to_string()).is_ok()
}

impl Focus {
    /// 切换到下一个焦点区域
    pub fn next_focus(self) -> Self {
        match self {
            Focus::Input => Focus::Messages,
            Focus::Messages => Focus::SidebarArea,
            Focus::SidebarArea => Focus::Input,
            Focus::IdentityArea => Focus::Input,
        }
    }
}
