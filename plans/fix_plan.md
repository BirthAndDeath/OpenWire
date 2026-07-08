# 修复计划

## P1: send_text_impl 首次发送标记 hash 不一致

**文件**: `openwire_core/src/core/message_ops.rs`

**问题**: `mark_sent_by_hash` 使用 `hash_input` 匹配消息，但 DeliveryReceipt 使用 `ChatMessage.hash` 查找。首次发送后数据库中存储的 `message_hash` 是 `hash_input`，而发送后分发的 `ChatMessage.hash` 不同，导致后续送达回执无法匹配已标记的消息。

**修复方案**:
1. 首次发送后，不立即调用 `mark_sent_by_hash`，而是保持 pending 状态
2. 在 `send_message`（发送网络消息）之后，立即更新数据库中的 `message_hash` 为 `ChatMessage.hash`
3. 然后调用 `mark_sent_by_hash` 使用正确的 `ChatMessage.hash` 标记

具体改动:
- `send_text_impl`: 移除 `if !is_retry { mark_sent_by_hash() }`
- 在 `send_message` 返回 `ChatMessage` 后，获取 `ChatMessage.hash`，调用 `update_message_hash` 更新数据库，再调用 `mark_sent_by_hash` 用正确的 hash 标记

---

## P2: 连接计数 underflow 风险

**文件**: `openwire_core/src/core/event_loop.rs`

**问题**: `ConnectionClosed` 使用 `*entry.get_mut() -= 1`，同一连接可能触发多次 `ConnectionClosed`，导致计数减到 0 以下（usize underflow 到 `usize::MAX`），使 peer 永远被认为在线。

**修复方案**:
- 使用 `saturating_sub(1)` 替代 `-= 1`

---

## P3: try_send 静默丢弃关键命令

**文件**: 多处文件（`event_loop.rs`、`contact_ops.rs`、`identity_ops.rs` 等）

**问题**: 所有 `send().await` 改为 `tx.try_send()` 且使用 `let _ = ...` 忽略错误。当通道满时，`SaveRoutingTable`、`Shutdown`、`Dial` 等关键命令静默丢失。

**修复方案**:
- 将 `SaveRoutingTable`、`Shutdown`、`Dial`、`DialAddr`、`SendNetEvent` 等关键命令的 `try_send` 改为带 `warn!` 日志的模式：
  ```rust
  if let Err(e) = self.p2p_handle.tx.try_send(cmd) {
      tracing::warn!("P2p command dropped: channel full: {e:?}");
  }
  ```
- 非关键命令（如 `GetProviders`、`PublishIdentity`）可保持静默丢弃，通道满说明事件循环堆积了

---

## P4: 中继重试指数退避

**文件**: `openwire_core/src/actor/p2p/mod.rs`

**问题**: 中继连接失败后只有 30 秒固定冷却时间，`ExpiredListenAddr` 处理绕过冷却检查，中继永久离线时无限循环。

**修复方案**:
1. 添加 `relay_reconnect_attempt: u32` 字段到 `P2pActor`
2. `dial_relay_nodes` 中失败后：冷却时间 = min(30 * 2^attempt, 3600) 秒
3. `ExpiredListenAddr` 处理器中也检查冷却时间
4. 成功连接后重置 `relay_reconnect_attempt = 0`

---

## P5: save_path 路径安全检查

**文件**: `openwire_core/src/core/file_transfer.rs`

**问题**: `save_path` 来自 UI 层用户输入，未验证是否在允许的下载目录下，存在任意路径写入风险。

**修复方案**:
1. 规范化 `save_path`（`fs::canonicalize`）
2. 验证规范化后的路径是否以 `download_dir` 规范化路径为前缀
3. 如果不在允许范围内，回退到 `download_dir` 并记录 `warn!`

---

## P6: Relay 地址重复 /p2p/ 协议段

**文件**: `openwire_core/src/actor/p2p/mod.rs`

**问题**: `dial_relay_nodes` 中 `with_p2p` 在地址已包含 `/p2p/` 时会失败，回退到无 PeerId 的地址，dial 可能失败。

**修复方案**:
- 在 `with_p2p` 之前检查地址是否已包含 `/p2p/` 协议段：
  ```rust
  let full_addr = if addr.iter().any(|p| matches!(p, Protocol::P2p)) {
      addr.clone()
  } else {
      addr.clone().with_p2p(peer_id).unwrap_or(addr.clone())
  };
  ```

---

## P7: DHT 缓存达到上限时记录日志

**文件**: `openwire_core/src/p2p/dht_cache.rs`

**问题**: 达到 `MAX_PUBKEY_PEERID` 等上限时静默跳过新插入，新身份映射永久丢失且无提示。

**修复方案**:
- 每个插入点在跳过时添加 `tracing::warn!`：
  ```rust
  tracing::warn!("DHT cache at capacity (max={}), skipping insert for {}", MAX_PUBKEY_PEERID, pubkey_hex);
  ```

---

## 实施顺序建议

1. **P2**（连接计数 underflow）— 单行改动，安全修复
2. **P1**（hash 不一致）— 核心正确性修复，影响消息送达确认
3. **P6**（relay 地址 /p2p/ 去重）— 小改动，提高连通率
4. **P3**（try_send 日志）— 增强可观测性
5. **P4**（指数退避）— 可靠性改进
6. **P5**（路径安全）— 安全加固
7. **P7**（缓存上限日志）— 低优先级