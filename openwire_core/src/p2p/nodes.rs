//! 节点配置管理
//!
//! 管理 relay 中继节点和 bootstrap 引导节点的配置。
//! 配置存储在 `data_dir/nodes.json` 文件中，支持动态加载和默认配置生成。
//!
//! JSON 序列化/反序列化使用 serde_json（openwire_core 的已有依赖）。
//! NodesConfig 使用 serde 的 Serialize/Deserialize derive 自动生成实现。

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
            // ============================================================
            // OpenWire 公共中继节点
            // 客户端通过中继节点进行 NAT 穿透（Circuit Relay v2）。
            // 每个中继节点可同时提供 TCP 和 QUIC 两种传输方式。
            // ============================================================
            relay_nodes: vec![
                [
                    "12D3KooWNHL5yLssLwToZG4iuuMkviVdQ1JCnxXnxgDvbk86Jk6P".to_string(),
                    "/ip4/206.237.12.198/tcp/44909".to_string(),
                ],
                [
                    "12D3KooWNHL5yLssLwToZG4iuuMkviVdQ1JCnxXnxgDvbk86Jk6P".to_string(),
                    "/ip4/206.237.12.198/udp/44909/quic-v1".to_string(),
                ],
            ],
            // ============================================================
            // DHT Bootstrap 引导节点
            //
            // 包含两类节点：
            // 1. OpenWire 中继节点（/chat/kad/0.0.1 协议）
            // 2. IPFS 公共引导节点（/ipfs/kad/1.0.0 协议，协议不兼容但无害）
            //
            // IPFS 节点虽然 Kademlia 协议不同，但可提供：
            // - 路由表桶划分（Kademlia 需要桶中有节点才能正常分裂）
            // - Identify 地址交换
            // - 底层 libp2p 连接（ping、连接保活）
            // - 部分节点可能支持 relay 协议
            //
            // 来源: https://github.com/ipfs/kubo
            // ============================================================
            bootstrap_nodes: vec![
                // OpenWire 中继 bootstrap
                [
                    "12D3KooWNHL5yLssLwToZG4iuuMkviVdQ1JCnxXnxgDvbk86Jk6P".to_string(),
                    "/ip4/206.237.12.198/tcp/44909".to_string(),
                ],
                // IPFS 官方 bootstrap 节点（dnsaddr 方式）
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
                // IPFS 基金会额外 bootstrap 节点（直连 IP 地址）
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
