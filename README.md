# OpenWire

跨平台 P2P 聊天应用，采用**后量子端到端加密**。

Cross-platform P2P chat with **post-quantum end-to-end encryption**.

[English](#english) | [中文](#中文)

> ⚠️ **Demo / 开发阶段**：未经审计，请勿用于生产与敏感通信。
> **Demo only — not audited. Use at your own risk.**

---

## English

OpenWire is a peer-to-peer messenger: no accounts, no central server for your chats,
post-quantum encryption by default.

### Features

- **Post-quantum E2EE**: ML-DSA-65 + ML-KEM-768 + AES-GCM
- **P2P networking**: libp2p (QUIC, TCP, WebSocket, mDNS, Kademlia DHT)
- **NAT traversal**: Circuit Relay v2, DCUtR, AutoNAT
- **File transfer**: chunked streaming, resume, integrity verification
- **Offline queue**: messages are delivered automatically when a contact is online
- **Multiple surfaces**: Tauri 2 desktop app, ratatui CLI, JSON mode
- **Cross-platform**: Windows / macOS / Linux

### Quick start

```bash
git clone https://github.com/BirthAndDeath/OpenWire.git && cd OpenWire
# Desktop
cd openwire && npm install && npm run tauri dev
# CLI
cd openwire_cli && cargo run
```

### Usage tips

- Your identity is an exported/imported key (`身份 ID`), stored via your OS keyring;
  switch identities from the sidebar.
- Add friends by scanning their QR code, or by sharing your **Identity ID** (ML-DSA
  public key hex).
- Data lives in your app data directory (`documented in docs`); contacts, messages and
  files stay local.
- If the app cannot reach contacts behind NAT, configure **relay nodes** in
  **Settings → Nodes**.

### Feedback

- **Bugs / ideas / questions**: open a [GitHub Issue](https://github.com/BirthAndDeath/OpenWire/issues) —
  include steps to reproduce, OS, and logs if possible.
- Community: [Bilibili](https://space.bilibili.com/3494362084280927/)
- Note: this project is early-stage; issues are the best place to track requests.

### Repository layout

- `openwire_core/` — Core: P2P, crypto, file transfer, storage
- `openwire/` — Tauri 2 desktop app
- `openwire_cli/` — CLI (ratatui + JSON)
- `openwire_server/` — Relay / bootstrap server
- `libp2p-pathranker/` — libp2p path ranking
- `rootcell/` — Secure key storage

### License

[AGPL-3.0](LICENSE)

---

## 中文

OpenWire 是一个点对点通讯应用：没有账号体系、聊天不经过任何中心服务器，默认启用后量子加密。

### 特性

- **后量子 E2EE**: ML-DSA-65 + ML-KEM-768 + AES-GCM
- **P2P 网络**: libp2p（QUIC, TCP, WebSocket, mDNS, Kademlia DHT）
- **NAT 穿透**: Circuit Relay v2, DCUtR, AutoNAT
- **文件传输**: 分片流式、断点续传、完整性校验
- **离线队列**: 联系人上线后消息自动送达
- **多端形态**: Tauri 2 桌面应用、ratatui CLI、JSON 模式
- **跨平台**: Windows / macOS / Linux

### 快速开始

```bash
git clone https://github.com/BirthAndDeath/OpenWire.git && cd OpenWire
# 桌面端
cd openwire && npm install && npm run tauri dev
# CLI
cd openwire_cli && cargo run
```

### 使用小贴士

- 你的身份（ML-DSA 密钥对）经系统密钥环安全保存，可从侧边栏切换/管理多个身份。
- 添加好友：扫对方二维码，或直接分享你的**身份 ID**（ML-DSA 公钥 hex）。
- 数据（联系人、消息、文件）保存在本机应用数据目录，不上传任何服务器。
- 若对方在 NAT 后无法直连，请在 **设置 → 节点** 中配置中继节点。

### 反馈与交流

- **缺陷 / 建议 / 问题**：请在 [GitHub Issues](https://github.com/BirthAndDeath/OpenWire/issues)
  反馈——附上操作步骤、系统版本与日志更佳。
- 社区：QQ 群 1083388325 · [Bilibili](https://space.bilibili.com/3494362084280927/)
- 项目处于早期阶段，功能请求请在 Issues 中跟踪。

### 项目结构

- `openwire_core/` — 核心：P2P、加密、文件传输、存储
- `openwire/` — Tauri 2 桌面应用
- `openwire_cli/` — CLI（ratatui + JSON）
- `openwire_server/` — 中继 / 引导服务器
- `libp2p-pathranker/` — libp2p 路径排序
- `rootcell/` — 安全密钥存储

### 许可

[AGPL-3.0](LICENSE)