# OpenWire / 开源通信

跳转： [English](#english) | [中文](#中文)

---

## English

OpenWire is a cross-platform P2P chat application currently in a very early stage of development. This project aims to provide a high-performance, low-resource, privacy-first communication tool.

***Demo only, use at your own risk***

## Goals

- Minimize data collection to protect privacy
- Local-first and security-first
- Open source and auditable

## Attention

- This project has not been audited yet
- Features are incomplete and there is no mature community

## License

This project is licensed under the [**GNU Affero General Public License v3.0**](LICENSE).
For communication software security reasons, the license is currently AGPLv3.0.

## Project Status

version: 0.0.1
🚧 This project is still in a very early development stage. Features are incomplete. Feedback is welcome!

## Features

- End-to-end encryption using ML-DSA + ML-KEM (post-quantum)
- P2P messaging with libp2p (TCP, QUIC, WebSocket, mDNS, DHT/Kademlia)
- File transfer with chunked compressed streaming, integrity verification, and resume support
- Auto relay for NAT traversal (Circuit Relay v2, DCUtR hole punching, AutoNAT)
- Offline message queue with auto-retry when contacts come online
- SQLite local storage with SQLx
- Post-quantum ready: AWS-LC-RS ML-DSA-65, ML-KEM-768, AES-GCM
- CLI interface based on ratatui
- Desktop GUI based on Tauri 2 + SvelteKit
- Cross-platform: Windows, macOS, Linux

## Project Structure

- `openwire_core/` — core P2P networking, crypto, file transfer, storage
- `openwire/` — Tauri 2 + SvelteKit desktop application
- `openwire_cli/` — CLI application (ratatui TUI + JSON mode)
- `openwire_server/` — Cloudflare Workers (relay registry, presence, signaling)
- `libp2p-pathranker/` — custom libp2p Kademlia path ranking library
- `rootcell/` — keyring/secure key storage library

## Technology Stack

- **Language**: Rust
- **P2P**: libp2p 0.56 (TCP, QUIC, WebSocket, Relay, DHT/Kademlia, AutoNAT, DCUtR, mDNS, Identify, Ping)
- **Crypto**: ML-DSA-65 (signing), ML-KEM-768 (key exchange), AES-GCM (encryption)
- **Storage**: SQLite (sqlx), Redb (DHT record store)
- **Serialization**: postcard (CBOR-based), serde
- **Compression**: zstd
- **Frontend**: Tauri 2 + SvelteKit
- **CLI**: ratatui
- **Server**: Cloudflare Workers

## Development and Testing

### Requirements

- Node.js (>=18)
- npm
- Rust toolchain

### Setup

```bash
# clone and enter the project
git clone https://github.com/OpenWire/im.git
cd im

# run desktop GUI
cd openwire
npm install
npm run tauri dev

# or run CLI
cd openwire_cli
cargo run

# build desktop GUI
cd openwire
npm run tauri build
```

## Contact

- QQ group: 1083388325
- [bilibili account](https://space.bilibili.com/3494362084280927/)

Contributions are welcome. Unless explicitly stated otherwise, contributions are assumed to be under the project license.

## HISTORY

- 2025 Project created
- 2026 Core P2P networking, encryption, file transfer implemented

## PLAN

Build the basic framework.
Future ideas:

- Compute sharing?
- Web-based P2P transport layer?

### Notes

^q^

---

## 中文

OpenWire 是一个跨平台 P2P 聊天应用程序，目前处于非常早期的开发阶段。本项目旨在提供一个高性能、低资源占用、隐私优先的通信工具。

***仅演示，生产自负***

## 目标

- 数据最小化保证隐私
- 本地优先，安全优先
- 开源可审计

## 注意

- 此项目版本目前未经审计
- 功能不完善，无完整社区

## License-zh

This project is licensed under the [**GNU Affero General Public License v3.0**](LICENSE)。
出于对通信软件安全性的考虑，暂定为AGPLv3.0。

## 项目状态

version: 0.0.1
🚧 此项目目前处于**非常早期的开发阶段**。功能尚不完善。欢迎提供建议！

## 特性

- 端到端加密：ML-DSA + ML-KEM（后量子密码）
- P2P 消息：libp2p 网络（TCP, QUIC, WebSocket, mDNS, DHT/Kademlia）
- 文件传输：分片压缩流式传输，完整性校验，断点续传
- 自动中继：NAT 穿透（Circuit Relay v2, DCUtR 打洞, AutoNAT）
- 离线队列：联系人上线后自动重试
- 本地存储：SQLite + Redb
- CLI：ratatui TUI
- GUI：Tauri 2 + SvelteKit
- 跨平台：Windows / macOS / Linux

## 项目结构

- `openwire_core/` — 核心 P2P 网络、加密、文件传输、存储
- `openwire/` — Tauri 2 + SvelteKit 桌面应用
- `openwire_cli/` — CLI 应用（ratatui TUI + JSON 模式）
- `openwire_server/` — Cloudflare Workers（中继注册、在线状态、信令）
- `libp2p-pathranker/` — 自定义 libp2p Kademlia 路径排序库
- `rootcell/` — 密钥环/安全密钥存储库

## 技术栈

- **语言**: Rust
- **P2P**: libp2p 0.56（TCP, QUIC, WebSocket, Relay, DHT/Kademlia, AutoNAT, DCUtR, mDNS, Identify, Ping）
- **加密**: ML-DSA-65（签名）, ML-KEM-768（密钥交换）, AES-GCM（数据加密）
- **存储**: SQLite（sqlx）, Redb（DHT 记录存储）
- **序列化**: postcard（CBOR-based）, serde
- **压缩**: zstd
- **前端**: Tauri 2 + SvelteKit
- **CLI**: ratatui
- **服务端**: Cloudflare Workers

## 开发测试

### 环境要求

- Node.js (>=18)
- npm
- Rust 工具链

### 开发环境搭建

```bash
# 克隆项目
git clone https://github.com/OpenWire/im.git
cd im

# 运行桌面 GUI
cd openwire
npm install
npm run tauri dev

# 或运行 CLI
cd openwire_cli
cargo run

# 构建桌面 GUI
cd openwire
npm run tauri build
```

## 联系

- 开发群 QQ:1083388325
- [bilibili 账号](https://space.bilibili.com/3494362084280927/)

欢迎贡献！在未明确申明的情况下，默认您的贡献遵守本项目的 license。

## HISTORY-zh

- 2025 创建项目
- 2026 完成核心 P2P 网络、加密、文件传输实现

## PLAN-zh

- 基础框架搭建完成
- 未来展望：
  - 算力共享？
  - 实现 P2P 传输层的网页？
