use std::path::PathBuf;

/// 核心配置：初始化参数集合
#[derive(Default, Clone)]
pub struct CoreConfig {
    /// 数据目录路径
    pub data_dir: PathBuf,
    /// 日志文件路径（可选）
    pub path_to_log: Option<PathBuf>,
    /// 日志级别（可选）
    pub log_level: Option<String>,
    /// 中继节点列表 [(PeerId, Multiaddr)]
    pub relay_nodes: Vec<(String, String)>,
    /// 引导节点列表 [(PeerId, Multiaddr)]
    pub bootstrap_nodes: Vec<(String, String)>,
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
                relay_nodes: Vec::new(),
                bootstrap_nodes: Vec::new(),
            };
        }

        let safe_path_to_log = path_to_log.map(Into::into);
        Self {
            data_dir: data_dir.into(),
            path_to_log: safe_path_to_log,
            log_level: log_level.map(|s| s.into()),
            relay_nodes: Vec::new(),
            bootstrap_nodes: Vec::new(),
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
