# OpenWire Relay Server

OpenWire 的中继服务器（libp2p relay + DHT bootstrap + 目录服务），供客户端 NAT 穿透、
DHT 引导与 FriendOnline 身份缓存使用。**运行即公网暴露**，部署前请阅读本节。

## ⚠️ 开放中继的滥用风险

- 默认配置下，**任何能完成 libp2p 握手的节点**都能向本服务器发起 reservation / circuit：
  其本质是全开放中继。可能被滥用为：
  - **匿名流量中转/带宽吸血**：陌生人借你的机器与带宽代传任意流量（本中继不限制流量内容；
    仅按电路限定字节）。
  - **资源占用**：连接/中继槽位占满（已有 `connection_limits` 与中继配额兜底，但仍会消耗带宽）。
  - **好友名单枚举**：`/chat/rr_netevent` 的 `DiscoverPeer` 目录查询对任意节点开放，
    可查询 pubkey→PeerID 映射（仅暴露在线状态维度，不暴露联系人名单本身）。
- 这不是缺陷，而是公共基础设施的设计取舍；**运行方须知情并自行加固**。

## 推荐加固（部署时）

1. **Peer 白名单（首选）**：libp2p 中继协议不提供 peer ACL，请在系统层做：
   - 防火墙/云安全组按源 PeerID 或源 IP 白名单放行；
   - 仅允许你的客户节点 IP/CIDR 访问中继端口。
2. **收紧资源配额**（环境变量，默认即原硬编码值）：
   - `OPENWIRE_RELAY_MAX_CIRCUITS`（默认 50）
   - `OPENWIRE_RELAY_MAX_CIRCUITS_PER_PEER`（默认 5）
   - `OPENWIRE_RELAY_MAX_RESERVATIONS`（默认 50）
   - `OPENWIRE_RELAY_MAX_RESERVATIONS_PER_PEER`（默认 5）
   - `OPENWIRE_RELAY_MAX_CIRCUIT_BYTES`（默认 104857600，100MB）
   - `OPENWIRE_RELAY_MAX_CIRCUIT_DURATION_SECS`（默认 3600）
   - `OPENWIRE_RELAY_RESERVATION_DURATION_SECS`（默认 7200）
3. **监控**：定时读取 `relay-info.json` 监听地址与端口，关注异常连接数；
   记录中继服务器的带宽用量，异常时配合白名单关闭。

## 运行

```bash
openwire-server-cli [data_dir] [port]
```

默认端口 44909（成功后持久化到 `port-preference.txt`）。中继 PeerID/地址写入
`relay-info.json`，复制到客户端 `nodes.json` 的 `relay_nodes` 即可。

## 数据与密钥

- `ed25519.bin`：中继身份私钥（unix 0600 / windows 独占）。**泄漏=中继身份被冒用**，请妥善保管。
- `dht.redb`：Kademlia 持久化存储（DDOS 下 provider 记录会增长；内置每小时过期清理）。
- `port-preference.txt`：上次使用的监听端口。