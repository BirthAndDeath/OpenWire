# PeerID 持久化与端口偏好方案评估

## 现状分析

### 当前行为

| 组件 | 当前行为 | 代码位置 |
| ------ | ---------- | ---------- |
| Ed25519 密钥对 | 每次启动 `generate_ed25519()` 随机生成 | `identity.rs:10` |
| PeerID | 由 Ed25519 公钥派生，每次启动不同 | `identity.rs:10→libp2p` |
| TCP 端口 | `Tcp(0)` → OS 自动分配 | `swarm.rs:216` |
| QUIC 端口 | `Udp(0)` → OS 自动分配 | `swarm.rs:200` |
| WebSocket 端口 | `Tcp(0)` → OS 自动分配 | `swarm.rs:231` |

### 重启后网络拓扑变化

```
启动 A:  PeerID=A1,  listen=/ip4/0.0.0.0/tcp/45001
重新启动:  PeerID=A2,  listen=/ip4/0.0.0.0/tcp/45002
```

- DHT 路由表中旧条目 `A1@45001` 变成孤儿，直到 2h TTL 过期
- 中继节点的 reservation 需重新建立
- 所有已连接的 Peer 收到 `ConnectionClosed(A1)` → 需要重新发现
- DHT 中 `pubkey→PeerID` 映射需要重新发布
- mDNS 发现到新 PeerID，但无法关联到之前的用户

---

## 方案评估

### 设计要点

```

temp_peerid.json
{
  "ed25519_private_key": "hex...",
  "preferred_ports": {
    "tcp": 45001,
    "quic": 45001,
    "ws": 45002
  },
  "created_at": "2026-08-02T19:00:00Z",
  "ttl": 3600
}
```

1. 启动时读取 `temp_peerid.json`，若存在且未超时 → 复用
2. 端口先尝试偏好值，被占用或超时 → 释放并轮换
3. 超时后重新生成 Ed25519 + 端口

### 对网络拓扑稳定性的改善

| 场景 | 改善前 | 改善后 | 效果 |
| ------ | -------- | -------- | ------ |
| 快速重启（<1h） | PeerID 变化，全部连接断开 | PeerID 不变，端口不变 | 直连重连，无需中继 |
| 重启后 DHT 路由 | 路由表条目 A1 3h 后过期，新条目 A2 需重新 bootstrap | 路由表条目不变 | bootstrap 加速 |
| 中继 reservation | 每次重启需重新 reservation | 中继允许复用旧 reservation（如果端口不变） | 减少 2-3 轮中继握手 |
| mDNS 发现 | 对端看到新 PeerID，无法关联到联系人 | 对端看到相同 PeerID，可直接匹配 | 加速局域网发现 |
| DCUtR 打洞 | 端口变化后需重新打洞 | 端口稳定，打洞结果可复用 | 减少打洞延迟 |
| 消息重试 | 新 PeerID 无法匹配 old pending 消息 | 相同 PeerID，在线后立即投递 | 减少消息丢失 |

### 安全风险

| 风险 | 严重度 | 概率 | 说明 | 缓解措施 |
| ------ | -------- | ------ | ------ | ---------- |
| **设备指纹追踪** | 中 | 高 | 固定 PeerID 可在不同 session 间关联同一设备 | TTL 轮换；不存储到持久化存储（仅 tmpfs） |
| **DHT 关联攻击** | 中 | 中 | 观察者看到 pubkey→PeerID 映射不变，可关联同一用户的不同 session | TTL 轮换限制窗口期 |
| **端口扫描** | 低 | 低 | 固定端口使扫描更精准 | 端口仅偏好，被占用则跳转 |
| **临时文件泄漏** | 高 | 低 | Ed25519 私钥写入磁盘，可能被其他进程读取 | 使用 `RUSTFLAGS` 或 OS 级别权限保护 |
| **重放攻击** | 低 | 低 | 旧 PeerID 可能被冒充 | 与 ML-DSA 签名结合验证身份 |

### 实现复杂度

| 模块 | 改动量 | 复杂度 |
| ------ | -------- | -------- |
| 新增 `PeerIdStore` | ~80 行 | 低 |
| 修改 `generate_temporary_peerid` | ~10 行 | 低 |
| 修改 `swarm.rs` 端口分配 | ~30 行 | 中 |
| 修改 `identity_ops.rs` 和 `core/mod.rs` | ~30 行 | 低 |
| 新增 `peerid.json` 生命周期管理 | ~50 行 | 中 |
| **总计** | **~200 行** | **低~中** |

---

## 实现建议

### 核心结构

```rust
// src/peerid_store.rs
pub struct PeerIdConfig {
    ed25519_bytes: [u8; 32],
    preferred_ports: PreferredPorts,
    created_at: SystemTime,
    ttl: Duration,
}

pub struct PreferredPorts {
    pub tcp: u16,
    pub quic: u16,
    pub ws: u16,
}

impl PeerIdConfig {
    const TTL: Duration = Duration::from_secs(3600); // 1h
    const FILE_NAME: &str = "peerid.json";

    pub fn load_or_create(data_dir: &Path) -> Self {
        let path = data_dir.join(Self::FILE_NAME);
        if let Ok(config) = Self::load_from(&path) {
            if config.created_at.elapsed() < config.ttl {
                return config;
            }
            tracing::info!("PeerId TTL expired, rotating");
        }
        let config = Self::create_new();
        config.save_to(&path).ok();
        config
    }

    fn create_new() -> Self {
        let keypair = identity::Keypair::generate_ed25519();
        let mut ed25519_bytes = [0u8; 32];
        ed25519_bytes.copy_from_slice(keypair.encode()[..32]);
        let ports = PreferredPorts {
            tcp: pick_available_port(0),  // 偏好 OS 分配的端口
            quic: pick_available_port(0),
            ws: pick_available_port(0),
        };
        Self { ed25519_bytes, preferred_ports: ports, ... }
    }
}
```

### 端口分配策略

```rust
// 修改 swarm.rs
fn try_listen_on_preferred(
    swarm: &mut Swarm<MyBehaviour>,
    preferred: &PreferredPorts,
) -> P2pResult {
    let tcp_port = preferred.tcp;
    // 先尝试偏好端口
    let result = swarm.listen_on(
        Multiaddr::empty()
            .with(Protocol::from(Ipv4Addr::UNSPECIFIED))
            .with(Protocol::Tcp(tcp_port)),
    );
    if result.is_ok() {
        return Ok(());  // 偏好端口可用
    }
    // 被占用则回退到 OS 分配
    tracing::warn!("Port {} occupied, falling back to OS-assigned", tcp_port);
    swarm.listen_on(
        Multiaddr::empty()
            .with(Protocol::from(Ipv4Addr::UNSPECIFIED))
            .with(Protocol::Tcp(0)),
    )
}
```

### 生命周期管理

```
┌─────────────┐     ┌─────────────┐     ┌─────────────┐
│ 启动        │────>│ 读取        │────>│ 未超时      │
│             │     │ peerid.json │     │ → 复用      │
└─────────────┘     └─────────────┘     └──────┬──────┘
                                               │ 超时
                                               v
                                        ┌─────────────┐
                                        │ 重新生成     │
                                        │ Ed25519+端口 │
                                        │ 写入文件     │
                                        └─────────────┘
```

---

## 结论

| 维度 | 评估 |
| ------ | ------ |
| 网络拓扑稳定性 | **显著改善**。快速重启后 PeerID 不变，DHT 路由表、中继 reservation、mDNS 缓存均可用。对 1h 内的反复重启效果最明显。 |
| 安全风险 | **可控**。TTL 轮换限制了追踪窗口；临时文件使用 `600` 权限保护；PeerID 仅是路由标识，不包含用户身份信息。 |
| 实现复杂度 | **低**。~200 行新代码，不涉及现有数据结构和协议变更。 |
| 兼容性 | **向后兼容**。PeerID 持久化是客户端本地行为，不影响协议层。 |

### 推荐：实施

理由：

1. 当前每次重启获得新 PeerID 的收益（隐私）远小于代价（网络拓扑断裂）
2. 用户身份已经由 ML-DSA 签名密钥保证（持久化在 rootcell），PeerID 仅作为路由层标识
3. TTL 轮换机制在隐私和稳定性之间取得平衡
4. 改动量小，风险可控

### 不推荐但可选的替代方案

| 方案 | 代价 | 收益 |
| ------ | ------ | ------ |
| **完全持久化**（无 TTL） | 隐私损失更大 | 拓扑稳定性最高 |
| **仅持久化端口，不持久化 PeerID** | 实现复杂度相近 | 仅改善地址稳定性，无法改善路由表 |
| **不做任何变更** | 当前碎片化拓扑 | 隐私保护最大化 |
