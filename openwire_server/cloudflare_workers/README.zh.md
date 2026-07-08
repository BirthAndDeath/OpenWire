# OpenWire 服务端 — Cloudflare Workers

[English](README.md) | [中文](README.zh.md)

为 [OpenWire](https://github.com/BirthAndDeath/OpenWire) P2P 聊天应用设计的轻量协调服务，完全运行在 Cloudflare 边缘网络上。

## 功能一览

| 服务 | 说明 | 免费套餐 |
|------|------|----------|
| **中继注册表** | 公网中继节点通过心跳注册；NAT 客户端通过 HTTP 发现 | ✅ |
| **引导节点发现** | 用于初始 DHT 加入的权威引导节点列表 | ✅ |
| **名称 → 公钥映射** | 可读名称到 ML-DSA 公钥的映射 | ✅ |
| **WebSocket 信令** | 房间式 PeerId + 地址交换，用于 NAT 穿透 | ✅ |
| **nodes.json** | `GET /api/nodes.json` 输出匹配 `NodesConfig`，可直接写入 `data_dir/` | ✅ |

## 快速部署

```bash
npx wrangler login
cd openwire_server/cloudflare_workers
npx wrangler deploy
```

然后在 Cloudflare 控制台 Workers → openwire-server → Durable Objects 中启用 Durable Objects。

## OpenWire 如何使用

### 引导节点和中继（手动）

获取一次数据并写入 `data_dir/nodes.json`：

```bash
curl -s https://你的-worker.example/api/nodes.json > /path/to/data_dir/nodes.json
```

OpenWire 下次启动时通过 `CoreConfig::load_nodes_config()` → `NodesConfig::load()` 读取。

### 名称解析（姓名 → 公钥）

```bash
# 注册
curl -X PUT https://你的-worker.example/api/presence/alice \
  -H 'Content-Type: application/json' \
  -d '{"pubkey":"<mldsa_公钥_hex>"}'

# 查询
curl https://你的-worker.example/api/presence/alice

# 删除
curl -X DELETE https://你的-worker.example/api/presence/alice
```

### 信令连接（WebSocket NAT 穿透）

通过 `CoreConfig` 配置即可，无需手动处理 WS：

```rust
cfg.signaling_server = Some("你的-worker.example".into());
cfg.signaling_room = Some("my-room".into());  // 可选，默认用 ML-DSA 公钥前 16 字符
```

启动时 `ChatCore::try_init()` 自动创建 `SignalingActor`，其行为：

1. 连接 `wss://你的-worker.example/api/signal/my-room`
2. 发送 `{"type":"register","peer_id":"...","addrs":["...","..."]}`
3. 接收其他节点的 `{"type":"peer","peer_id":"...","addrs":["...","..."]}`
4. 自动调用 `P2pCommand::DialAddr` 拨号

### 信令协议（JSON over WebSocket）

| 方向 | 消息 | 说明 |
|------|------|------|
| 客户端 → | `{"type":"register","peer_id":"...","addrs":["..."]}` | 宣布上线 |
| 服务器 → | `{"type":"peer","peer_id":"...","addrs":["..."]}` | 另一节点上线 |
| 服务器 → | `{"type":"peer_left","peer_id":"..."}` | 节点离线 |
| 客户端 ↔ | `{"type":"signal","target":"...","data":"..."}` | 定向信令 |

不中转数据 — 只交换地址。真实的 libp2p 连接（noise 握手 + yamux）直接在节点间发生。

## 架构

- **Durable Objects**：中继注册表一个单例，每个名称一个单例，每个信令房一个单例（按房间名按需创建）。
- **心跳淘汰**：中继节点每 ~60s 发心跳；超过 120s 无心跳自动移除。
- **零 npm 依赖**：纯 Workers 运行时 API，无需安装任何包。
- **中继节点需要 VPS**：本服务只提供*发现*，不提供中继协议。在 VPS 上部署专用 OpenWire 中继后通过 `POST /api/relays` 注册。

## API 参考

| 方法 | 路径 | 说明 |
|------|------|------|
| `GET` | `/` | 服务信息 |
| `GET` | `/api` | API 索引 |
| `GET` | `/api/nodes.json` | 完整节点配置（中继 + 引导） |
| `GET` | `/api/bootstrap` | 列出引导节点 |
| `GET` | `/api/relays` | 列出活跃中继节点 |
| `POST` | `/api/relays` | 注册为中继（`{"id":"..","addr":".."}`） |
| `POST` | `/api/relays/ping` | 中继心跳（`{"id":".."}`） |
| `WS` | `/api/signal/:room` | WebSocket 信令房间（NAT 穿透） |
| `GET` | `/api/presence/:name` | 名称 → 公钥 查询 |
| `PUT` | `/api/presence/:name` | 设置名称 → 公钥（`{"pubkey":".."}`） |
| `DELETE` | `/api/presence/:name` | 删除映射 |

## 文件结构

```
openwire_server/cloudflare_workers/
├── wrangler.toml       # Workers 配置 + DO 绑定
├── package.json
├── src/
│   ├── worker.mjs      # HTTP 入口 + 路由
│   ├── registry.mjs    # DO: 中继心跳注册表
│   ├── presence.mjs    # DO: 名称→公钥 KV
│   └── signaling.mjs   # DO: WebSocket 信令房间

openwire_core/src/actor/signaling/
└── mod.rs              # SignalingActor (Rust WS 客户端)
```

## 部署检查清单

1. `npx wrangler deploy` — 上传 worker
2. 在控制台启用 Durable Objects
3. （可选）在应用配置中设置 `signaling_server` 和 `signaling_room`
4. （可选）在 VPS 部署中继节点，通过 `POST /api/relays` 注册
5. （可选）编辑 `DEFAULT_BOOTSTRAP` 填充 bootstrap 节点