//! OpenWire 核心库，处理后端可复用代码工作
//! ✅
#![warn(missing_docs)]

// ============================================================
// DHT 存储特性检查：至少启用 mem_dht 或 redb_dht 之一
// mem_dht 与 redb_dht 可共存（mem_dht 控制 DhtCache 实现，redb_dht 控制 RedbRecordStore）
// ============================================================
#[cfg(not(any(feature = "mem_dht", feature = "redb_dht")))]
compile_error!("必须启用 mem_dht 或 redb_dht 特性之一（请在 Cargo.toml 的 features 中添加 mem_dht 或 redb_dht）");
// ============================================================

/// Actor 模块（P2P 事件循环 Actor 模式）
pub mod actor;
/// 命令与事件类型定义
pub mod command;
/// 压缩/解压缩模块
pub mod compression;
/// 核心逻辑（ChatCore）
pub mod core;
mod coreconfig;
/// 核心句柄（外部控制接口）
pub mod corehandle;
/// 加密模块（ML-KEM + AES-GCM）
pub mod crypto;
/// 诊断模块（DHT + 加密自检）
pub mod diagnostics;
/// 错误类型定义
pub mod error;
/// 身份管理（ML-DSA + ML-KEM）
pub mod identity;
/// 日志模块
mod log;
/// PeerID 持久化存储
pub mod peerid_store;

pub use vstd::prelude::*;
/// 消息结构定义
pub mod message;
/// P2P 网络模块
pub mod p2p;
/// 基于 Redb 的持久化 DHT 记录存储（供服务器节点使用）
#[cfg(feature = "redb_dht")]
pub mod server_redb_store;
/// 签名模块（ML-DSA）
pub mod signature;
/// 存储模块（SQLite）
pub mod storage;
/// 文件传输模块
pub mod transfer;
pub use actor::p2p::{P2pActor, P2pActorBuilder, P2pActorHandle, P2pCommand, P2pEvent};
pub use actor::RUNTIME;
pub use command::{ChatCommand, ChatcoreEvent, IncomingMessage, MessageEvent, NetworkStatusData, PeerInfoDto, TransferProgressStatus};
pub use core::ChatCore;
pub use coreconfig::CoreConfig;
pub use identity::{
    extract_public_key_from_private, generate_complete_identity, generate_temporary_peerid,
    load_or_create_peerid, load_or_generate_complete_identity,
};
pub use message::{ChatMessage, ChatMessageType, ChatResponse, OnlineStatusPayload};
pub use signature::{
    generate_mldsa_keypair, sign_data, validate_mldsa_pubkey_hex, verify_signature,
};
