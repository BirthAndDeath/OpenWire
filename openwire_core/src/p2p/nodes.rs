//! 节点配置管理
//!
//! 管理 relay 中继节点和 bootstrap 引导节点的配置。
//! 配置存储在 `data_dir/nodes.json` 文件中，支持动态加载和默认配置生成。
//!
//! # 设计说明
//! 为避免引入 serde_json 等序列化依赖，JSON 的读写使用手动实现。
//! NodesConfig 使用 serde 的 Serialize/Deserialize derive 用于其他用途（如 postcard），
//! JSON 的序列化/反序列化由本模块手动处理。

use serde::{Deserialize, Serialize};
use std::path::Path;

/// 节点配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodesConfig {
    /// Relay 中继节点列表：[(PeerId, Multiaddr)]
    #[serde(default)]
    pub relay_nodes: Vec<[String; 2]>,
    /// Bootstrap 引导节点列表：[(PeerId, Multiaddr)]
    #[serde(default)]
    pub bootstrap_nodes: Vec<[String; 2]>,
}

impl Default for NodesConfig {
    fn default() -> Self {
        Self {
            // Relay 节点列表默认为空。
            // 节点启动后通过 DHT 引导 + Identify 协议发现网络中的 relay 节点，
            // 用户也可通过设置界面（get_nodes_config/save_nodes_config）手动添加。
            relay_nodes: vec![],
            bootstrap_nodes: vec![
                // ============================================================
                // IPFS 官方 bootstrap 节点（dnsaddr 方式）
                // 来源: https://github.com/ipfs/kubo
                // ============================================================
                [
                    "QmNnooDu7bfjPFoTZYxMNLWUQJyrVwtbZg5gBMjTezGAJN".to_string(),
                    "/dnsaddr/sv15.bootstrap.libp2p.io".to_string(),
                ],
                [
                    "QmQCU2EcMqAqQPR2i9bChDtGNJchTbq5TbXJJ16u19uLTa".to_string(),
                    "/dnsaddr/ny5.bootstrap.libp2p.io".to_string(),
                ],
                [
                    "QmbLHAnMoJPWSCR5Zhtx6BHJX9KiKNN6tpvbUcqanj75Nb".to_string(),
                    "/dnsaddr/am6.bootstrap.libp2p.io".to_string(),
                ],
                [
                    "QmcZf59bWwK5XFi76CZX8cbJ4BhTzzA3gU1ZjYZcYW3dwt".to_string(),
                    "/dnsaddr/sg1.bootstrap.libp2p.io".to_string(),
                ],
                // ============================================================
                // IPFS 基金会额外 bootstrap 节点（直连 IP 地址）
                // ============================================================
                [
                    "QmaCpDMGvV2BGHeYERUEnRQAwe3N8SzbUtfsmvsqQLuvuJ".to_string(),
                    "/ip4/104.131.131.82/tcp/4001".to_string(),
                ],
                [
                    "QmSoLnSGccFuZQJzRadHn95W2CrSFmZuTdDWP8HXaHca9z".to_string(),
                    "/ip4/104.236.176.52/tcp/4001".to_string(),
                ],
                [
                    "QmSoLPppuBtQSGwKDZT2M73ULpjvfd3aZ6ha4oFGL1KrGM".to_string(),
                    "/ip4/104.236.179.241/tcp/4001".to_string(),
                ],
                [
                    "QmSoLueR4xBeUbY9WZ9xGUUxunbKWcrNFTDAadQJmocnWm".to_string(),
                    "/ip4/162.243.248.213/tcp/4001".to_string(),
                ],
                [
                    "QmSoLSafTMBsPKadTEgaXctDQVcqN88CNLHXMkTNwMKPnu".to_string(),
                    "/ip4/128.199.219.111/tcp/4001".to_string(),
                ],
                [
                    "QmSoLV4Bbm51jM9C4gDYZQ9Cy3U6aXMJDAbzgu2fzaDs64".to_string(),
                    "/ip4/104.236.76.40/tcp/4001".to_string(),
                ],
                [
                    "QmSoLer265NRgSp2LA3dPaeykiS1J6DifTC88f5uVQKNAd".to_string(),
                    "/ip4/178.62.158.247/tcp/4001".to_string(),
                ],
                [
                    "QmSoLMeWqB7YGVLJN3pNLQpmmEk35v6wYtsMGLzSr5QBU3".to_string(),
                    "/ip4/178.62.61.185/tcp/4001".to_string(),
                ],
                [
                    "QmSoLju6m7xTh3DuokvT3886QRYqxAzb1kShaanJgW36yx".to_string(),
                    "/ip4/104.236.151.122/tcp/4001".to_string(),
                ],
            ],
        }
    }
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

    /// 手动将 NodesConfig 序列化为 JSON 字符串（不依赖 serde_json）
    fn to_json_pretty(&self) -> String {
        let mut json = String::from("{\n");

        // relay_nodes
        json.push_str("  \"relay_nodes\": [\n");
        for (i, node) in self.relay_nodes.iter().enumerate() {
            json.push_str(&format!("    [{:?}, {:?}]", node[0], node[1]));
            if i < self.relay_nodes.len().saturating_sub(1) {
                json.push(',');
            }
            json.push('\n');
        }
        json.push_str("  ],\n");

        // bootstrap_nodes
        json.push_str("  \"bootstrap_nodes\": [\n");
        for (i, node) in self.bootstrap_nodes.iter().enumerate() {
            json.push_str(&format!("    [{:?}, {:?}]", node[0], node[1]));
            if i < self.bootstrap_nodes.len().saturating_sub(1) {
                json.push(',');
            }
            json.push('\n');
        }
        json.push_str("  ]\n");
        json.push('}');
        json
    }

    /// 将 NodesConfig 序列化为紧凑 JSON 字符串（用于前端 API 返回）
    pub fn to_json_string(&self) -> String {
        let mut json = String::from("{");

        // relay_nodes
        json.push_str("\"relay_nodes\":[");
        for (i, node) in self.relay_nodes.iter().enumerate() {
            json.push_str(&format!("[\"{}\",\"{}\"]", node[0], node[1]));
            if i < self.relay_nodes.len().saturating_sub(1) {
                json.push(',');
            }
        }
        json.push_str("],");

        // bootstrap_nodes
        json.push_str("\"bootstrap_nodes\":[");
        for (i, node) in self.bootstrap_nodes.iter().enumerate() {
            json.push_str(&format!("[\"{}\",\"{}\"]", node[0], node[1]));
            if i < self.bootstrap_nodes.len().saturating_sub(1) {
                json.push(',');
            }
        }
        json.push_str("]");

        json.push('}');
        json
    }


    /// 手动解析 JSON 字符串为 NodesConfig（不依赖 serde_json）
    fn from_json_str(content: &str) -> Result<Self, String> {
        let content = content.trim();

        // 简单校验：必须以 { 开头，以 } 结尾
        if !content.starts_with('{') || !content.ends_with('}') {
            return Err("JSON 必须是一个对象".to_string());
        }

        let inner = content[1..content.len() - 1].trim();

        let mut relay_nodes: Vec<[String; 2]> = Vec::new();
        let mut bootstrap_nodes: Vec<[String; 2]> = Vec::new();

        // 按顶层 key 分割
        // 查找 "relay_nodes" 和 "bootstrap_nodes" 键
        if let Some(arr_str) = extract_json_array(inner, "relay_nodes") {
            relay_nodes = parse_node_array(&arr_str)?;
        }
        if let Some(arr_str) = extract_json_array(inner, "bootstrap_nodes") {
            bootstrap_nodes = parse_node_array(&arr_str)?;
        }

        Ok(NodesConfig {
            relay_nodes,
            bootstrap_nodes,
        })
    }
}

/// 从 JSON 对象字符串中提取指定 key 的数组内容（包括方括号）
fn extract_json_array<'a>(json_obj: &'a str, key: &str) -> Option<String> {
    // 查找 "key": 模式
    let search = &format!("\"{}\"", key);
    let key_start = json_obj.find(search)?;
    let after_key = &json_obj[key_start + search.len()..];

    // 跳过空白和冒号
    let after_colon = after_key.trim_start();
    let after_colon = after_colon.strip_prefix(':')?;
    let after_colon = after_colon.trim_start();

    // 找到匹配的方括号
    if after_colon.starts_with('[') {
        let mut depth = 0;
        let mut end = 0;
        for (i, ch) in after_colon.char_indices() {
            match ch {
                '[' => depth += 1,
                ']' => {
                    depth -= 1;
                    if depth == 0 {
                        end = i + 1;
                        break;
                    }
                }
                _ => {}
            }
        }
        if end > 0 {
            Some(after_colon[..end].to_string())
        } else {
            None
        }
    } else {
        None
    }
}

/// 解析节点数组字符串，如 [["peer1", "/addr1"], ["peer2", "/addr2"]]
fn parse_node_array(arr_str: &str) -> Result<Vec<[String; 2]>, String> {
    let inner = arr_str.trim();
    if !inner.starts_with('[') || !inner.ends_with(']') {
        return Err("节点配置必须是数组".to_string());
    }

    let inner = inner[1..inner.len() - 1].trim();
    if inner.is_empty() {
        return Ok(Vec::new());
    }

    let mut nodes = Vec::new();
    let mut depth = 0;
    let mut start = 0;

    for (i, ch) in inner.char_indices() {
        match ch {
            '[' => {
                if depth == 0 {
                    start = i;
                }
                depth += 1;
            }
            ']' => {
                depth -= 1;
                if depth == 0 {
                    let sub = &inner[start..=i];
                    if let Some(node) = parse_single_node(sub)? {
                        nodes.push(node);
                    }
                }
            }
            _ => {}
        }
    }

    Ok(nodes)
}

/// 解析单个节点 ["peer_id", "multiaddr"]
fn parse_single_node(s: &str) -> Result<Option<[String; 2]>, String> {
    let s = s.trim();
    if !s.starts_with('[') || !s.ends_with(']') {
        return Err("节点必须是数组格式 [\"peer_id\", \"multiaddr\"]".to_string());
    }

    let inner = s[1..s.len() - 1].trim();
    if inner.is_empty() {
        return Ok(None);
    }

    // 提取两个引号字符串
    let parts: Vec<&str> = inner.splitn(2, ',')
        .map(|p| p.trim())
        .filter(|p| !p.is_empty())
        .collect();

    if parts.len() != 2 {
        return Err(format!("节点数组必须包含两个元素，实际: {}", parts.len()));
    }

    let peer_id = extract_quoted_string(parts[0])?;
    let multiaddr = extract_quoted_string(parts[1])?;

    Ok(Some([peer_id, multiaddr]))
}

/// 提取引号字符串（去掉首尾引号）
fn extract_quoted_string(s: &str) -> Result<String, String> {
    let s = s.trim();
    if s.len() < 2 || !s.starts_with('"') || !s.ends_with('"') {
        return Err(format!("期望字符串，实际: {}", s));
    }
    // 处理转义字符
    let inner = &s[1..s.len() - 1];
    Ok(inner.to_string())
}
