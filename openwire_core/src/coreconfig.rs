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
    /// 用户密码派生密钥 hex（前端用 Argon2id 处理后的 256 位密钥）
    /// 用于 Keyring 不可用时的降级加密文件存储
    pub passwd: Option<String>,
    /// Relay 中继节点列表：[(PeerId, Multiaddr)]
    /// 用于 NAT 穿透，节点会尝试通过这些 relay 建立中继连接。
    /// 如果为空，则不使用 relay。
    pub relay_nodes: Vec<(String, String)>,
    /// Bootstrap 引导节点列表：[(PeerId, Multiaddr)]
    /// 用于 Kademlia DHT 网络引导。
    /// 如果为空，使用默认的 IPFS bootstrap 节点。
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
                download_dir: None,
                passwd: None,
                relay_nodes: Vec::new(),
                bootstrap_nodes: Vec::new(),
            };
        }

        Self {
            data_dir: data_dir.into(),

            path_to_log: Some(path_to_log.unwrap().into()),
            log_level: log_level.map(|s| s.into()),
            download_dir: None,
            passwd: None,
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
