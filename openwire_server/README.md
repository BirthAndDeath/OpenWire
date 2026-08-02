# OpenWire Relay Server

OpenWire 自带的 Circuit Relay v2 中继服务端，用于 NAT 穿透。部署在公网后，NAT 后的客户端可通过此中继建立连接，并通过 DCUtR 升级为直连。

## 编译

```bash
# 编译 release 版本
cargo build --release -p openwire-server-cli

# 产物位于 target/release/ 目录
# Linux/macOS: ./target/release/openwire-server-cli
# Windows:     .\target\release\openwire-server-cli.exe
```

## 运行

```bash
# 默认端口 44909，数据目录 .openwire-relay
RUST_LOG=info ./target/release/openwire-server-cli

# 指定数据目录和端口
RUST_LOG=info ./target/release/openwire-server-cli /path/to/data 44909
```

`RUST_LOG` 可选值：`info`（默认）、`debug`、`warn`、`error`。

首次运行自动生成 `nodes.json` 和 Ed25519 密钥对（`<data_dir>/ed25519.bin`），控制台输出 `PeerId=<peer_id>`。

默认监听地址：

| 协议 | 地址 |
|------|------|
| TCP IPv4 | `/ip4/0.0.0.0/tcp/44909` |
| TCP IPv6 | `/ip6/::/tcp/44909` |
| QUIC IPv4 | `/ip4/0.0.0.0/udp/44909/quic-v1` |
| QUIC IPv6 | `/ip6/::/udp/44909/quic-v1` |

## 防火墙配置

### Ubuntu (ufw)

```bash
sudo ufw allow 44909/tcp
sudo ufw allow 44909/udp
sudo ufw reload
```

### CentOS/RHEL (firewall-cmd)

```bash
sudo firewall-cmd --permanent --add-port=44909/tcp
sudo firewall-cmd --permanent --add-port=44909/udp
sudo firewall-cmd --reload
```

### Windows Defender

```powershell
New-NetFirewallRule -DisplayName "OpenWire Relay TCP" -Direction Inbound -Protocol TCP -LocalPort 44909 -Action Allow
New-NetFirewallRule -DisplayName "OpenWire Relay UDP" -Direction Inbound -Protocol UDP -LocalPort 44909 -Action Allow
```

### 云服务商安全组

- AWS EC2 / 阿里云 / 腾讯云 / 所有 VPS 面板：放行 44909 TCP + UDP

## 前置条件

- 公网 IP（非 NAT 后）。NAT 后的中继失去意义。
- 端口 44909 TCP/UDP 未被占用。

## systemd 服务（Linux）

```ini
[Unit]
Description=OpenWire Relay Server
After=network.target

[Service]
Type=simple
User=openwire
ExecStart=/usr/local/bin/openwire-server-cli /var/lib/openwire-relay 44909
Restart=on-failure
RestartSec=30
Environment=RUST_LOG=info

[Install]
WantedBy=multi-user.target
```

```bash
sudo systemctl enable openwire-relay
sudo systemctl start openwire-relay
```

## 功能

- **Circuit Relay v2**: 为 NAT 后客户端提供 relay 中继服务
- **DHT 节点**: 参与 Kademlia DHT 网络，提供路由查询
- **FriendOnline 缓存**: 缓存客户端 FriendOnline 通知，支持 DiscoverPeer 协议
- **DHT 注册**: 启动后向 DHT 注册为中继节点（key: `relay_nodes_public`）

## 配置

`nodes.json` 位于数据目录下，首次运行自动生成。服务端仅使用 `bootstrap_nodes` 字段加入 DHT 网络：

```json
{
  "relay_nodes": [
    ["<relay_peer_id>", "<relay_multiaddr>"]
  ],
  "bootstrap_nodes": [
    ["<bootstrap_peer_id>", "<bootstrap_multiaddr>"]
  ]
}
```

## 客户端配置

客户端 `nodes.json` 的 `relay_nodes` 中添加本中继的 PeerId 和地址：

```json
{
  "relay_nodes": [
    ["<relay_server_peer_id>", "/ip4/<relay_server_ip>/tcp/44909"]
  ]
}
```

## 验证

服务端日志显示 reservation：

```
relay reservation from <client_peer_id>
```

客户端日志显示：

```
=== RELAY READY: reservation accepted by relay <relay_server_peer_id> ===
```