use std::path::PathBuf;
/// 核心配置：初始化参数集合
pub struct CoreConfig {
    /// SQLite 数据库路径，示例: "/path/to/database.db"
    pub database_path: PathBuf,
    /// 日志文件路径，None 表示标准输出
    pub path_to_log: Option<PathBuf>,
    /// 日志级别，如 "info", "debug", "warn"
    pub log_level: Option<String>,
}

impl CoreConfig {
    /// 创建配置实例
    ///
    /// # 参数
    /// - `database_path`: 数据库文件路径
    /// - `rx_cmd`: 命令接收通道，用于外部控制核心（发送消息、关闭等）
    /// - `path_to_log`: 可选日志文件路径
    /// - `log_level`: 可选日志级别
    pub fn new(
        database_path: impl Into<PathBuf>,

        path_to_log: Option<impl Into<PathBuf>>,
        log_level: Option<impl Into<String>>,
    ) -> Self {
        if path_to_log.is_none() {
            return Self {
                database_path: database_path.into(),

                path_to_log: None,
                log_level: None,
            };
        }

        Self {
            database_path: database_path.into(),

            path_to_log: Some(path_to_log.unwrap().into()),
            log_level: log_level.map(|s| s.into()),
        }
    }
}
