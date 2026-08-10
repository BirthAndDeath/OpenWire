# openwire_server 子模块化可行性分析

## 现状

| 维度 | 现状 |
|------|------|
| 集成方式 | Cargo workspace 成员（`Cargo.toml` lines 8-9） |
| 代码量 | 2 个 Rust crate（`common` + `server_cli`）+ 1 个 Node.js 文件（`server.js`） |
| 依赖关系 | `openwire_server_common` → `openwire_core`（workspace dep），`server_cli` → `common`（path dep） |
| 独立构建性 | ❌ 无独立 `Cargo.lock`，依赖 workspace 级别的 `openwire_core`、`libp2p`、`tokio` 等 |
| 提交频率 | 仅 5 次 commit 涉及 `openwire_server/`，变更极低 |
| 共享依赖 | `server.js` 零依赖（纯 Node.js 内置模块），Rust 端与主项目共享 6 个 workspace 依赖 |

---

## 子模块方案分析

### 方案概述

将 `openwire_server/` 从 workspace 中移除，推送到独立仓库，作为子模块引入根目录。

### 优点

| 优点 | 说明 |
|------|------|
| 独立版本管理 | server 可独立发版、回滚，不影响主项目 |
| 解耦 CI | server 的测试/构建可独立运行，不阻塞主项目 |
| 减少克隆体积 | 不需要 server 的开发者可 `git clone --recurse-submodules=false` 跳过 |
| 权限隔离 | 可限制 server 仓库的写权限 |

### 缺点

| 缺点 | 严重程度 | 说明 |
|------|----------|------|
| **构建链断裂** | ❌ 致命 | `openwire_server_common` 依赖 workspace 的 `openwire_core`，子模块化后无 workspace 上下文，无法单独编译 |
| **Cargo.lock 分叉** | ❌ 致命 | 子模块需要独立 `Cargo.lock`，导致 `openwire_core`、`libp2p` 等版本可能 drift 不同步 |
| **开发体验降级** | ⚠️ 高 | `cargo build --workspace` 不再编译 server；`cargo check --all-targets` 需额外 `cd openwire_server && cargo check` |
| **子模块维护成本** | ⚠️ 中 | 每次切换分支需 `git submodule update --recursive`；CI 需额外步骤初始化子模块 |
| **PR 复杂度增加** | ⚠️ 中 | 跨仓库的修改需要两个 PR（主项目 + 子模块），且需按顺序合并 |
| **server.js 无关** | ⚠️ 中 | `server.js` 是纯 Node.js 文件，不依赖 Rust，但被一起打包进子模块增加了不必要的耦合 |

### 关键依赖问题

```
当前：  openwire_server_common  ──(workspace)──→  openwire_core
                                                   libp2p
                                                   tokio
                                                   tracing

子模块化后：openwire_server_common  ──(path/git)──→  openwire_core ?
                                                     需要 workspace 或 独立 dep
                                                     版本 drift 风险
```

子模块化后，`openwire_server_common` 无法再使用 `{ workspace = true }`，必须改为：
- `openwire_core = { git = "...", version = "..." }` → 依赖发布到 crates.io 的版本，或指定 git commit
- 或者 `openwire_core = { path = "../openwire_core" }` → 硬编码相对路径，违背子模块独立原则

---

## 推荐方案：保持现状（workspace 成员）

### 理由

1. **代码量太小不值得**：仅 2 个 Rust crate + 1 个 Node.js 文件，子模块管理开销（克隆、更新、CI 配置）远超其收益
2. **强耦合无法独立**：`openwire_server_common` 直接依赖 `openwire_core` 的核心类型（`DhtCache`、`NetEventRequest/Response`），子模块化后无法独立编译
3. **变更频率极低**：5 次 commit 触及 server，不值得为几乎不变的代码增加复杂的子模块管理
4. **破坏了开发体验**：`cargo check` 从一键变两步，CI 配置复杂度翻倍

### 适用子模块的场景（当前不满足）

| 条件 | 当前 | 要求 |
|------|------|------|
| 代码量 | ~300 行 Rust + ~140 行 JS | > 5000 行，或独立部署 |
| 变更频率 | 5 commits / 全生命周期 | 每周 ≥ 1 commit |
| 独立可部署 | ❌ 依赖 workspace | ✅ 可独立编译部署 |
| 独立团队维护 | 同一人 | 不同团队/不同权限 |
| 外部贡献者 | 不需要 | 外部开发者仅需 server |

---

## 更优替代方案

### 方案 A：Cargo workspace feature gate（推荐）

```toml
[features]
default = ["server"]
server = ["dep:openwire_server_common"]
```

在 workspace 成员中保留 `openwire_server`，但通过 feature 控制是否编译。`cargo build --workspace --no-default-features` 跳过 server。

**优点**：零架构变更，渐进式，`cargo check` 仍一键完成。

### 方案 B：独立仓库 + Cargo git dep（server 增长后）

当 server 代码量超过 5000 行或需独立部署时，将其移出 workspace 到独立仓库，`openwire_core` 通过 git path 引用：

```toml
# server/Cargo.toml
[dependencies]
openwire_core = { git = "https://github.com/OpenWire/im", branch = "main" }
```

**优点**：真正的独立版本管理，不破坏构建链。

### 方案 C：server.js 单独抽离

`server.js` 是纯 Node.js 文件，零依赖，与 Rust server 耦合无意义。可单独作为一个仓库：

```
openwire-signaling-server/     ← 独立 GitHub 仓库
├── server.js
└── README.md
```

**优点**：Node.js 用户无需拉取 Rust 代码，`npm install` 即可部署，降低门槛。

---

## 结论

| 方案 | 可行性 | 推荐度 |
|------|--------|--------|
| **子模块化** | ❌ 不可行 | 致命依赖问题，收益远小于成本 |
| **保持 workspace 成员** | ✅ 当前最优 | 最适合当前规模和耦合度 |
| **Feature gate 跳过编译** | ✅ 可做 | 零成本，给不需要 server 的构建路径 |
| **server.js 独立仓库** | ✅ 推荐补做 | 与 Rust 完全解耦，降低部署门槛 |

**建议路线**：
1. 短期：不做任何变更，保持 workspace 成员
2. 中期：将 `server.js` 单独推送到独立仓库（纯 Node.js，零依赖）
3. 长期：若 Rust server 代码量增长到 > 5000 行，移出 workspace 为独立仓库 + git dep