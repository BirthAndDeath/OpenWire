# Multi-Device Sync Architecture / 多设备同步消息交换架构

> Status: Implemented (steps 1-5) / 状态：已实现（第 1-5 步）
> Scope: 账号级多设备（一个 ML-DSA 身份 = 多台设备）的消息同步、密钥分发、启动引导
> / 范围：账号级多设备（一个 ML-DSA 身份 = 多台设备）的消息同步、密钥分发、启动引导

## 1. 信任模型 / Trust Model

**信任边界 = 账号（ML-DSA 身份）。** 同一账号的所有设备完全互信，共享账号级 ML-DSA 身份。
消息 E2EE 目标是「账号」，因此每台设备都能解密看到明文。设备间全程加密，但明文落在任一设备内存/本地库均属正常。

/ Trust boundary = account (ML-DSA identity). All devices of one account fully trust each other and share the account ML-DSA identity. Message E2EE targets the account, so every device can decrypt and read plaintext. Transport is encrypted end-to-end; plaintext existing on any own device is normal.

## 2. 拓扑 / Topology

**全互连网格（full mesh）**，非星型：同账号设备两两直连（直连或经中继），任一设备可直接与任何其他设备通信，无单一 hub（避免单点失效与转发热点）。

/ Full mesh, not star: all devices of an account connect pairwise (direct or via relay). Any device can talk to any other; no single hub (no SPOF, no forwarding hotspot).

## 3. 身份与密钥分发（DHT 只存指纹，本体走 netevent）/ Identity & Key Distribution

### 3.1 DHT：只存 SHA256 指纹，防数据过大 / DHT stores fingerprints only

- provider key = `SHA256(ML-DSA 公钥 hex)`（已实现 `swarm_ops.rs::dht_key`）
- provider 集合 = 该账号当前在线设备的 PeerID（指针，非密钥本体）
- **ML-KEM / ML-DSA 公钥本体绝不入 DHT**——避免 DHT 存放大密钥、暴露全账号设备指纹、被未授权方批量拉取
- 已实现：`publish_identity_to_dht` 只 `start_providing(dht_key)`；`get_providers` 返回 provider（PeerID）

/ DHT stores fingerprints only. provider key = SHA256(ML-DSA pubkey hex). provider set = online device PeerIDs (pointers). ML-KEM / ML-DSA key material never goes to DHT.

### 3.2 netevent：点对点传本体 / netevent carries key material P2P

- `FriendOnline`（rr_netevent 协议）点对点携带：ML-DSA 公钥 hex + PeerID + listen_addrs + **当前会话 ML-KEM 公钥** + ML-DSA 签名（已实现）
- 好友流程：
  1. `GetProviders(dht_key(账号公钥))` → 得到该账号全部在线设备 PeerID
  2. 逐设备拨号 → 连接建立时/主动请求 `FriendOnline` → 各设备用自己的 ML-KEM 签名响应
  3. 好友缓存 **账号级 ML-KEM 表**：`账号公钥 → [(设备PeerID, ML-KEM公钥, sha256指纹)]`（`DhtCache.pubkey_devices` + `PeerCache.peerid_to_mlkem`）
- 防投毒：FriendOnline 的 ML-DSA 签名证明「该设备确实属于该账号」，非签名 PeerInfo 不落加密映射（已实现原则）

/ FriendOnline carries key material P2P with ML-DSA signature, proving device ownership of the account. Friend caches account-level ML-KEM table keyed by account pubkey.

## 4. 消息投递（方案A：发送侧逐设备扇出）/ Delivery: sender-side per-device fan-out

**发送方**解析目标账号全部在线设备（`DhtCache.list_pubkey_devices`），对 ≥2 台设备时：
- 用**各设备各自的 ML-KEM** 加密**同一明文**；
- 用**同一发送方签名**构建消息（`ChatMessage.sender_public_key = 原作者`，身份不变）；
- 携带**同一 `DeviceRouting.origin_hash`**（跨副本稳定去重键）；
- 逐设备经 rr_msg 发送。

**接收方**每设备用自己的 ML-KEM 私钥解出明文；`routing.origin_hash` 已见过则丢弃（重传/重复副本去重），否则按原流程处理（sender = 原作者）。

单设备/旧客户端：目标账号仅 1 台在线设备时走原路径（`routing=None`），完全兼容。

/ Sender enumerates the target account's online devices; for ≥2 devices it encrypts the same plaintext with each device's ML-KEM, signs once (identity unchanged), carries the same origin_hash, and sends per device. Receiver dedups by origin_hash and processes normally.

## 5. 指纹失效检测与再同步 / Fingerprint staleness & re-sync

复用已实现的 `ChatMessage.sender_mlkem_sha256`（发送方当前 ML-KEM 指纹，纳入签名哈希）：

1. 接收方比对「缓存中发送方设备的 ML-KEM 指纹」vs 消息附带指纹
2. **命中** → 密钥仍有效，继续处理（零开销）
3. **未命中** → 发送方会话密钥已轮换 → 回发 FriendOnline 完成双向密钥交换
4. **持续失配（30s 窗口内多次）** → 按骚扰处理：按账号（pubkey）计数限流，停止回发 FriendOnline；窗口过期自动恢复（日志降级 debug，防日志洪泛）

/ Staleness: compare cached device ML-KEM fingerprint vs attached fingerprint. Hit = continue. Miss = re-exchange FriendOnline bidirectionally. Persistent mismatch = account-level rate-limited as abuse (stop replying; auto-recover after window; debug-level logs).

## 6. 启动引导时序 / Startup bootstrap order

```
启动 → publish_current_identity_to_dht()（本设备注册为 provider）
     → GetProviders(dht_key(自己公钥))        # 查询自己账号下所有设备 PeerID
     → 与各设备 FriendOnline 互认              # 确认哪些设备在线
     → 若有 ≥1 设备在线：
         从在线设备增量同步（消息 seq > 本地游标 / 联系人 / 路由）
         跳过联系人全量 DHT 发现
     → 若无任何设备在线（账号冷启动）：
         discover_all_contacts()               # 现有全量发现
```

挂载点：`event_loop.rs::P2pEvent::BootstrapReady` → `discover_own_devices()`；`TimerDiscoverAllContacts` 兜底（自检超时回退全量发现）。
/ Mount point: BootstrapReady → discover_own_devices(); TimerDiscoverAllContacts fallback.

## 7. 增量同步与去重 / Incremental sync & dedup

- **发送侧扇出已覆盖"在线设备同步"**：发送方直接把消息投递给目标账号每台在线设备，无需设备间转发（方案A，相比转发重签更简单且身份正确）。
- **去重**：`DeviceRouting.origin_hash` 由真源设备一次生成（`SHA256(源账号公钥‖msgtype‖seq‖明文)`，seq 单调），所有扇出副本共享同一值；接收端 `device_seen_seqs` 按 origin_hash 去重（上限 4096，满清空）。
- **防环**：`hop` 上限 8（兼容旧版转发消息，`MAX_DEVICE_ROUTING_HOP`）。
- 离线补拉（设备上线后从在线设备拉缺失消息）：**尚未实现**，留作后续扩展。

/ Sender-side fan-out covers online-device sync; no device-to-device re-encryption needed. Dedup by origin_hash (monotonic seq). hop cap 8 for legacy forward messages. Offline pull (incremental since_seq) is not yet implemented.

## 8. 消息格式扩展（兼容）/ Wire extension (backward compatible)

```rust
pub struct ChatMessage {
    // ...现有字段...
    #[serde(default)]
    pub routing: Option<DeviceRouting>,   // None = 单设备/旧客户端，照旧
}

pub struct DeviceRouting {
    pub seq: u64,                 // 账号内单调序号（源设备生成）
    pub source_device: String,    // 源设备 PeerID（Base58）
    pub target_identity: String,  // 目标账号 ML-DSA 公钥 hex
    pub hop: u8,                  // 转发跳数（防环）
    #[serde(default)]
    pub origin_hash: String,      // 真源设备生成的稳定去重键（跨副本）
}
```

全部字段纳入 `compute_hash` 签名（防篡改）。单设备场景 `routing=None` 完全照旧；新旧客户端混用时旧端忽略 `routing`，仍可收发。
/ All routing fields covered by signature hash; single-device & mixed-version safe.

## 9. 实施状态 / Implementation status

| # | 内容 | 状态 |
|---|---|---|
| 1 | `DhtCache` 多设备映射（`pubkey_devices` + `list_pubkey_devices`，保留单值兼容） | ✅ |
| 2 | `DeviceRouting` + `ChatMessage.routing`（含 `origin_hash`，兼容、纳入签名） | ✅ |
| 3 | 启动引导 `discover_own_devices()`（有设备在线跳过全量发现 + 超时兜底） | ✅ |
| 4 | 账号级 ML-KEM 查找（`lookup_mlkem_pubkey` 遍历设备） + 指纹未命中再同步 | ✅ |
| 5 | 发送侧逐设备扇出 + 接收端 origin_hash 去重 + hop 防环 | ✅ |
| 安全 | 骚扰限流（按账号计数、防绕过、日志降级） | ✅ |
| 未来 | 离线补拉（增量同步 since_seq） | ⏳ 未实现 |

验证：`cargo check` / `clippy` 零告警、23 项测试通过。
/ Verification: cargo check, clippy clean, 23 tests pass.
