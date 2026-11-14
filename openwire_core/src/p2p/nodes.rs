//! 节点配置管理
//!
//! 管理 relay 中继节点和 bootstrap 引导节点的配置。
//! 配置存储在 `data_dir/nodes.json` 文件中，支持动态加载和默认配置生成。

use serde::{Deserialize, Serialize};
use std::path::Path;

/// 节点配置
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NodesConfig {
    /// Relay 中继节点列表：[(PeerId, Multiaddr)]
    #[serde(default)]
    pub relay_nodes: Vec<[String; 2]>,
    /// Bootstrap 引导节点列表：[(PeerId, Multiaddr)]
    #[serde(default)]
    pub bootstrap_nodes: Vec<[String; 2]>,
}

impl NodesConfig {
    /// 节点配置文件名
    const NODES_CONFIG_FILE: &'static str = "nodes.json";

    /// 从 data_dir 加载节点配置
    ///
    /// 如果文件不存在，创建默认配置并返回。
    /// 如果文件存在但解析失败，记录警告并返回默认配置。
    pub fn load(data_dir: &Path) -> Self {
        let config_path = data_dir.join(Self::NODES_CONFIG_FILE);

        if !config_path.exists() {
            tracing::info!(
                "节点配置文件不存在，创建默认配置: {:?}",
                config_path
            );
            let default_config = NodesConfig::default();
            if default_config.relay_nodes.is_empty() && default_config.bootstrap_nodes.is_empty() {
                // 默认配置为空（无内置公共节点）：全新安装将无引导。
                // 须由用户配置 nodes.json / save_nodes_config，否则 DHT 发现不可用。
                tracing::warn!(
                    "默认节点配置为空：未配置 relay/bootstrap 节点，DHT 发现将不可用。请通过设置页配置节点或写入 nodes.json"
                );
            }
            if let Err(e) = default_config.save(data_dir) {
                tracing::warn!("保存默认节点配置失败: {}", e);
            }
            return default_config;
        }

        match std::fs::read_to_string(&config_path) {
            Ok(content) => match Self::from_json_str(&content) {
                Ok(config) => {
                    tracing::info!(
                        "已加载节点配置: {} relay 节点, {} bootstrap 节点",
                        config.relay_nodes.len(),
                        config.bootstrap_nodes.len()
                    );
                    config
                }
                Err(e) => {
                    tracing::warn!(
                        "解析节点配置文件失败: {}，使用默认配置",
                        e
                    );
                    NodesConfig::default()
                }
            },
            Err(e) => {
                tracing::warn!("读取节点配置文件失败: {}，使用默认配置", e);
                NodesConfig::default()
            }
        }
    }

    /// 保存节点配置到 data_dir
    pub fn save(&self, data_dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
        let config_path = data_dir.join(Self::NODES_CONFIG_FILE);
        let content = self.to_json_pretty();
        std::fs::write(&config_path, content)?;
        tracing::debug!("节点配置已保存: {:?}", config_path);
        Ok(())
    }

    /// 重置为默认节点配置并保存
    pub fn reset_to_default(data_dir: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        let default_config = NodesConfig::default();
        default_config.save(data_dir)?;
        tracing::info!("节点配置已重置为默认值");
        Ok(default_config)
    }

    /// 将 NodesConfig 序列化为美化 JSON 字符串
    fn to_json_pretty(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".to_string())
    }

    /// 将 NodesConfig 序列化为紧凑 JSON 字符串（用于前端 API 返回）
    pub fn to_json_string(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{}".to_string())
    }

    /// 解析 JSON 字符串为 NodesConfig
    fn from_json_str(content: &str) -> Result<Self, String> {
        serde_json::from_str(content).map_err(|e| format!("JSON 解析失败: {e}"))
    }
}
