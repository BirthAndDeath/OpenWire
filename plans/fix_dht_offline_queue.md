# DHT 离线队列故障修复计划

## 故障根因

**实际日志**：

```
[日志] 消息已保存到离线队列（联系人 3942906c815fd2a6 当前不在线）
[警告] 发送消息失败: 联系人 3942906c815fd2a6 当前不在线，消息已保存到离线队列
```

**根因**：发送方在 `send_text()` 中查询接收方 PeerID 时，本地 DHT 缓存未命中，网络 Kademlia `get_record` 查询超时（30 秒），导致消息被错误地放入离线队列，即使接收方实际在线。

**深层原因**：

1. DHT 注册循环每 5 分钟执行一次（`DHT_REGISTRATION_INTERVAL_SECS = 300`），接收方刚上线时其 PeerID 记录尚未发布到 DHT 网络
2. 发送方本地 DHT 缓存每 30 分钟清理一次，清理后需要重新网络查询
3. 只有 2 个节点的 Kademlia 网络中，DHT 记录复制和查询不稳定

---

## 修复方案

### 修复 1：启动后立即执行 DHT 身份发布（高优先级）

**文件**：[`chat_core/src/core/event_loop.rs`](../chat_core/src/core/event_loop.rs)

**问题**：`run_inner()` 中调用 `spawn_dht_registration()` 启动的 DHT 注册循环需要等待 5 分钟才执行首次 `publish_identity_to_dht()`。

**修复**：在 `run_inner()` 中，启动 DHT 注册循环后，**立即**通过命令通道发送一次 `DhtPublishIdentity` 命令。

**具体修改**：

在 [`event_loop.rs:48-52`](../chat_core/src/core/event_loop.rs:48) 的 `run_inner()` 中，在 `spawn_dht_registration()` 之后添加：

```rust
// 启动 DHT 定期注册任务
let dht_reg_cmd_tx = self.core_handle.cmd_tx.clone();
self.spawn_dht_registration(rt_handle);

// === 修复：启动后立即执行一次 DHT 身份发布 ===
// 避免等待 5 分钟后的首次定时注册
if let (Some(pubkey), Some(pid)) = (self.mldsa_pubkey_hex.clone(), self.current_peer_id) {
    let mlkem = self.mlkem_pubkey_hex.clone().unwrap_or_default();
    // 更新本地 DHT 数据库
    if let Ok(store) = self.get_dht_store() {
        let _ = store.set_pubkey_peerid(&pubkey, &pid);
        if !mlkem.is_empty() {
            let _ = store.set_mlkem_pubkey(&pubkey, &mlkem);
        }
    }
    // 发送 DHT 发布命令
    let _ = dht_reg_cmd_tx.try_send(ChatCommand::DhtPublishIdentity {
        mldsa_pubkey_hex: pubkey.clone(),
        peer_id: pid.to_string(),
        mlkem_pubkey_hex: mlkem,
    });
    tracing::info!("启动后立即发布身份到 DHT 网络");
}
```

---

### 修复 2：连接建立后主动发布身份（高优先级）

**文件**：[`chat_core/src/p2p/events.rs`](../chat_core/src/p2p/events.rs)

**问题**：`ConnectionEstablished` 事件处理中只触发了 `RetryPendingMessages`，但没有发布当前身份到 DHT。当接收方连接到发送方时，发送方的 PeerID 记录可能尚未在 DHT 中更新。

**修复**：在 `ConnectionEstablished` 事件处理中，增加 DHT 身份发布。

**具体修改**：

在 [`events.rs:147-156`](../chat_core/src/p2p/events.rs:147) 的 `ConnectionEstablished` 分支中，在 `RetryPendingMessages` 之后添加：

```rust
// === 修复：连接建立后主动发布身份到 DHT ===
// 确保对方能通过 DHT 查询到我们的最新 PeerID
if let (Some(pubkey), Some(pid)) = (core.mldsa_pubkey_hex.clone(), core.current_peer_id) {
    let mlkem = core.mlkem_pubkey_hex.clone().unwrap_or_default();
    let _ = core.core_handle.cmd_tx.try_send(ChatCommand::DhtPublishIdentity {
        mldsa_pubkey_hex: pubkey,
        peer_id: pid.to_string(),
        mlkem_pubkey_hex: mlkem,
    });
}
```

---

### 修复 3：DHT 查询失败后尝试直接连接再重试（高优先级）

**文件**：[`chat_core/src/core/command_handler.rs`](../chat_core/src/core/command_handler.rs)

**问题**：`send_text()` 中 DHT 网络查询返回 `Ok(None)`（未找到）时，直接进入离线队列，没有尝试通过已建立的连接直接发送。

**修复**：在 DHT 网络查询返回 `Ok(None)` 时，不立即进入离线队列，而是：

1. 尝试通过已连接的 peers 列表判断接收方是否在线
2. 如果接收方在线但 DHT 查询失败，尝试通过 `discover_contact` 重新发现

**具体修改**：

在 [`command_handler.rs:157-169`](../chat_core/src/core/command_handler.rs:157) 的 `Ok(None)` 分支中，修改为：

```rust
Ok(None) => {
    // 网络 DHT 查询未找到，检查是否已与接收方建立连接
    // 如果已连接，说明接收方在线但 DHT 记录尚未传播
    tracing::warn!(
        "DHT 网络查询未找到联系人 {} 的 PeerID，尝试通过已建立连接发送",
        &mldsa_pubkey_hex[..16]
    );
    
    // 尝试通过 discover_contact 重新发现（会再次查询 DHT）
    match self.discover_contact(mldsa_pubkey_hex, None).await {
        Ok(Some(peer_id)) => {
            tracing::info!(
                "通过 discover_contact 成功获取联系人 {} 的 PeerID: {}",
                &mldsa_pubkey_hex[..16],
                peer_id
            );
            peer_id
        }
        _ => {
            // 仍然失败，保存到离线队列
            tracing::info!(
                "联系人 {} 当前不在线，消息将保存到离线队列",
                &mldsa_pubkey_hex[..16]
            );
            self.save_pending_message(mldsa_pubkey_hex, msgtype, &data)
                .await;
            return Err(anyhow::anyhow!(
                "联系人 {} 当前不在线，消息已保存到离线队列",
                &mldsa_pubkey_hex[..16]
            ));
        }
    }
}
```

---

### 修复 4：增强离线消息重试机制（中优先级）

**文件**：[`chat_core/src/core/event_loop.rs`](../chat_core/src/core/event_loop.rs)

**问题**：当前重试仅在 `ConnectionEstablished` 时触发。如果连接已建立但 DHT 查询失败，消息会进入离线队列且不会自动重试。

**修复**：在 `run_inner()` 的主循环中增加定期重试定时器（每 30 秒）。

**具体修改**：

在 [`event_loop.rs:54-89`](../chat_core/src/core/event_loop.rs:54) 的 `run_inner()` 主循环中，增加一个 `tokio::time::interval` 分支：

```rust
// 主事件循环：处理网络事件和控制命令
let mut retry_interval = tokio::time::interval(std::time::Duration::from_secs(30));
retry_interval.tick().await; // 跳过首次立即触发

loop {
    tokio::select! {
        event = self.swarm.select_next_some() => {
            p2p::swarm_event(event, self).await;
        }
        Some(cmd) = self.rx_cmd.recv() => {
            // ... 现有代码 ...
        }
        _ = retry_interval.tick() => {
            // 定期重试待发送消息
            self.handle_command(ChatCommand::RetryPendingMessages).await;
        }
        else => break,
    }
}
```

---

### 修复 5：DHT 查询超时后自动重试（中优先级）

**文件**：[`chat_core/src/p2p/mod.rs`](../chat_core/src/p2p/mod.rs)

**问题**：`lookup_peerid_by_pubkey_network()` 超时 30 秒后直接返回 `Ok(None)`，没有重试机制。

**修复**：增加一次自动重试，使用更短的超时时间。

**具体修改**：

在 [`p2p/mod.rs:172-206`](../chat_core/src/p2p/mod.rs:172) 的超时处理中，增加重试逻辑：

```rust
// 3. 等待结果（超时 30 秒）
match tokio::time::timeout(std::time::Duration::from_secs(30), rx).await {
    // ... 现有代码 ...
    Err(_) => {
        // 超时 - 自动重试一次
        tracing::warn!(
            "DHT network lookup: timeout for pubkey {}, retrying once...",
            &pubkey_hex[..16]
        );
        // 清理旧回调
        dht_query_callbacks().lock().unwrap().remove(pubkey_hex);
        
        // 重试
        let rx2 = register_dht_query_callback(pubkey_hex.to_string());
        let query_id2 = swarm.behaviour_mut().kademlia.get_record(key.clone());
        
        match tokio::time::timeout(std::time::Duration::from_secs(15), rx2).await {
            Ok(Ok(Some(peer_id))) => {
                tracing::info!(
                    "DHT network retry: found PeerID {} for pubkey {}",
                    peer_id,
                    &pubkey_hex[..16]
                );
                Ok(Some(peer_id))
            }
            _ => {
                tracing::warn!(
                    "DHT network lookup: retry also failed for pubkey {}",
                    &pubkey_hex[..16]
                );
                dht_query_callbacks().lock().unwrap().remove(pubkey_hex);
                Ok(None)
            }
        }
    }
}
```

---

## 修改文件清单

| # | 文件 | 修改内容 | 优先级 |
|---|------|---------|--------|
| 1 | [`chat_core/src/core/event_loop.rs`](../chat_core/src/core/event_loop.rs) | `run_inner()` 启动后立即 DHT 发布 | 高 |
| 2 | [`chat_core/src/p2p/events.rs`](../chat_core/src/p2p/events.rs) | `ConnectionEstablished` 时 DHT 发布 | 高 |
| 3 | [`chat_core/src/core/command_handler.rs`](../chat_core/src/core/command_handler.rs) | DHT 查询失败后尝试 `discover_contact` 再进离线队列 | 高 |
| 4 | [`chat_core/src/core/event_loop.rs`](../chat_core/src/core/event_loop.rs) | 主循环增加 30 秒定期重试定时器 | 中 |
| 5 | [`chat_core/src/p2p/mod.rs`](../chat_core/src/p2p/mod.rs) | DHT 查询超时自动重试一次 | 中 |

---

## 验证方法

1. **单元测试**：验证 `send_text()` 在 DHT 查询失败后不会立即进入离线队列
2. **集成测试**：启动两个节点，加好友后立即发送消息，确认消息能正常到达
3. **日志验证**：确认以下日志不再出现：
   - `"消息已保存到离线队列（联系人 ... 当前不在线）"`
   - `"发送消息失败: 联系人 ... 当前不在线"`
4. **回归测试**：确认离线消息功能仍然正常（接收方真正离线时，消息应进入离线队列）
