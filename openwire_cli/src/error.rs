#[derive(Debug, thiserror::Error)]
pub enum CliError {
    #[error("核心未初始化")]
    CoreNotInitialized,

    #[error("消息通道未初始化")]
    ChannelNotInitialized,

    #[error("数据库连接池未初始化")]
    PoolNotInitialized,

    #[error("基础路径获取失败")]
    BaseStrategyFailed,

    #[error("系统密钥环（Keyring）不可用。OpenWire 需要系统密钥环来安全存储加密密钥。\n请确保已安装并配置密钥环服务：\n  - Windows: Credential Manager（默认可用）\n  - macOS: Keychain（默认可用）\n  - Linux: 安装 gnome-keyring 或 kwallet\n  - Android/iOS: 平台内置密钥环")]
    KeyringNotAvailable,

    #[error("I/O 错误: {0}")]
    IoError(#[from] std::io::Error),

    #[error("JSON 序列化失败: {0}")]
    JsonError(#[from] serde_json::Error),

    #[error("openwire_core 错误: {0}")]
    CoreError(#[from] openwire_core::error::CoreError),

    #[error("存储错误: {0}")]
    StorageError(#[from] openwire_core::error::StorageError),
}

pub type CliResult<T> = Result<T, CliError>;