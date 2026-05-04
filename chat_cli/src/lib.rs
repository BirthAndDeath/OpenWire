// 定义应用状态（Model）
#![doc = include_str!("../../README.md")]
use std::collections::HashMap;

use anyhow::Context;
use chat_core::ChatCore;
use chat_core::storage::{self, Contact, Identity, list_contacts, list_identities, pool};
use etcetera::BaseStrategy;
use ratatui::widgets::ListState;
pub mod notui;
pub mod tui;
pub mod use_json;
pub struct App {
    // --- 焦点系统 ---
    current_focus: Focus,

    // --- 消息列表组件及其状态 ---
    /// 按联系人公钥分组的消息列表
    /// key: 联系人 ML-DSA 公钥 hex, value: 该联系人的消息列表
    messages_by_contact: HashMap<String, Vec<String>>,
    message_list_state: ListState,

    //contacts:HashMap<id,Socket>;
    contact_list_state: ListState,
    // --- 输入框组件 ---
    input: String, // 当前输入的文本

    should_quit: bool,
    core: Option<ChatCore>,
    core_handle: chat_core::corehandle::CoreHandle,
    contacts: Vec<Contact>, // 联系人列表
    // --- 身份管理 ---
    identities: Vec<Identity>,
    identity_list_state: ListState,
    status_message: String, // 状态提示信息
    /// 当前选中的联系人 ML-DSA 公钥 hex（用于过滤消息显示）
    selected_contact: Option<String>,
    /// 在线联系人数量（通过 libp2p 连接事件更新）
    online_peers: usize,
    /// 文件发送模式：为 true 时，按 Enter 将输入框内容作为文件路径发送
    file_send_mode: bool,
    /// 剪贴板实例（复用避免每次创建）
    clipboard: Option<arboard::Clipboard>,
}

impl App {
    /// 获取当前选中联系人的消息列表
    pub fn current_messages(&self) -> &[String] {
        let contact_key = self
            .contact_list_state
            .selected()
            .and_then(|i| self.contacts.get(i))
            .map(|c| &c.mldsa_pubkey_hex);
        match contact_key {
            Some(key) => self
                .messages_by_contact
                .get(key)
                .map(|v| v.as_slice())
                .unwrap_or_default(),
            None => &[],
        }
    }

    /// 消息列表最大条数（防止无限增长）
    const MAX_MESSAGES_PER_CONTACT: usize = 1000;

    /// 向当前选中联系人的消息列表添加消息
    pub fn push_message(&mut self, text: String) {
        let contact_key = self
            .contact_list_state
            .selected()
            .and_then(|i| self.contacts.get(i))
            .map(|c| c.mldsa_pubkey_hex.clone());
        if let Some(key) = contact_key {
            let msgs = self.messages_by_contact.entry(key).or_default();
            msgs.push(text);
            // 限制消息列表大小，防止内存无限增长
            if msgs.len() > Self::MAX_MESSAGES_PER_CONTACT {
                msgs.remove(0);
            }
        }
    }

    /// 向指定联系人的消息列表添加消息
    pub fn push_message_to(&mut self, contact_pubkey: &str, text: String) {
        let msgs = self
            .messages_by_contact
            .entry(contact_pubkey.to_string())
            .or_default();
        msgs.push(text);
        // 限制消息列表大小，防止内存无限增长
        if msgs.len() > Self::MAX_MESSAGES_PER_CONTACT {
            msgs.remove(0);
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq)]
// 定义焦点枚举
enum Focus {
    Messages,
    Input,
    SidebarArea,
    IdentityArea,
}

impl App {
    /// 尝试初始化应用。
    ///
    /// 如果提供了 `password`（原始密码），使用 Argon2id 派生密钥后初始化。
    /// 否则先尝试无密码初始化；如果 Keyring 不可用，交互式提示用户输入密码。
    pub async fn try_init(password: Option<&str>) -> anyhow::Result<App> {
        let data_dir = etcetera::choose_base_strategy()
            .context("Failed to get base strategy")?
            .config_dir()
            .join("myapp");
        let log_path = data_dir.join("log");
        #[cfg(debug_assertions)]
        let log_level = "debug";
        #[cfg(not(debug_assertions))]
        let log_level = "info";

        // 如果用户通过 --password 提供了原始密码，使用统一 KDF 派生密钥
        if let Some(raw_password) = password {
            let key_hex =
                rootcell::identity::PrivateKeyHandle::derive_key_from_password(raw_password);
            let mut cfg = chat_core::CoreConfig::new(
                data_dir.clone(),
                Some(log_path.clone()),
                Some(log_level),
            );
            cfg.passwd = Some(key_hex);
            let core = chat_core::ChatCore::try_init(cfg).await?;
            return Self::build_app(core, data_dir).await;
        }

        // 先尝试无密码初始化（Keyring 可用的情况）
        let cfg =
            chat_core::CoreConfig::new(data_dir.clone(), Some(log_path.clone()), Some(log_level));
        match chat_core::ChatCore::try_init(cfg).await {
            Ok(core) => {
                return Self::build_app(core, data_dir).await;
            }
            Err(e) => {
                // Keyring 不可用，需要用户输入密码
                eprintln!("\n⚠️  系统密钥环（Keyring）不可用，无法自动管理加密密钥。");
                eprintln!("   原因: {}", e);
                eprintln!("   建议: 安装并配置密钥环服务（如 gnome-keyring/seahorse），");
                eprintln!("         或使用密码派生密钥（安全性较低）\n");
                eprintln!("⚠️  重要警告：如果忘记密码，将无法解密私钥文件，");
                eprintln!("   身份将永久丢失，无法启动应用！请务必牢记密码！\n");
                eprint!("🔑 请输入密码（输入空行取消）: ");

                // 使用 rpassword 安全读取密码（不回显）
                let password = match rpassword::read_password() {
                    Ok(p) => p,
                    Err(rp_err) => {
                        anyhow::bail!("无法读取密码输入: {}. 原始错误: {}", rp_err, e);
                    }
                };

                if password.trim().is_empty() {
                    anyhow::bail!("用户取消了密码输入。Keyring 错误: {}", e);
                }

                // 使用统一的 Argon2id KDF 派生密钥
                eprintln!("⏳ 正在派生加密密钥（Argon2id, 64 MiB）...");
                let key_hex =
                    rootcell::identity::PrivateKeyHandle::derive_key_from_password(&password);
                // 立即清零密码内存
                drop(password);

                let mut cfg = chat_core::CoreConfig::new(
                    data_dir.clone(),
                    Some(log_path.clone()),
                    Some(log_level),
                );
                cfg.passwd = Some(key_hex);
                match chat_core::ChatCore::try_init(cfg).await {
                    Ok(core) => {
                        eprintln!("✅ 密码派生密钥初始化成功（安全性低于 Keyring）\n");
                        return Self::build_app(core, data_dir).await;
                    }
                    Err(e2) => {
                        anyhow::bail!("密码派生密钥初始化也失败: {}. 原始 Keyring 错误: {}", e2, e);
                    }
                }
            }
        }
    }

    /// 在初始化 ChatCore 后构建 App 实例
    async fn build_app(core: ChatCore, _data_dir: std::path::PathBuf) -> anyhow::Result<App> {
        let mut list_state = ListState::default();
        list_state.select(Some(0)); // 默认选中第一条消息
        let core_handle = core.core_handle.clone();
        let pool = pool().context("Database pool not initialized")?;
        let owner = storage::get_current_identity(pool)
            .await
            .ok()
            .flatten()
            .unwrap_or_default();
        let contacts = list_contacts(pool, &owner).await?;
        let identities = list_identities(pool).await?;

        // 加载历史消息（按联系人分组）
        let mut messages_by_contact: HashMap<String, Vec<String>> = HashMap::new();
        for contact in &contacts {
            if let Ok(msgs) =
                chat_core::storage::get_messages(pool, &owner, &contact.mldsa_pubkey_hex, None, 50)
                    .await
            {
                let entry = messages_by_contact
                    .entry(contact.mldsa_pubkey_hex.clone())
                    .or_default();
                let name = contact.name.as_deref().unwrap_or("(未命名)");
                entry.push(format!("--- 与 {} 的聊天记录 ---", name));
                for msg in msgs.iter().rev() {
                    let prefix = if msg.is_outgoing == 1 {
                        "[我]"
                    } else {
                        "[对方]"
                    };
                    entry.push(format!("{} {}", prefix, msg.content));
                }
            }
        }

        Ok(App {
            current_focus: Focus::Input,
            messages_by_contact,
            message_list_state: list_state,
            contact_list_state: ListState::default(),
            input: String::new(),
            should_quit: false,
            core: Some(core),
            core_handle,
            contacts,
            identities,
            identity_list_state: ListState::default(),
            status_message: String::new(),
            selected_contact: None,
            online_peers: 0,
            file_send_mode: false,
            clipboard: None,
        })
    }

    /// 刷新联系人列表
    pub async fn refresh_contacts(&mut self) {
        if let Some(pool) = pool() {
            let owner = storage::get_current_identity(pool)
                .await
                .ok()
                .flatten()
                .unwrap_or_default();
            if let Ok(contacts) = list_contacts(pool, &owner).await {
                self.contacts = contacts;
            }
        }
    }

    /// 刷新身份列表
    pub async fn refresh_identities(&mut self) {
        if let Some(pool) = pool()
            && let Ok(identities) = list_identities(pool).await
        {
            self.identities = identities;
        }
    }

    /// 获取当前身份信息
    pub fn current_identity(&self) -> Option<&Identity> {
        self.identities.iter().find(|id| id.is_current == 1)
    }
}
