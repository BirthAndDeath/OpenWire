// 定义应用状态（Model）
#![doc = include_str!("../../README.md")]
use chat_core::{ChatCommand, ChatCore};
use ratatui::widgets::ListState;
use tokio::sync::mpsc;
pub mod notui;
pub mod tui;

pub struct AppData {
    pub cmd_tx: mpsc::Sender<ChatCommand>,
}
pub struct App {
    // --- 焦点系统 ---
    current_focus: Focus,

    // --- 消息列表组件及其状态 ---
    messages: Vec<String>, // 所有消息
    message_list_state: ListState,

    //contacts:HashMap<id,Socket>;
    contact_list_state: ListState,
    // --- 输入框组件 ---
    input: String, // 当前输入的文本

    should_quit: bool,
    core: Option<ChatCore>,
    app_data: AppData,
}
#[derive(Debug, Clone, Copy, PartialEq)]
// 定义焦点枚举
enum Focus {
    Messages,
    Input,
    SidebarArea,
}

impl App {
    pub async fn try_init() -> anyhow::Result<App> {
        let mut list_state = ListState::default();
        list_state.select(Some(0)); // 默认选中第一条消息
        let (cmd_tx, cmd_rx) = mpsc::channel::<ChatCommand>(64);
        let app_data = AppData { cmd_tx };
        let home_dir = std::env::home_dir().expect("failedto get home dir");
        let database_path = home_dir.join(".chat/database.sqlite");
        let log_path = home_dir.join(".chat/log");
        #[cfg(debug_assertions)]
        let log_level = "debug";
        #[cfg(not(debug_assertions))]
        let log_level = "info";

        let cfg =
            chat_core::CoreConfig::new(database_path, cmd_rx, Some(log_path), Some(log_level));
        let core = chat_core::ChatCore::try_init(cfg).await?;

        Ok(App {
            current_focus: Focus::Input,
            messages: vec![
                "欢迎使用 chat cli".to_string(),
                "按 Ctrl+Tab 切换焦点，↑↓ 选择消息".to_string(),
                "按 Esc或Ctrl+C 退出应用，在输入框中Ctrl+Enter 发送".to_string(),
            ],
            message_list_state: list_state,
            contact_list_state: list_state,
            input: String::new(),
            should_quit: false,
            core: Some(core),
            app_data,
        })
    }
}
