# Changelog

## v0.3.0 (unreleased)

### rootcell — 不兼容更改

- **加密文件格式变更**（`identity.rs` / `store.rs`）：文件头部从 `[version(1)][nonce(12)]` 改为 `[version(1)][salt(16)][nonce(12)]`，增加 16 字节随机 salt。
- **密钥派生改为每保存新密钥**：`derive_aes_key` 新增 `salt` 参数，使用随机 salt 作为 HKDF salt，每次保存派生独立 AES-256-GCM 密钥。即使 nonce 碰撞，因密钥不同可完全消除 GCM nonce 重用风险。
- **`PrivateKeyHandle` API 签名变更**：
  - `save()` 返回类型变更（`Result<Self>` → `Result<()>`）
  - `has_master_key()` 返回 `Result<bool, String>`（原为 `bool`）
  - `generate_and_save_master_key()` 返回 `Zeroizing<[u8;32]>`（原为 `[u8; 32]`）
  - `load_encrypted_private_key()` 返回 `Zeroizing<Vec<u8>>`（原为 `Vec<u8>`）
  - `private_key` 字段类型从 `Pin<Box<[u8]>>` 改为 `Box<[u8]>`
- **新增 `EncryptedStore` 泛型加密存储**（`store.rs`）：基于 keyring + AES-256-GCM 的 Serde 泛型 `save<T: Serialize>` / `load<T: DeserializeOwned>`，支持任意数据结构的加密持久化。
- **`PeerIdConfig` 存储改为加密**（`peerid_store.rs`）：Ed25519 私钥从明文 `peerid.json` 改为通过 `EncryptedStore` 加密存储，文件名使用 BLAKE3 哈希派生。
- **`keys` 目录权限设为 `0o700`**（Unix），加密文件权限 `0o600`。

### 迁移说明

v0.1.x 的 ML-DSA 加密密钥文件不能直接在新版本使用。如有重要 ML-DSA 身份需要保留，请在升级前记录并重新生成。PeerID 配置（`peerid.json`）在首次启动时自动迁移到加密存储，旧明文文件会被删除。

### security

- `copy_file` Tauri 命令：源路径和目标路径均通过 `canonicalize` + `starts_with(&data_canon)` 校验，并加入 TOCTOU 符号链接防护。
- `connected_peers` 跟踪：改为 `HashMap<PeerId, usize>` 连接计数，`ConnectionClosed` 使用 `saturating_sub` 避免下溢。
- 身份绑定检查：降级为 warn 日志，不阻断消息处理（签名验证已保证身份真实性）。
- `FriendOnline` 签名失败：增加 warn 日志，消除静默失败。
- 路由缓存 SHA-256 校验和移除：路由数据非机密，仅保留 TTL 时间戳。
- `FriendOnline` 增加 `version` 字段：接收端校验协议版本兼容性。
- 两处 `handle_friend_online` 的 `SendNetEventResponse::Ack` 重复代码提取为 `send_friend_online_ack` 辅助方法。

### features

- 路由表随机刷新（30 分钟间隔，随机 PeerID 发起 `get_closest_peers`）。
- DHT 身份停止提供（`P2pCommand::StopProviding`）替代 tombstone 重发布。
- PeerID 轮换时端口自增（`next_port` 辅助函数）。
- 前端文件分享格式兼容：`detect_msgtype` 识别 `[文件] [hash:...]` 格式。
- 多语言翻译更新（de/en/es/fr/ja/zh）。
- 设置页面重构（`+page.svelte`）。
