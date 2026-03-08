# Chat

Chat是一个基于 Tauri 框架构建的跨平台应用程序，目前处于非常早期的开发阶段。本项目旨在提供一个高性能、低资源占用的通用通信工具。

仅演示，生产自负

## 目标

- 数据最小化保证隐私
- 本地优先，安全优先
- 开源可审计

## Attention

- 此项目版本目前未经审计
- 功能不完善，无完整社区

## License

This project is licensed under the [GNU Affero General Public License v3.0](LICENSE).
出于对通信软件安全性的考虑，暂定为AGPLv3.0

## 项目状态

version:0.0.1
🚧 此项目目前处于**非常早期的开发阶段**。功能尚不完善。欢迎提供建议！

## 特性

## 项目结构

- /chat_cli 存放cli项目（rust命令行，基于ratatui，维护中）
- /chat_tauri 存放tauri项目（维护中）
- /chat_dioxus 存放dioxus项目（新建）
- /chat_core 存放核心项目逻辑（演进中）

### chat app

- **框架**: Tauri 2
- **后端**: Rust

### chat cli

- **界面**: ratatui

## 开发测试

### 环境要求

- Node.js (>=18)
- npm
- Rust 工具链

### 开发环境搭建

如果使用Tauri，请确保已安装Tauri CLI。
如果cargo网络超时，可以使用镜像源

```bash
# 克隆项目后进入目录
cd myapp

# 安装依赖
npm install

# 启动开发服务器
npm tauri dev
```

你也可以进入cli目录然后执行

```bash
cargo run
```

单独运行cli事例

### 开发构建

```bash
# 构建
npm tauri build
```

---
开发群 QQ:1083388325

## HISTORY

- 2025 创建项目
- 2026 摸鱼中

## PLAN

先搭建基础框架
未来展望：
点对点加密
身份密钥认证
算力共享(？)
以及各种可能的设想

### 吐槽

^q^
