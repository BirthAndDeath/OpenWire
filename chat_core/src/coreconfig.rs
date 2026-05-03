use std::path::PathBuf;
/// 核心配置：初始化参数集合
#[derive(Default, Clone)]
pub struct CoreConfig {
    /// 数据目录路径，示例: "/path/to/data"
    pub data_dir: PathBuf,
    /// 日志文件路径，None 表示标准输出
    pub path_to_log: Option<PathBuf>,
    /// 日志级别，如 "info", "debug", "warn"
    pub log_level: Option<String>,
    /// 文件下载目录（默认: data_dir/downloads）
    pub download_dir: Option<PathBuf>,
}

impl CoreConfig {
    /// 创建配置实例
    ///
    /// # 参数
    /// - `data_dir`: 数据目录路径
    /// - `rx_cmd`: 命令接收通道，用于外部控制核心（发送消息、关闭等）
    /// - `path_to_log`: 可选日志文件路径
    /// - `log_level`: 可选日志级别
    pub fn new(
        data_dir: impl Into<PathBuf>,

        path_to_log: Option<impl Into<PathBuf>>,
        log_level: Option<impl Into<String>>,
    ) -> Self {
        if path_to_log.is_none() {
            return Self {
                data_dir: data_dir.into(),

                path_to_log: None,
                log_level: None,
                download_dir: None,
            };
        }

        Self {
            data_dir: data_dir.into(),

            path_to_log: Some(path_to_log.unwrap().into()),
            log_level: log_level.map(|s| s.into()),
            download_dir: None,
        }
    }
}
