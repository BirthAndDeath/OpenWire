# 审查修复计划 / Code Review Fix Plan

目标：修复 8/9 条审查发现，全部经过源码核实。
Target: fix the review findings, all verified against the source. Where a finding is not fully confirmed, it is marked `待确认` and verified during implementation.

## 一、核实结论 / Verification Verdict

| 发现 Finding | 来源 Source | 核实结果 Verdict |
|---|---|---|
| relay 禁用触发 `unreachable!` panic | behaviour.rs:564-579 vs 288-294 | ✅ 确认（disable 清空 maps → 定时器到期落 vacant 分支） |
| relay 禁用后既有 circuit 仍转发 | behaviour.rs:291-293 | ✅ 确认（仅清簿记，handler 数据流独立） |
| peerid.enc 损坏 → 启动永久失败 | identity.rs:22, peerid_store.rs:55-75 | ✅ 确认（读路径还用 `init()` 会伪造新 master key） |
| 离线队列去重被移除 | message/ops.rs 259-268 | ✅ 确认（SHA256(pubkey,msgtype,data) → `""`） |
| msgtype 列与决策记录相悖 | storage/message.rs:30 + 003 迁移未跟踪 | ✅ 确认（代码走向=列权威；决策记录=内容推断） |
| `--apk` 注入全部矩阵 | build-tauri.yml:132 + `args:""` | ✅ 确认（空串 falsy → 桌面构建也带 `--apk`） |
| RPM 产物路径疑误 | build-server-packages.yml:132,138 | ⚠️ 部分确认（cargo-rpm 是否支持 `--target`/输出路径需本地验证） |
| relay-info 端口/空占位失真 | common/lib.rs:449-476 | ✅ 确认（恒用入参 port + 空集占位地址） |
| select_identity DB 先写、内存后切 | identity_ops/ops.rs:73,141 | ✅ 确认 |
| 收发共享传输配额 + 非法文件名静默丢弃 | file_transfer.rs:404-411,179-187 | ✅ 确认 |
| DialPeer fire-and-forget | src-tauri/lib.rs:1131-1154 + command_handler.rs | ✅ 确认（无响应通道） |
| strip_escape 放行 TAB/U+2028/9 | notui.rs:18-32 | ✅ 确认 |
| 缺 Firefox 路径拦截 | src-tauri/lib.rs:107-109 | 🔶 待确认（审查建议合理，码位需看全列表） |
| 65536 字节 vs UTF-16 码元差异 | src-tauri/lib.rs:87 vs index.js:133 | ✅ 确认 |
| copyPeerId 静默失败 | NetworkMonitor.svelte:47-55 | 🔶 待确认（行为真，影响待实测） |
| 分页常量三处硬编码 | storage/message.rs:8 等 | ✅ 确认 |
| `aws-lc-rs` unstable feature 冗余 | openwire_core/Cargo.toml:30 | ✅ 确认（grep 无引用） |
| 日志清理过宽 | log.rs:92-101 | ✅ 确认（前缀 `chat` 且非 `.log` 即删） |
| NetEvent 文档过期 | netevent.rs:45-49 | ✅ 确认（None 已被拒绝，文档仍写"降级信任"） |
| `generate_temporary_peerid` 死代码 | identity.rs:12-15 | ✅ 确认（pub + re-export，无内部调用方） |
| `ensure_peerid` async 无 await | identity_ops/ops.rs | ✅ 确认（clippy unused_async 候选） |
| Messagelist 新格式文件分支伪造 | Messagelist.svelte:96-115 | 🔶 待确认 |
| 审阅中其余 Low（debug_assert、常量等） | — | 归类到"暂不处理/可选" |

## 二、P0 — 合并前必修 / Must-fix before merge

### P0-1 relay 禁用 panic（崩溃可达）
- 位置：`patches/libp2p-relay/src/behaviour.rs:564-579`
- 方案：`ReservationTimedOut` 的 `Vacant(_) => unreachable!` 改为优雅分支（dispatch 工具 `libp2p_core::util::unreachable` 不适用；`on_connection_handler_event` 内 `Either::Right -> unreachable` 是结构不能达，此处是反例）。直接用 `{}` + debug 日志或 `tracing::debug!`，并删除该 branch 的 unreachable。
- 理由：disable 清空 maps 后该状态成为合法状态，panic 会杀死整个 `P2pActor`。

### P0-2 relay 禁用需真正停机
- 位置：`patches/libp2p-relay/src/behaviour.rs:288-294`
- 方案：不清空 `self.reservations`/`self.circuits`，保留簿记让 `on_connection_closed` 正常清理（避免对象状态涨袋与非对齐）。禁用语义改为：拒绝新请求（现有 414-446 行门控已做）+ 对既有 connection 发 `NotifyHandler` 关闭数据流；若本版本 `ToSwarm` 无 `CloseConnection`，则保留簿记并更新 doc 注释为"既有 circuit 在连接关闭时终止"（明示限制）。
- 验证：`cargo check --workspace`；手动 `SetPaidNetworkMode("disabled")` 后 relay 无新 reservation。

### P0-3 peerid 损坏自愈（区分 keyring 失败与数据损坏）
- 位置：`openwire_core/src/peerid_store.rs:55-75`、`openwire_core/src/identity.rs:19-36`
- 方案：
  - 读路径改用 `EncryptedStore::open()`；仅创建分支用 `init()`，杜绝读路径伪造 master key。
  - `load_or_create` 返回错误时区分：master key 不存在（keyring 问题）→ 硬停（符合约束）；`load` 返回加载/解密/反序列化错误 → `delete_corrupted_entry(data_dir)` 后重建一次。
  - `identity::load_or_create_peerid` 简化：让 `PeerIdConfig::load_or_create` 内部完成自愈，直接 `?` 传播，或顶层对 load-Err 同样走 delete+retry。
- 注意：AEAD 失败与 master key 丢失在密码学上不可区分；方案采用"重建=静默身份轮换"。若不可接受，退化为保留硬停但错误信息附带文件路径与处置指引——二选一，默认前者（复用已有 `delete_corrupted_entry` 机制）。

### P0-4 恢复离线队列内容去重
- 位置：`openwire_core/src/core/message/ops.rs:257-268`
- 方案：恢复 `SHA256(mldsa_pubkey_hex ‖ msgtype ‖ data)` 作为 dedup key 传给 `add_message_with_hash`（与 HEAD 行为一致；key 含 peer，跨 peer 不误伤）。同时修复注释——"无法计算协议 hash"只适用于协议级 hash，内容级 dedup key 一直可得。
- 验证：单测或手动验证同一文本离线发两次只入队一条。

### P0-5 CI 矩阵 `--apk` 注入
- 位置：`.github/workflows/build-tauri.yml`
- 方案：android 条目（L35-36）显式加 `args: "--apk"`；L132 改为 `args: ${{ matrix.args || '' }}`。
- 验证：空/非空 args 各走一遍逻辑检查。

### P0-6 msgtype 决策对齐 + 迁移入库
- 位置：`openwire_core/src/core/message/ops.rs:379-385`、`openwire_core/migrations/003_add_msgtype.sql`
- 方案（默认方向=保持现状"列权威"，与工作树一致，符合"一个概念只保留一份表示"）：
  - `retry_single_pending_message`：当 `msgtype == 0` 且内容形如 `[N]<hex>` 时回退内容推断（覆盖迁移前行），否则按列。
  - `003_add_msgtype.sql` 与代码同 commit（当前 untracked，否则 sqlx `ignore_missing=false` 下全新 checkout 编译失败）。
  - 更新决策记忆（`sqlx.msgtype_migration_keep` 的"回退内容推断"描述已过时）。
- 备选（若坚持旧决策）：删除列引用、恢复 `detect_msgtype`，列保留不引用。二选一，默认前者。

## 三、P1 — 本迭代应修 / Should-fix this iteration

### P1-1 relay-info.json 只写真实地址
- 位置：`openwire_server/common/src/lib.rs:449-476, 273/284`
- 方案：`listened` 为空时不写占位地址（写 `"addresses": []` 并 ERROR 日志），`port` 字段从实际绑定地址解析（`get_port` from addresses / 无监听时为 0），不再用入参 port。
- 验证：端口被占用回退分配后，relay-info.json 的 port 与实际监听一致。

### P1-2 select_identity 事务性
- 位置：`openwire_core/src/core/identity_ops/ops.rs:71-174`
- 方案：`set_current_identity` 移至 `ensure_peerid` 与 `rebuild_p2p_stack` 都成功之后（或失败时按新 identity 回滚）。最小改动：把 L73 的 DB 写入挪到 L153 `rebuild_p2p_stack` 成功之后。
- 验证：切换时 keyring 不可用 → 内存与 DB 一致保持旧身份。

### P1-3 传输配额分离 + 非法文件名告警
- 位置：`openwire_core/src/core/file_transfer.rs:404-411, 179-187`
- 方案：入站/出站分别计数（各按 `MAX_CONCURRENT_TRANSFERS`）；`handle_file_download_response` 非法文件名分支补 `send_warning_mpsc`。
- 注意：出站最多 `MAX_CONCURRENT_TRANSFERS`（3），入站同值。若连入站也按 3 限制过严，可在实现时确认约束。

### P1-4 DialPeer 反馈通道
- 位置：`openwire_core/src/command.rs`（DialPeer 加 oneshot 响应）+ `openwire_core/src/core/handle/command_handler.rs:168-194` + `openwire/src-tauri/src/lib.rs`
- 方案：仿 `ExportRoutingTable` 模式：`ChatCommand::DialPeer` 携带 `Arc<oneshot::Sender<...>>`，P2pActor 侧发送成功/失败结果；Tauri 命令返回结果字符串。最小替代：UI 文案改为"已发送拨号请求"。
- 默认先做最小替代，避免扩接口；若需要诊断则加 oneshot。

### P1-5 终端注入清洗
- 位置：`openwire_cli/src/notui.rs:18-32`
- 方案：`strip_escape` 白名单化——只允许可见 ASCII（`c.is_ascii()` 且非控制字符），剔除 TAB、U+2028/2029、C1 控制符；TUI 路径（`tui/event.rs`）与 notui 的 Log/Warning/Error 输出同样过清洗。
- 验证：构造含 `\t\u{2028}` 的远端消息，TUI 不换行。

### P1-6 长度语义对齐（字节）
- 位置：`openwire/src-tauri/src/lib.rs:87-89`、`openwire/dist-isolation/index.js:133`
- 方案：JS 侧用 `new TextEncoder().encode(p.message).length` 校验 65536 字节，与 Rust 一致。
- 验证：22k+ 汉字消息两端判定一致。

### P1-7 sensitive path 补 Firefox
- 位置：`openwire/src-tauri/src/lib.rs:107-109`（审查提）
- 方案：`reject_sensitive_path` 黑名单补 `/mozilla/firefox/` 与 Windows `appdata/roaming/mozilla` 片段（实现前先读完整函数确认匹配方式）。

## 四、P2 — 低风险清理 / Low-risk cleanup

- 移除 `aws-lc-rs` `unstable` feature（Cargo.toml:30，grep 确认零引用）
- 收紧 `log.rs:92-101` 清理条件（仅匹配旧日志命名形态，而非任意 `chat` 前缀）
- 更新 `netevent.rs:45-49` 文档（缺失签名=直接拒绝，非降级）
- 删除 `generate_temporary_peerid` 冗余（先 grep 全仓含 re-export 确认零调用）或保留并在内存记录原因
- `identity_ops/ops.rs` `ensure_peerid` 去掉 async（或加 `#[allow(clippy::unused_async)]`）
- 分页常量统一：JS 常量改为从后端派生或单点注释互指
- `ChatMessageType` TryFrom 用 `value == ChatMessageType::X as i32` 或 derive，消除重复表示
- workflows：`build-server-packages.yml` RPM 路径/`--target` 支持做本地一次验证后修正；`build-tauri.yml` keystore decode 加 `if` fail-fast；gradle signing 仅在有环境变量时挂载；`ci-security.yml` 补 `-p openwire_server_common`
- `copyPeerId`/`Messagelist` 新格式分支按 `待确认` 结果处理
- relay patch 的 `debug_assert!`（behaviour.rs:592）：保留，纯本地 patch

## 五、不处理（明确 reason）/ Won't fix

- `constant_time_compare` 用于非机密哈希（events.rs:793）：无害，保留
- 全监听失败静默容忍：符合 `p2p.listening_either_ipv4_or_ipv6_sufficient` 约束，前端以空监听集为警告即可
- 路由表 cache 在 PeerID 重建后加载旧缓存（swarm.rs:323）：仅垃圾提示，无正确性问题，不修

## 六、验证方式 / Validation

1. `cargo check --workspace` 零新 warning
2. `cargo clippy --workspace -- -D warnings`（新增 P2 项）
3. grep 删除/改名符号零残留（AGENTS.md 自检)
4. 手动路径：relay disable → 无 panic、无新 reservation；peerid.enc 手工损坏 → 启动自愈；同文离线两次 → 单条 pending；relay-info 端口回退正确
5. workflow 逻辑走查（`--apk`、RPM glob）

## 实施顺序 / Order

P0-1..P0-6 → cargo check → P1 (收发配额→select_identity→relay-info→DialPeer→清洗→长度→sensitive) → P2 逐项 + grep 残留 → clippy → 手动验证清单