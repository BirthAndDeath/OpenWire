# OpenWire

[En](#english) | [中](#中文)

---

## English

OpenWire is a cross-platform P2P chat app with post-quantum end-to-end encryption.

**⚠ Demo only — use at your own risk. Not audited.**

### Features

- **Post-quantum E2EE**: ML-DSA-65 + ML-KEM-768 + AES-GCM
- **P2P networking**: libp2p (QUIC, TCP, WebSocket, mDNS, Kademlia DHT)
- **NAT traversal**: Circuit Relay v2, DCUtR, AutoNAT
- **File transfer**: chunked streaming, resume, integrity verification
- **Offline queue**: auto-retry on contact online
- **CLI**: ratatui TUI + JSON mode
- **Desktop**: Tauri 2 + SvelteKit
- **Cross-platform**: Windows / macOS / Linux

### Structure

| Path | Description |
|---|---|
| `openwire_core/` | Core: P2P, crypto, file transfer, storage |
| `openwire/` | Tauri 2 desktop app |
| `openwire_cli/` | CLI (ratatui + JSON) |
| `openwire_server/` | Cloudflare Workers |
| `libp2p-pathranker/` | libp2p path ranking |
| `rootcell/` | Secure key storage |

### Quick Start

```bash
git clone https://github.com/OpenWire/im.git && cd im
# Desktop
cd openwire && npm install && npm run tauri dev
# CLI
cd openwire_cli && cargo run
```

### License

[AGPL-3.0](LICENSE)

---

## 中文

OpenWire 是一个跨平台 P2P 聊天应用，采用后量子端到端加密。

**⚠ 仅供演示，生产自负。未经审计。**

### 特性

- **后量子 E2EE**: ML-DSA-65 + ML-KEM-768 + AES-GCM
- **P2P 网络**: libp2p（QUIC, TCP, WebSocket, mDNS, Kademlia DHT）
- **NAT 穿透**: Circuit Relay v2, DCUtR, AutoNAT
- **文件传输**: 分片流式、断点续传、完整性校验
- **离线队列**: 联系人上线自动重试
- **CLI**: ratatui TUI + JSON 模式
- **桌面**: Tauri 2 + SvelteKit
- **跨平台**: Windows / macOS / Linux

### 项目结构

| 路径 | 说明 |
|---|---|
| `openwire_core/` | 核心：P2P、加密、文件传输、存储 |
| `openwire/` | Tauri 2 桌面应用 |
| `openwire_cli/` | CLI（ratatui + JSON） |
| `openwire_server/` | Cloudflare Workers |
| `libp2p-pathranker/` | libp2p 路径排序 |
| `rootcell/` | 安全密钥存储 |

### 快速开始

```bash
git clone https://github.com/OpenWire/im.git && cd im
# 桌面
cd openwire && npm install && npm run tauri dev
# CLI
cd openwire_cli && cargo run
```

### 联系

- QQ 群: 1083388325
- [Bilibili](https://space.bilibili.com/3494362084280927/)

### 许可

[AGPL-3.0](LICENSE)