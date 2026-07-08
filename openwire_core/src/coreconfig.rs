use std::path::PathBuf;

/// 默认 WebSocket 信令服务器（Cloudflare Workers）
/// 海外用户可直接使用，国内用户请设 `signaling_server = None`
pub const DEFAULT_SIGNALING_SERVER: &str = "openwire-server.3589206993.workers.dev";

/// 核心配置：初始化参数集合
#[derive(Default, Clone)]
pub struct CoreConfig {
    pub data_dir: PathBuf,
    pub path_to_log: Option<PathBuf>,
    pub log_level: Option<String>,
    pub download_dir: Option<PathBuf>,
    pub relay_nodes: Vec<(String, String)>,
    pub bootstrap_nodes: Vec<(String, String)>,
    /// WebSocket 信令服务器主机，如 "openwire-server.3589206993.workers.dev"
    /// 设为 `None` 可禁用信令功能（国内用户建议禁用）
    pub signaling_server: Option<String>,
    /// 信令房间名（可选），不设置则用 ML-DSA 公钥前 16 字符
    pub signaling_room: Option<String>,
}

impl CoreConfig {
    /// 创建配置实例
    ///
    /// # 参数
    /// - `data_dir`: 数据目录路径
    /// - `path_to_log`: 可选日志文件路径
    /// - `log_level`: 可选日志级别
    ///
    /// 注意：relay_nodes 和 bootstrap_nodes 默认为空，
    /// 调用方应通过 `load_nodes_config()` 或直接设置字段来填充。
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
                relay_nodes: Vec::new(),
                bootstrap_nodes: Vec::new(),
                signaling_server: Some(DEFAULT_SIGNALING_SERVER.to_string()),
                signaling_room: None,
            };
        }

        Self {
            data_dir: data_dir.into(),
            path_to_log: Some(path_to_log.unwrap().into()),
            log_level: log_level.map(|s| s.into()),
            download_dir: None,
            relay_nodes: Vec::new(),
            bootstrap_nodes: Vec::new(),
            signaling_server: Some(DEFAULT_SIGNALING_SERVER.to_string()),
            signaling_room: None,
        }
    }

    /// 从 data_dir 加载节点配置（nodes.json）
    ///
    /// 填充 relay_nodes 和 bootstrap_nodes 字段。
    /// 如果文件不存在，会自动创建默认配置。
    pub fn load_nodes_config(&mut self) {
        let nodes_config = crate::p2p::nodes::NodesConfig::load(&self.data_dir);
        self.relay_nodes = nodes_config
            .relay_nodes
            .into_iter()
            .map(|a| (a[0].clone(), a[1].clone()))
            .collect();
        self.bootstrap_nodes = nodes_config
            .bootstrap_nodes
            .into_iter()
            .map(|a| (a[0].clone(), a[1].clone()))
            .collect();
    }
}
