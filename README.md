# Chat / 聊天

跳转： [English](#english) | [中文](#中文)

---

## English

Chat is a cross-platform application currently in a very early stage of development. This project aims to provide a high-performance, low-resource communication tool.

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

*(To be added)*

## Project Structure

- `/chat_cli` contains the CLI project (based on ratatui, under maintenance)
- `/chat_tauri` contains the Tauri project (under maintenance)
- `/chat_dioxus` contains the Dioxus project (new)
- `/chat_core` contains the core logic (evolving)

### chat_tauri

- **Framework**: Tauri 2
- **Backend**: Rust

### chat_cli

- **Interface**: ratatui

## Development and Testing

### Requirements

- Node.js (>=18)
- npm
- Rust toolchain

### Setup

If you use Tauri, make sure Tauri CLI is installed.
[Install Tauri CLI](https://tauri.app/zh-cn/start/prerequisites/)

If cargo times out, try using mirror sources.

```bash
# clone and enter the project
cd myapp

# enter the tauri project
cd chat_tauri

# install dependencies
npm install

# run in development mode
npm tauri dev
```

You can also enter the CLI directory and run:

```bash
cargo run
```

CLI usage example

*(todo: consider migrating to Dioxus)*

### Build

```bash
# build
npm tauri build
```

## Contact

- QQ group: 1083388325
- [bilibili account](https://space.bilibili.com/3494362084280927/)

Contributions are welcome. Unless explicitly stated otherwise, contributions are assumed to be under the project license.

## HISTORY

- 2025 Project created
- 2026 Still tinkering

## PLAN

Build the basic framework.
Future ideas:
- Compute sharing?
- Web-based P2P transport layer?

### Notes

^q^

---

## 中文

Chat是一个跨平台应用程序，目前处于非常早期的开发阶段。本项目旨在提供一个高性能、低资源占用的通用通信工具。

***仅演示，生产自负***

## 目标

- 数据最小化保证隐私
- 本地优先，安全优先
- 开源可审计

## 注意

- 此项目版本目前未经审计
- 功能不完善，无完整社区

## License

This project is licensed under the [**GNU Affero General Public License v3.0**](LICENSE)。
出于对通信软件安全性的考虑，暂定为AGPLv3.0。

## 项目状态

version: 0.0.1
🚧 此项目目前处于**非常早期的开发阶段**。功能尚不完善。欢迎提供建议！

## 特性

*(待补充)*

## 项目结构

- `/chat_cli` 存放 CLI 项目（基于 ratatui，维护中）
- `/chat_tauri` 存放 Tauri 项目（维护中）
- `/chat_dioxus` 存放 Dioxus 项目（新建）
- `/chat_core` 存放核心项目逻辑（演进中）

### chat_tauri

- **框架**: Tauri 2
- **后端**: Rust

### chat_cli

- **界面**: ratatui

## 开发测试

### 环境要求

- Node.js (>=18)
- npm
- Rust 工具链

### 开发环境搭建

如果使用 Tauri，请确保已安装 Tauri CLI。
[安装 tauri cli](https://tauri.app/zh-cn/start/prerequisites/)

如果 cargo 网络超时，可以尝试使用镜像源。

```bash
# 克隆项目后进入目录
cd myapp

# 进入 tauri 项目
cd chat_tauri

# 安装依赖
npm install

# 开发调试
npm tauri dev
```

你也可以进入 CLI 目录然后执行：

```bash
cargo run
```

运行 CLI 事例

*(todo:考虑是否迁移到 dioxus)*

### 开发构建

```bash
# 构建
npm tauri build
```

## 联系

- 开发群 QQ:1083388325
- [bilibili 账号](https://space.bilibili.com/3494362084280927/)

欢迎贡献！在未明确申明的情况下，默认您的贡献遵守本项目的 license。

## HISTORY

- 2025 创建项目
- 2026 摸鱼中

## PLAN

搭建基础框架 ing
未来展望：
- 算力共享？
- 实现 p2p 传输层的网页？

### 吐槽

^q^
