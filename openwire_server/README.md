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

## .deb 打包（Linux）

使用 [cargo-deb](https://crates.io/crates/cargo-deb) 构建 Debian 包，安装后自动创建 `openwire` 用户、数据目录，并注册 systemd 服务：

```bash
# 构建（需在 Linux 环境）
cargo install cargo-deb
cargo deb -p openwire-server-cli

# 安装
sudo dpkg -i target/debian/openwire-server-cli_*.deb

# 服务已自动启用并启动
sudo systemctl status openwire-relay
```

### 包内容

| 路径 | 说明 |
|------|------|
| `/usr/bin/openwire-server-cli` | 服务端二进制 |
| `/etc/systemd/system/openwire-relay.service` | systemd 服务单元 |
| `/var/lib/openwire-relay/` | 数据目录（Ed25519 密钥、DHT 数据库、nodes.json） |

### 维护脚本行为

- **安装**：创建 `openwire` 系统用户，生成默认 `nodes.json`，`systemctl enable + start` 服务
- **升级**：不删除数据目录，服务自动重启，密钥和路由表保留
- **卸载（purge）**：停止并禁用服务，删除数据目录和用户

### 手动 systemd 配置（不使用 .deb）

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
# 手动创建用户和数据目录
sudo useradd --system --home /var/lib/openwire-relay --shell /usr/sbin/nologin openwire
sudo mkdir -p /var/lib/openwire-relay
sudo chown openwire:openwire /var/lib/openwire-relay
sudo systemctl enable openwire-relay
sudo systemctl start openwire-relay
```

### 常用命令

```bash
sudo systemctl status openwire-relay      # 查看状态
sudo journalctl -u openwire-relay -f      # 查看日志
sudo systemctl restart openwire-relay     # 重启
sudo systemctl stop openwire-relay        # 停止
```

## 功能

- **Circuit Relay v2**: 为 NAT 后客户端提供 relay 中继服务
- **DHT 节点**: 参与 Kademlia DHT 网络，提供路由查询
- **FriendOnline 缓存**: 缓存客户端 FriendOnline 通知，支持 DiscoverPeer 协议
- **DHT 注册**: 启动后向 DHT 注册为中继节点（key: `relay_nodes_public`）

## 数据存储路径 / Data Storage Paths

服务器将以下文件存储在数据目录（默认 `.openwire-relay`，可通过命令行第一个参数指定）中：

| 文件 / File | 说明 / Description |
|-------------|-------------------|
| `<data_dir>/ed25519.bin` | Ed25519 身份密钥对（0600 权限），用于生成服务器 PeerId / Ed25519 identity keypair (0600 perms), derives the server PeerId |
| `<data_dir>/dht.redb` | 持久化 Kademlia 路由表与 DHT 记录（Redb 数据库，重启后路由表不丢失）/ Persistent Kademlia routing table & DHT records (Redb database, survives restarts) |
| `<data_dir>/nodes.json` | 节点配置（仅 `bootstrap_nodes` 字段被使用）/ Node config (only `bootstrap_nodes` is used) |
| `<data_dir>/relay-info.json` | 中继信息：PeerId 与带 `/p2p/` 后缀的监听地址，可直接复制到客户端 `nodes.json` / Relay info: PeerId and listen addresses with `/p2p/` suffix, copy-paste ready for client `nodes.json` |

`.deb` 安装版使用固定目录 `/var/lib/openwire-relay/`。

The server stores all state in the data directory (default `.openwire-relay`, overridable via the first CLI arg); the `.deb` package uses the fixed `/var/lib/openwire-relay/`.

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