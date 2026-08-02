# 依赖版本升级与适配计划

## 当前状态

### libp2p

| 项目 | 版本 |
|------|------|
| libp2p (crates.io) | **0.56.0**（最新，无 0.57.0 发布） |
| rust-libp2p master | 同 0.56.0 基线 + 若干修复提交 |
| 当前使用的补丁 | mDNS 缓冲区 4096→9216（`patches/libp2p-mdns`） |
|  | request-response assertion 移除（`patches/libp2p-request-response`） |
| 项目 MSRV | 1.83.0（libp2p 0.56.0 要求） |
| master MSRV | 1.88.0（2026-03-31 提升） |

### aws-lc-rs（ML-DSA 后量子签名）

| 项目 | 版本 |
|------|------|
| aws-lc-rs (crates.io) | **1.17.3**（最新） |
| ML-DSA 状态 | 🔴 仍处于 `unstable` feature gate 下 |
| 依赖路径 | `openwire_core → aws-lc-rs`（features = `["unstable", "prebuilt-nasm"]`） |
| 使用位置 | `signature.rs`、`identity.rs` 中的 `ML_DSA_65_SIGNING`、`PqdsaKeyPair` |
| 上游状态 | `src/unstable/signature.rs` 暴露，`src/pqdsa/` 为 `pub(crate)` 内部模块 |

---

## 方案对比

### 方案 A：保持现状（当前补丁方案）✅ 推荐

继续使用 `[patch.crates-io]` 维护两个补丁，等待上游 0.57.0 正式发布。

**工作量**：0 人日
**风险**：补丁侵入性极低（mDNS: 1 行改缓冲区大小，request-response: 1 行改 `debug_assert_eq` → `if`）
**问题**：补丁与上游版本绑定，上游发版后需验证补丁是否仍适用

### 方案 B：切换到 master 分支 git 引用

```toml
libp2p = { git = "https://github.com/libp2p/rust-libp2p", rev = "cdabcd0..." }
```

**优点**：包含两处修复（request-response + 其他），无需维护补丁
**缺点**：

| 障碍 | 影响 | 修复难度 |
|------|------|----------|
| MSRV 1.83.0 → 1.88.0 | 需要 Rust 1.88.0+ | 低（升级 rustup） |
| Edition 2024（master 已切） | 与当前 workspace 的 edition 2024 一致，无影响 | 无 |
| Swarm API 可能变更 | `Transport redesign` commit (079b2d6) 可能改变 transport 构建 API | 需要验证 |
| `NetworkBehaviour` 可能变更 | `ConnectionClosed` 事件结构变化 | 需要验证 |
| 编译时间 | git 依赖每次都重新编译，不缓存 | 中 |
| 稳定性 | master 分支可能引入新问题 | 中 |

**工作量**：1-2 人日（依赖验证 + 可能的 API 适配）
**风险**：中（master API 不稳定）

### 方案 C：等待 0.57.0 正式发布

**优点**：零风险，上游正式发布，有 changelog 和 migration guide
**缺点**：发布时间未知（当前 0.56.0 已发布 1 年+，尚无 0.57.0 计划）

**工作量**：发布后 0.5 人日
**风险**：无

---

## 依赖冲突排查

### 已知冲突：`signature` crate

当前 `signature` 版本冲突（`didcomm` 通过 `askar-crypto` 需要 `signature >=1.3, <1.4`，`did-key` 通过 `p256` 需要 `signature >=1.5, <1.7`）已通过移除 `didcomm` 依赖解决。`libp2p` 升级不涉及此问题。

### libp2p 内部依赖（Cargo.lock 当前版本）

| 子 crate | 当前 | master 预计 | 兼容性 |
|----------|------|-------------|--------|
| libp2p-core | 0.43.2 | 0.44.x | breaking |
| libp2p-swarm | 0.47.1 | 0.48.x | breaking |
| libp2p-identity | 0.2.14 | 0.2.x | 兼容 |
| libp2p-kad | 0.48.0 | 0.49.x | breaking |
| libp2p-mdns | 0.48.0 | 0.49.x | 更新 API |
| libp2p-request-response | 0.29.0 | 0.30.x | 更新 API |
| libp2p-relay | 0.21.1 | 0.22.x | breaking |
| libp2p-tcp | 0.44.1 | 0.45.x | 兼容 |
| libp2p-noise | 0.46.1 | 0.47.x | 兼容 |
| libp2p-yamux | 0.47.0 | 0.48.x | 兼容 |
| libp2p-quic | 0.13.1 | 0.14.x | 兼容 |
| libp2p-websocket | 0.45.1 | 0.46.x | 兼容 |
| libp2p-dns | 0.44.0 | 0.45.x | 兼容 |
| libp2p-identify | 0.47.0 | 0.48.x | breaking |
| libp2p-ping | 0.47.0 | 0.48.x | 兼容 |
| libp2p-dcutr | 0.14.1 | 0.15.x | breaking |
| libp2p-autonat | 0.15.0 | 0.16.x | breaking |

---

## 代码修改指引（方案 B 适用）

### 步骤 1: 升级 Rust 工具链

```bash
rustup install 1.88.0
rustup default 1.88.0
```

### 步骤 2: 更新 Cargo.toml

```toml
# 将 workspace 中的 libp2p 改为 git 引用（或新版本发布后改为版本号）
libp2p = { git = "https://github.com/libp2p/rust-libp2p", rev = "cdabcd0...", features = [ ... ] }

# 移除不再需要的补丁
# [patch.crates-io]
# libp2p-mdns = { path = "patches/libp2p-mdns" }
# libp2p-request-response = { path = "patches/libp2p-request-response" }
```

### 步骤 3: 适配 API 变更

**疑点 1: `Transport` 构建 API**
`swarm.rs` 中 `SwarmBuilder::with_tcp(...)` 等 API 可能变化。检查 `Transport redesign` commit (079b2d6) 的影响范围。

**疑点 2: `ConnectionClosed` 事件**
`ConnectionClosed` 结构体字段可能变化（`remaining_established` 已被移除）。检查 `P2pActor::handle_swarm_event` 中的 match 模式。

**疑点 3: `NetworkBehaviour::poll` 返回值**
`ToSwarm` 枚举可能新增变体，需要 exhaustive match 处理。

### 步骤 4: 编译 & 修复

```bash
# 初始编译，收集所有编译错误
cargo check 2>&1 | tee compile_errors.log

# 分类修复
# - 类型变更：更新字段名/类型
# - 方法移除：替换为新 API
# - 枚举变体：补充 match 分支
```

### 步骤 5: 回归测试

```bash
# 1. 编译通过
cargo check --workspace

# 2. 所有 tests 通过
cargo test --workspace

# 3. Clippy 无新增警告
cargo clippy --workspace

# 4. 运行时验证
# - 启动两个 CLI 实例，验证 mDNS 互相发现
# - 发送消息验证 request-response 正常
# - 检查日志中无 assertion panic
```

---

## 建议路线

```
当前 (0.56.0 + patches) ──→ 继续维护补丁 ──→ 0.57.0 发布后 ──→ 升级到 0.57.0
                              (方案 A)                         (方案 C)
```

**短期（现在）**：方案 A。当前补丁仅改动 2 行，风险极低，无需等待 0.57.0。

**中期（0.57.0 发布后）**：方案 C。移除补丁，直接升级到 0.57.0。升级工作预计 0.5 人日。

**不推荐**：方案 B（master 分支）。API 不稳定，编译时间增加，master 可能引入新问题。

---

## 附加：ML-DSA 稳定化跟踪

### 当前需要 `unstable` feature 的代码

```rust
// openwire_core/src/signature.rs:3
use aws_lc_rs::unstable::signature::{ML_DSA_65, ML_DSA_65_SIGNING, PqdsaKeyPair};

// openwire_core/src/identity.rs:178-179
use aws_lc_rs::unstable::signature::ML_DSA_65_SIGNING;
use aws_lc_rs::unstable::signature::PqdsaKeyPair;
```

### 稳定化后的迁移

当 `aws-lc-rs` 将 ML-DSA 移出 `unstable` 后：

```toml
# Cargo.toml 移除 unstable feature
aws-lc-rs = { version = "1.x", features = ["prebuilt-nasm"] }
```

```rust
// 移除 unstable 路径前缀
use aws_lc_rs::signature::{ML_DSA_65, ML_DSA_65_SIGNING, PqdsaKeyPair};
```

### 检查方法

```bash
# 定期检查 aws-lc-rs 是否有新版本
cargo search aws-lc-rs --limit 1

# 检查 unstable 是否被移除（PowerShell）
Select-String -Path Cargo.lock -Pattern 'aws-lc-rs' -SimpleMatch -Context 0,3
# 检查 Cargo.toml 中 unstable feature 是否仍被启用
Select-String -Path openwire_core/Cargo.toml -Pattern 'unstable'
```

## 补丁维护清单

| 补丁 | 文件 | 改动 | 上游状态 |
|------|------|------|----------|
| `libp2p-mdns` | `behaviour/iface.rs:95` | `4096` → `9216` | 未合并，需上游 PR |
| `libp2p-request-response` | `src/lib.rs:678` | `debug_assert_eq!` → `if` | 上游已修复（`155ccfe`），未发版 |

0.57.0 发布后，验证两处修复是否包含在发版中，确认后移除 `patches/` 目录和 `[patch.crates-io]` 配置。