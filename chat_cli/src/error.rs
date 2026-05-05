//! # chat_cli 错误类型定义
//!
//! 使用 thiserror 定义 CLI 应用中的错误类型。

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

    #[error("密码读取失败: {0}")]
    PasswordReadFailed(String),

    #[error("用户取消了密码输入: {0}")]
    PasswordCancelled(String),

    #[error("密码派生密钥初始化失败: {0}")]
    KeyDerivationFailed(String),

    #[error("I/O 错误: {0}")]
    IoError(#[from] std::io::Error),

    #[error("JSON 序列化失败: {0}")]
    JsonError(#[from] serde_json::Error),

    #[error("chat_core 错误: {0}")]
    CoreError(#[from] chat_core::error::CoreError),

    #[error("存储错误: {0}")]
    StorageError(#[from] chat_core::error::StorageError),
}

/// chat_cli 的便捷 Result 类型别名
pub type CliResult<T> = Result<T, CliError>;
