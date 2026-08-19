// 定义应用状态（Model）
#![doc = include_str!("../../README.md")]
use std::collections::{HashMap, HashSet};

use error::CliError;
use etcetera::BaseStrategy;
use openwire_core::ChatCore;
use openwire_core::IncomingMessage;
use openwire_core::MessageEvent;
use openwire_core::storage::{self, Contact, Identity, list_contacts, list_identities, pool};
use ratatui::widgets::ListState;
pub mod error;
pub mod notui;
pub mod tui;
pub mod use_json;

/// 移除字符串中的终端转义序列，防止终端注入攻击
/// 保留可打印 ASCII 与普通非 ASCII 字符（CJK/emoji 等），剥离：
/// - C0/C1 控制字符（ESC 等）
/// - Unicode 格式控制字符（bidi 覆盖/定向符、零宽字符等）
pub fn strip_escape(s: &str) -> String {
    use unicode_properties::GeneralCategory;
    use unicode_properties::UnicodeGeneralCategory;
    s.chars()
        .filter(|&c| {
            if c.is_control() {
                return false;
            }
            let gc = c.general_category();
            if gc == GeneralCategory::Format
                || gc == GeneralCategory::LineSeparator
                || gc == GeneralCategory::ParagraphSeparator
            {
                return false;
            }
            c.is_ascii_graphic() || c == ' ' || !c.is_ascii()
        })
        .collect()
}

/// ML-DSA 公钥验证失败的提示文案（CLI 多入口共用，避免漂移）
pub const MLDSA_PUBKEY_INVALID: &str =
    "ML-DSA 公钥无效（格式或密码学验证失败，应为3904字符的hex编码）";

/// 文件分享信息，用于 TUI 渲染下载按钮
#[derive(Debug, Clone)]
pub struct FileShareInfo {
    pub file_hash: String,
    pub sender: String,
    pub filename: String,
}

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
    core_handle: openwire_core::corehandle::CoreHandle,
    contacts: Vec<Contact>, // 联系人列表
    // --- 身份管理 ---
    identities: Vec<Identity>,
    identity_list_state: ListState,
    status_message: String, // 状态提示信息
    /// 在线联系人数量（通过 libp2p 连接事件更新）
    online_peers: usize,
    /// 在线联系人的 ML-DSA 公钥 hex 集合（用于 per-contact 在线指示器）
    online_contacts: HashSet<String>,
    /// 文件发送模式：为 true 时，按 Enter 将输入框内容作为文件路径发送
    file_send_mode: bool,
    /// 添加联系人模式：为 true 时，按 Ctrl+Enter 将输入框内容作为公钥添加联系人
    add_contact_mode: bool,
    /// 剪贴板实例（复用避免每次创建）
    clipboard: Option<arboard::Clipboard>,
    /// 文件分享信息映射（与 messages_by_contact 同步），用于 TUI 渲染下载按钮
    pub file_shares_by_contact: HashMap<String, Vec<Option<FileShareInfo>>>,
    /// 下载对话框状态：为 Some 时显示覆盖层让用户输入保存路径
    pub download_dialog: Option<FileShareInfo>,
    /// 数据目录，用于提供默认下载路径
    pub data_dir: std::path::PathBuf,
}

impl App {
    /// 滚动消息列表到底部
    pub fn scroll_to_bottom(&mut self) {
        let msg_count = self.current_messages().len();
        if msg_count > 0 {
            self.message_list_state.select(Some(msg_count - 1));
        }
    }

    /// 从数据库加载指定联系人的历史消息
    pub async fn load_messages_for_contact(&mut self, pubkey: &str) {
        if self.messages_by_contact.contains_key(pubkey) {
            return;
        }
        let Some(pool) = openwire_core::storage::pool() else { return };
        let owner = openwire_core::storage::get_current_identity(pool)
            .await
            .ok()
            .flatten()
            .unwrap_or_default();
        let Ok(msgs) = openwire_core::storage::get_messages(
            pool,
            &owner,
            pubkey,
            None,
            50,
        )
        .await else { return };
        let entry = self.messages_by_contact.entry(pubkey.to_string()).or_default();
        let shares = self.file_shares_by_contact.entry(pubkey.to_string()).or_default();
        let contact = self.contacts.iter().find(|c| c.mldsa_pubkey_hex == pubkey);
        let name = contact.and_then(|c| c.name.as_deref()).unwrap_or("(未命名)");
        entry.push(format!("--- 与 {} 的聊天记录 ---", name));
        shares.push(None);
        for msg in msgs.iter().rev() {
            let prefix = if msg.is_outgoing == 1 { "[我]" } else { "[对方]" };
            entry.push(format!("{} {}", prefix, crate::strip_escape(&msg.content)));
            shares.push(crate::detect_file_share(msg));
        }
    }

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
            let msgs = self.messages_by_contact.entry(key.clone()).or_default();
            let shares = self.file_shares_by_contact.entry(key).or_default();
            msgs.push(text);
            shares.push(None);
            if msgs.len() > Self::MAX_MESSAGES_PER_CONTACT {
                msgs.remove(0);
                shares.remove(0);
            }
        }
    }

    /// 向指定联系人的消息列表添加消息
    pub fn push_message_to(&mut self, contact_pubkey: &str, text: String) {
        let msgs = self
            .messages_by_contact
            .entry(contact_pubkey.to_string())
            .or_default();
        let shares = self
            .file_shares_by_contact
            .entry(contact_pubkey.to_string())
            .or_default();
        msgs.push(text);
        shares.push(None);
        // 限制消息列表大小，防止内存无限增长
        if msgs.len() > Self::MAX_MESSAGES_PER_CONTACT {
            msgs.remove(0);
            shares.remove(0);
        }
    }
}

/// 从数据库消息中检测文件分享并返回 FileShareInfo
pub fn detect_file_share(msg: &openwire_core::storage::Message) -> Option<FileShareInfo> {
    if msg.msgtype != openwire_core::ChatMessageType::FileHash as i32 {
        return None;
    }
    let bytes = hex::decode(&msg.content).ok()?;
    let info = postcard::from_bytes::<openwire_core::message::FileHashInfo>(&bytes).ok()?;
    Some(FileShareInfo {
        file_hash: hex::encode(info.file_hash),
        sender: msg.peer_pubkey_hex.clone(),
        filename: info.filename,
    })
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
    pub async fn try_init() -> Result<App, CliError> {
        let data_dir = etcetera::choose_base_strategy()
            .map_err(|_| CliError::BaseStrategyFailed)?
            .config_dir()
            .join("openwire");
        let log_path = data_dir.join("log");
        #[cfg(debug_assertions)]
        let log_level = "debug";
        #[cfg(not(debug_assertions))]
        let log_level = "info";

        if !rootcell::identity::PrivateKeyHandle::check_keyring_available() {
            return Err(CliError::KeyringNotAvailable);
        }

        let mut cfg = openwire_core::CoreConfig::new(
            data_dir.clone(),
            Some(log_path.clone()),
            Some(log_level),
        );
        cfg.load_nodes_config();
        let core = openwire_core::ChatCore::try_init(cfg).await?;
        Self::build_app(core, data_dir).await
    }

    /// 在初始化 ChatCore 后构建 App 实例
    async fn build_app(core: ChatCore, data_dir: std::path::PathBuf) -> Result<App, CliError> {
        let mut list_state = ListState::default();
        list_state.select(Some(0)); // 默认选中第一条消息
        let core_handle = core.core_handle.clone();
        let pool = pool().ok_or(CliError::PoolNotInitialized)?;
        let owner = storage::get_current_identity(pool)
            .await
            .ok()
            .flatten()
            .unwrap_or_default();
        let contacts = list_contacts(pool, &owner).await?;
        let identities = list_identities(pool).await?;

        // 加载历史消息（按联系人分组）
        let mut messages_by_contact: HashMap<String, Vec<String>> = HashMap::new();
        let mut file_shares_by_contact: HashMap<String, Vec<Option<FileShareInfo>>> =
            HashMap::new();
        for contact in &contacts {
            if let Ok(msgs) = openwire_core::storage::get_messages(
                pool,
                &owner,
                &contact.mldsa_pubkey_hex,
                None,
                50,
            )
            .await
            {
                let entry = messages_by_contact
                    .entry(contact.mldsa_pubkey_hex.clone())
                    .or_default();
                let shares = file_shares_by_contact
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
let text = format!("{} {}", prefix, crate::strip_escape(&msg.content));
                    entry.push(text);
                    shares.push(detect_file_share(msg));
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
            online_peers: 0,
            online_contacts: HashSet::new(),
            file_send_mode: false,
            add_contact_mode: false,
            clipboard: None,
            file_shares_by_contact,
            download_dialog: None,
            data_dir,
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

    /// 统一处理 MessageEvent — 更新 App 内部状态
    ///
    /// 所有 CLI 模式（notui/tui/json）共享此方法，避免重复的 match 逻辑。
    /// 调用方在返回后根据自身模式做额外输出（println / JSON / 渲染）。
    pub fn handle_message_event(&mut self, msg: MessageEvent) {
        match msg {
            MessageEvent::ReceiveMessage(msg) => {
                match msg {
                    IncomingMessage::Text { text, sender } => {
                        self.push_message_to(&sender, format!("[对方] {}", strip_escape(&text)));
                    }
                    IncomingMessage::FileShare {
                        filename,
                        file_hash,
                        total_size: _,
                        sender,
                        ..
                    } => {
                        self.push_message_to(
                            &sender,
                            format!("[文件] {} [hash:{}]", strip_escape(&filename), file_hash),
                        );
                        // 更新最后一条消息的 file_shares 条目（由 push_message_to 推入的 None）
                        if let Some(shares) = self.file_shares_by_contact.get_mut(&sender)
                            && let Some(last) = shares.last_mut() {
                                *last = Some(FileShareInfo {
                                    file_hash,
                                    sender: sender.clone(),
                                    filename: filename.clone(),
                                });
                            }
                    }
                    IncomingMessage::DeliveryReceipt { peer_id, .. } => {
                        self.push_message_to(&peer_id, "[系统] 消息已送达 ✓".to_string());
                    }
                    IncomingMessage::MessageSent { .. } => {
                        // 消息已发送通知，CLI 不需要额外处理
                    }
                }
                // 更新选中状态到最新消息
                let msg_count = self.current_messages().len();
                if msg_count > 0 {
                    self.message_list_state.select(Some(msg_count - 1));
                }
            }
            MessageEvent::OnlineStatus { online_contacts } => {
                self.online_peers = online_contacts.len();
                self.online_contacts = online_contacts.into_iter().collect();
            }
            MessageEvent::FileTransferProgress(progress) => {
                self.push_message(format!(
                    "[文件传输] {} ({}/{}) - {}",
                    strip_escape(&progress.filename),
                    progress.received_bytes,
                    progress.total_size,
                    progress.status,
                ));
                let msg_count = self.current_messages().len();
                if msg_count > 0 {
                    self.message_list_state.select(Some(msg_count - 1));
                }
            }
            MessageEvent::Warning(data) => {
                self.push_message(format!("[警告] {}", strip_escape(&data)));
            }
            MessageEvent::Log(data) => {
                self.push_message(format!("[日志] {}", strip_escape(&data)));
            }
            MessageEvent::Error(data) => {
                self.push_message(format!("[错误] {}", strip_escape(&data)));
            }
            MessageEvent::ContactOnlineStatus {
                mldsa_pubkey_hex,
                online,
            } => {
                let short = if mldsa_pubkey_hex.len() > 16 {
                    format!("{}...", &mldsa_pubkey_hex[..16.min(mldsa_pubkey_hex.len())])
                } else {
                    mldsa_pubkey_hex.clone()
                };
                let status = if online { "在线" } else { "离线" };
                self.push_message(format!("[在线状态] {} {}", short, status));
            }
            MessageEvent::IdentityChanged { .. } => {
                // CLI 不需要缓存 ML-KEM 公钥，忽略
            }
        }
    }

    /// 向指定联系人的消息列表添加一条数据库历史消息，自动检测文件分享
    pub fn push_history_message(
        &mut self,
        contact_pubkey: &str,
        prefix: &str,
        msg: &openwire_core::storage::Message,
    ) {
        let text = format!("{} {}", prefix, crate::strip_escape(&msg.content));
        let msgs = self
            .messages_by_contact
            .entry(contact_pubkey.to_string())
            .or_default();
        let shares = self
            .file_shares_by_contact
            .entry(contact_pubkey.to_string())
            .or_default();
        msgs.push(text);
        shares.push(crate::detect_file_share(msg));
        if msgs.len() > Self::MAX_MESSAGES_PER_CONTACT {
            msgs.remove(0);
            shares.remove(0);
        }
    }
}
