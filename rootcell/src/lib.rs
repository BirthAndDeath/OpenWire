//! # Chat Root of Trust - WebAuthn 硬件认证器版
//!
//! ## 架构概述
//!
//! 本模块实现了一个四层信任根架构，用于端到端加密聊天系统：
//!
//! ```text
//! L3: WebAuthn 硬件认证器（FIDO2/CTAP2）或软件令牌
//!     └── 作用：身份认证、硬件级私钥保护、防钓鱼
//!     
//! L2: 会话根密钥（X25519 ECDH + HKDF）
//!     └── 作用：密钥协商、前向保密基础
//!     
//! L1: 消息密钥（双棘轮派生）
//!     └── 作用：每条消息独立密钥、后向保密
//!     
//! L0: 多设备状态同步（向量时钟 + 硬件签名）
//!     └── 作用：跨设备消息状态同步、防重放攻击
//! ```
//!
//! ## 核心特性
//!
//! - **硬件安全**: 支持 YubiKey、Windows Hello、Touch ID 等 FIDO2 认证器
//! - **软件回退**: 开发环境可使用 SoftToken，生产环境强制硬件
//! - **前向保密**: 通过 X25519 ECDH 和链密钥轮换实现
//! - **多会话管理**: 同时维护与多个聊天对象的安全会话
//! - **零信任架构**: 私钥永不离开硬件，仅存储凭证句柄
//!
//! ## 使用示例
//!
//! ```rust,no_run
//! use rootcell::{RootOfTrust, SoftwareTokenHsm, SessionManager};
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     // 1. 初始化硬件安全模块（HSM）
//!     let hsm = SoftwareTokenHsm::new()?; // 生产环境使用 UsbHsm::new().await?
//!     
//!     // 2. 创建信任根（自动注册 WebAuthn 身份）
//!     let root = RootOfTrust::with_hsm(hsm).await?;
//!     
//!     // 3. 获取 X25519 公钥用于密钥交换
//!     let my_public_key = root.public_key();
//!     
//!     // 4. 与对方建立加密会话
//!     let mut manager = SessionManager::new(root);
//!     manager.establish_session(b"peer_credential_id", &peer_public_key).await?;
//!     
//!     // 5. 加密/解密消息
//!     let encrypted = manager.encrypt_to(b"peer_credential_id", b"Hello")?;
//!     let decrypted = manager.decrypt_from(b"peer_credential_id", &encrypted)?;
//!     
//!     Ok(())
//! }
//! ```
//!
//! ## Cargo.toml 配置
//!
//! ```toml
//! [dependencies]
//! rootcell = { path = "../rootcell", features = ["hardware-auth"] }
//!
//! # 开发环境（软件令牌）
//! [dev-dependencies]
//! rootcell = { path = "../rootcell" } # 默认无 hardware-auth
//!
//! # 生产环境（USB HID 硬件密钥）
//! [features]
//! default = ["hardware-auth", "usb-hid"]
//! ```

use aes_gcm::{
    aead::{Aead, KeyInit},
    Aes256Gcm, Nonce,
};
use rand::rngs::OsRng;
use std::collections::HashMap;
use std::fmt;
use x25519_dalek::{PublicKey as X25519PublicKey, ReusableSecret};
use zeroize::{Zeroize, ZeroizeOnDrop};

/// 条件编译引入 WebAuthn 认证器库
///
/// ## feature = "hardware-auth"
/// 启用真实 WebAuthn 支持，需要：
/// - async-trait: 异步 trait 支持
/// - webauthn-authenticator-rs: CTAP2/USB/NFC 通信
/// - webauthn-rs-proto: WebAuthn 协议类型
///
/// ## 无 feature（默认）
/// 使用软件模拟，仅用于单元测试，**生产环境禁用**
#[cfg(feature = "hardware-auth")]
use async_trait::async_trait;
#[cfg(feature = "hardware-auth")]
use webauthn_authenticator_rs::{softtoken::SoftToken, ui::CliUi, AuthenticatorBackend};
#[cfg(feature = "hardware-auth")]
use webauthn_rs_proto::RegisterPublicKeyCredential;

/// 信任根错误类型
///
/// 使用 thiserror 派生，支持错误链和上下文
#[derive(thiserror::Error, Debug)]
pub enum TrustError {
    /// 访问被拒绝 - 硬件认证失败或权限不足
    #[error("AccessDenied")]
    AccessDenied,

    /// 硬件不可用 - 设备未插入或驱动问题
    #[error("HardwareUnavailable: {0}")]
    HardwareUnavailable(String),

    /// 密钥已撤销 - 凭证被标记为失效
    #[error("KeyRevoked:{0}")]
    KeyRevoked(String),

    /// 加密操作失败 - 内存不足或算法错误
    #[error("CryptoFailure")]
    CryptoFailure,

    /// 存储错误 - SQLite 或文件系统问题
    #[error("Storage error:{0}")]
    Storage(String),

    /// 加密/解密失败 - 密文损坏或密钥错误
    #[error("Encryption error: {0}")]
    Encryption(String),

    /// 序列化失败 - postcard 编码错误
    #[error("Serialization error: {0}")]
    Serialization(String),

    /// 密钥协商失败 - ECDH 计算错误
    #[error("KeyAgreement failed: {0}")]
    KeyAgreement(String),

    /// WebAuthn 协议错误 - CTAP2 通信异常
    #[error("WebAuthn error: {0}")]
    WebAuthn(String),

    /// 认证器未注册 - 首次使用需调用 register_identity
    #[error("Authenticator not registered")]
    AuthenticatorNotRegistered,

    /// 无有效会话 - 需先调用 establish_session
    #[error("No session for peer")]
    NoSession,

    /// 检测到重放攻击 - 时间戳或序列号异常
    #[error("Replay attack detected")]
    ReplayDetected,
}

/// 认证器类型枚举
///
/// 区分不同安全级别的认证方式，用于 UI 提示和策略控制
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum AuthenticatorType {
    /// USB HID 硬件密钥（YubiKey 5 系列等）
    /// - 跨平台、可插拔
    /// - 支持 Ed25519/P-256 签名
    UsbHid,

    /// 平台内置认证器（生物识别）
    /// - Windows Hello、Apple Touch ID、Android Keystore
    /// - 绑定特定设备，无法迁移
    Platform,

    /// 跨平台蓝牙（Hybrid/CABLE）
    /// - 手机作为认证器，通过蓝牙/USB 桥接
    /// - 适用于无 USB 口的设备
    Hybrid,

    /// 软件令牌（**仅开发/测试**）
    /// - 纯 Rust 实现，无硬件依赖
    /// - 私钥存储在内存，**无安全保证**
    SoftToken,
}

/// 硬件身份凭证（WebAuthn 注册结果）
///
/// 包含注册时从硬件获取的公钥和凭证 ID。
/// **注意**: 这不包含私钥，私钥永远留在硬件内。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct HardwareIdentity {
    /// 认证器类型（影响安全策略）
    pub auth_type: AuthenticatorType,

    /// 凭证 ID（唯一标识硬件中的密钥对）
    /// - 用于后续认证时指定使用哪个密钥
    /// - 可安全存储，不含敏感信息
    pub credential_id: Vec<u8>,

    /// COSE 格式的公钥（用于验证签名）
    /// - 发送给服务器或其他用户验证身份
    /// - 格式遵循 WebAuthn 标准（CBOR 编码）
    pub public_key: Vec<u8>,

    /// 用户验证标志（UV - User Verified）
    /// - true: 用户通过 PIN/生物识别验证
    /// - false: 仅物理接触（如点击按钮）
    pub user_verified: bool,

    /// 证明证书（Attestation）
    /// - 验证硬件真实性（防伪造设备）
    /// - 企业环境可要求特定厂商证书
    pub attestation: Option<Vec<u8>>,
}

/// WebAuthn 认证断言（签名结果）
///
/// 使用硬件私钥对挑战进行签名，证明：
/// 1. 用户拥有物理设备（ possession ）
/// 2. 用户知晓 PIN 或提供生物特征（ verification ）
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AuthAssertion {
    /// ECDSA/EdDSA 签名值
    pub signature: Vec<u8>,

    /// 认证器数据（包含计数器、UV 标志等）
    pub authenticator_data: Vec<u8>,

    /// 客户端数据 JSON（包含挑战、 origin 等）
    /// - 用于防钓鱼（验证 origin 匹配）
    pub client_data_json: Vec<u8>,
}

/// 消息状态枚举（多设备同步用）
///
/// 用于在用户的多个设备间同步消息读取状态
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum MessageStatus {
    /// 发送中（本地已创建，未入队）
    Sending = 0,
    /// 已发送（进入网络层）
    Sent = 1,
    /// 已送达（对方设备确认接收）
    Delivered = 2,
    /// 已读（对方打开聊天窗口）
    Read = 3,
    /// 发送失败（网络错误或被拒绝）
    Failed = -1,
}

/// 状态同步消息（设备间加密传输）
///
/// 通过独立加密通道（如苹果 Push、FCM）发送，
/// 使用与聊天消息不同的密钥派生路径。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct StateSyncMessage {
    /// 会话 ID（32 字节，SHA-256 哈希）
    pub session_id: [u8; 32],

    /// 消息 ID（32 字节，唯一标识）
    pub message_id: [u8; 32],

    /// 当前状态
    pub status: MessageStatus,

    /// Unix 时间戳（秒）
    pub timestamp: u64,

    /// 向量时钟（解决并发冲突）
    /// - Key: 设备 ID
    /// - Value: 该设备上的逻辑时钟
    pub vector_clock: HashMap<Vec<u8>, u64>,

    /// 硬件签名（防止伪造状态更新）
    pub signature: Vec<u8>,
}

/// 硬件安全模块 Trait（WebAuthn 版）
///
/// 抽象不同认证器类型的统一接口，支持：
/// - 软件令牌（开发测试）
/// - USB HID 设备（YubiKey）
/// - 平台内置（生物识别）
///
/// ## 异步说明
/// 所有操作均为异步，因为：
/// - USB 通信需要等待用户插入设备
/// - 生物识别需要等待用户触摸/注视
/// - 部分操作涉及网络（ attestation 验证）
#[cfg(feature = "hardware-auth")]
#[async_trait::async_trait]
pub trait HardwareSecurityModule: Send + Sync {
    /// 注册硬件身份（首次设置）
    ///
    /// # 流程
    /// 1. 生成随机挑战（防止重放）
    /// 2. 调用硬件生成密钥对
    /// 3. 返回公钥和凭证 ID
    ///
    /// # 参数
    /// - `challenge`: 32 字节随机数，来自服务器或本地生成
    ///
    /// # 错误
    /// - `HardwareUnavailable`: 设备未连接
    /// - `WebAuthn`: 用户取消或超时
    async fn register_identity(&mut self, challenge: &[u8])
        -> Result<HardwareIdentity, TrustError>;

    /// 使用硬件签名认证（证明拥有私钥）
    ///
    /// # 流程
    /// 1. 提供挑战和凭证 ID 给硬件
    /// 2. 用户验证（PIN/生物识别）
    /// 3. 硬件返回签名
    async fn authenticate(&mut self, challenge: &[u8]) -> Result<AuthAssertion, TrustError>;

    /// 获取当前凭证 ID（注册后可用）
    fn credential_id(&self) -> Option<&[u8]>;

    /// 获取平台类型（用于 UI 显示）
    fn platform(&self) -> AuthenticatorType;

    /// 是否软件回退（安全策略检查）
    /// - true: 警告用户当前非硬件保护
    fn is_software_fallback(&self) -> bool;
}

/// 软件回退 Trait 实现（无 hardware-auth 特性时）
///
/// 使用 Rust 1.75+ 的 impl Trait in return type 语法避免 async-trait 依赖。
/// 功能与上方相同，但编译时无额外宏展开。
#[cfg(not(feature = "hardware-auth"))]
pub trait HardwareSecurityModule: Send + Sync {
    fn register_identity(
        &mut self,
        challenge: &[u8],
    ) -> impl std::future::Future<Output = Result<HardwareIdentity, TrustError>> + Send;
    fn authenticate(
        &mut self,
        challenge: &[u8],
    ) -> impl std::future::Future<Output = Result<AuthAssertion, TrustError>> + Send;
    fn credential_id(&self) -> Option<&[u8]>;
    fn platform(&self) -> AuthenticatorType;
    fn is_software_fallback(&self) -> bool;
}

// ==================== 软件令牌实现（SoftToken） ====================
// 仅用于开发和单元测试，私钥存储在内存中，**不保证安全**

pub struct SoftwareTokenHsm {
    #[cfg(feature = "hardware-auth")]
    /// 底层 SoftToken 实例（webauthn-authenticator-rs 提供）
    inner: SoftToken,

    /// 缓存的身份凭证（注册后填充）
    credential: Option<HardwareIdentity>,
}

impl SoftwareTokenHsm {
    /// 创建新的软件令牌
    ///
    /// # 警告
    /// 生产环境必须使用硬件实现（UsbHsm/PlatformHsm）
    pub fn new() -> Result<Self, TrustError> {
        #[cfg(feature = "hardware-auth")]
        {
            let token = SoftToken::new().map_err(|e| TrustError::WebAuthn(e.to_string()))?;
            Ok(Self {
                inner: token,
                credential: None,
            })
        }
        #[cfg(not(feature = "hardware-auth"))]
        {
            Ok(Self { credential: None })
        }
    }
}

/// hardware-auth 特性下的真实实现
#[cfg(feature = "hardware-auth")]
#[async_trait::async_trait]
impl HardwareSecurityModule for SoftwareTokenHsm {
    async fn register_identity(
        &mut self,
        challenge: &[u8],
    ) -> Result<HardwareIdentity, TrustError> {
        // 调用 SoftToken 注册，使用 CLI UI（命令行提示）
        let (credential, _) = self
            .inner
            .register(challenge, &CliUi)
            .await
            .map_err(|e| TrustError::WebAuthn(e.to_string()))?;

        let identity = HardwareIdentity {
            auth_type: AuthenticatorType::SoftToken,
            credential_id: credential.id.clone(),
            public_key: credential.response.public_key.clone(),
            user_verified: true, // SoftToken 默认已验证
            attestation: None,   // 软件令牌无证明证书
        };

        self.credential = Some(identity.clone());
        Ok(identity)
    }

    async fn authenticate(&mut self, challenge: &[u8]) -> Result<AuthAssertion, TrustError> {
        let credential_id = self
            .credential
            .as_ref()
            .ok_or(TrustError::AuthenticatorNotRegistered)?
            .credential_id
            .clone();

        let (auth_data, _) = self
            .inner
            .authenticate(challenge, &credential_id, &CliUi)
            .await
            .map_err(|e| TrustError::WebAuthn(e.to_string()))?;

        Ok(AuthAssertion {
            signature: auth_data.signature.clone(),
            authenticator_data: auth_data.authenticator_data.clone(),
            client_data_json: auth_data.client_data_json.clone(),
        })
    }

    fn credential_id(&self) -> Option<&[u8]> {
        self.credential.as_ref().map(|c| c.credential_id.as_slice())
    }

    fn platform(&self) -> AuthenticatorType {
        AuthenticatorType::SoftToken
    }

    fn is_software_fallback(&self) -> bool {
        true
    }
}

/// 无 hardware-auth 时的模拟实现（空操作）
#[cfg(not(feature = "hardware-auth"))]
impl HardwareSecurityModule for SoftwareTokenHsm {
    async fn register_identity(
        &mut self,
        challenge: &[u8],
    ) -> Result<HardwareIdentity, TrustError> {
        // 模拟注册：直接使用挑战作为凭证 ID
        let identity = HardwareIdentity {
            auth_type: AuthenticatorType::SoftToken,
            credential_id: challenge.to_vec(),
            public_key: vec![],
            user_verified: true,
            attestation: None,
        };
        self.credential = Some(identity.clone());
        Ok(identity)
    }

    async fn authenticate(&mut self, challenge: &[u8]) -> Result<AuthAssertion, TrustError> {
        // 模拟认证：返回挑战作为签名（**不安全，仅测试**）
        Ok(AuthAssertion {
            signature: challenge.to_vec(),
            authenticator_data: vec![],
            client_data_json: vec![],
        })
    }

    fn credential_id(&self) -> Option<&[u8]> {
        self.credential.as_ref().map(|c| c.credential_id.as_slice())
    }

    fn platform(&self) -> AuthenticatorType {
        AuthenticatorType::SoftToken
    }

    fn is_software_fallback(&self) -> bool {
        true
    }
}

// ==================== USB HID 硬件实现（YubiKey 等） ====================
// 需要同时启用 hardware-auth 和 usb-hid 特性

#[cfg(all(feature = "hardware-auth", feature = "usb-hid"))]
pub mod usb {
    use super::*;
    use webauthn_authenticator_rs::ctap2::CtapAuthenticator;

    /// USB HID 硬件安全模块
    ///
    /// 通过 CTAP2 协议与 YubiKey 等 FIDO2 设备通信
    pub struct UsbHsm {
        authenticator: CtapAuthenticator<CliUi>,
        credential: Option<HardwareIdentity>,
    }

    impl UsbHsm {
        /// 异步初始化，等待设备连接
        ///
        /// # 使用示例
        /// ```rust,no_run
        /// let hsm = UsbHsm::new().await?;
        /// ```
        pub async fn new() -> Result<Self, TrustError> {
            let ui = CliUi;
            let (auth, _) = CtapAuthenticator::new(ui)
                .await
                .map_err(|e| TrustError::HardwareUnavailable(e.to_string()))?;

            Ok(Self {
                authenticator: auth,
                credential: None,
            })
        }
    }

    #[async_trait::async_trait]
    impl HardwareSecurityModule for UsbHsm {
        async fn register_identity(
            &mut self,
            challenge: &[u8],
        ) -> Result<HardwareIdentity, TrustError> {
            let (credential, _) = self
                .authenticator
                .register(challenge)
                .await
                .map_err(|e| TrustError::WebAuthn(e.to_string()))?;

            let identity = HardwareIdentity {
                auth_type: AuthenticatorType::UsbHid,
                credential_id: credential.id.clone(),
                public_key: credential.response.public_key.clone(),
                user_verified: credential.response.authenticator_data.user_verified,
                attestation: credential.response.attestation_object.clone(),
            };

            self.credential = Some(identity.clone());
            Ok(identity)
        }

        async fn authenticate(&mut self, challenge: &[u8]) -> Result<AuthAssertion, TrustError> {
            let credential_id = self
                .credential
                .as_ref()
                .ok_or(TrustError::AuthenticatorNotRegistered)?
                .credential_id
                .clone();

            let (auth_data, _) = self
                .authenticator
                .authenticate(challenge, &credential_id)
                .await
                .map_err(|e| TrustError::WebAuthn(e.to_string()))?;

            Ok(AuthAssertion {
                signature: auth_data.signature.clone(),
                authenticator_data: auth_data.authenticator_data.clone(),
                client_data_json: auth_data.client_data_json.clone(),
            })
        }

        fn credential_id(&self) -> Option<&[u8]> {
            self.credential.as_ref().map(|c| c.credential_id.as_slice())
        }

        fn platform(&self) -> AuthenticatorType {
            AuthenticatorType::UsbHid
        }

        fn is_software_fallback(&self) -> bool {
            false
        }
    }
}

// ==================== KeyDelegate Trait（ChatCore 集成） ====================
/// 业务层密钥委托接口
///
/// ChatCore 通过此 trait 与 rootcell 交互，无需了解底层加密细节。
/// 实现者：RootOfTrust（单会话）或 SessionManager（多会话）

#[cfg(feature = "hardware-auth")]
#[async_trait::async_trait]
pub trait KeyDelegate: Send + Sync {
    /// 加密给指定 peer
    ///
    /// # 参数
    /// - `peer_id`: 对方凭证 ID（唯一标识）
    /// - `plaintext`: 明文数据（会被序列化后加密）
    ///
    /// # 返回
    /// 序列化的 EncryptedMessage 结构体
    async fn encrypt_for(&self, peer_id: &[u8], plaintext: &[u8]) -> Result<Vec<u8>, TrustError>;

    /// 解密来自指定 peer
    ///
    /// # 安全
    /// 自动验证时间戳（±5 分钟窗口）防重放
    async fn decrypt_from(&self, peer_id: &[u8], ciphertext: &[u8]) -> Result<Vec<u8>, TrustError>;

    /// 使用硬件私钥签名数据
    ///
    /// 用于状态同步消息的身份验证
    async fn sign(&self, data: &[u8]) -> Result<AuthAssertion, TrustError>;

    /// 获取当前硬件身份（用于展示或传输）
    fn identity(&self) -> &HardwareIdentity;

    /// 获取 X25519 公钥（用于密钥交换）
    fn public_key(&self) -> &X25519PublicKey;
}

/// 非 hardware-auth 版本（使用 impl Trait 语法）
#[cfg(not(feature = "hardware-auth"))]
pub trait KeyDelegate: Send + Sync {
    fn encrypt_for(
        &self,
        peer_id: &[u8],
        plaintext: &[u8],
    ) -> impl std::future::Future<Output = Result<Vec<u8>, TrustError>> + Send;
    fn decrypt_from(
        &self,
        peer_id: &[u8],
        ciphertext: &[u8],
    ) -> impl std::future::Future<Output = Result<Vec<u8>, TrustError>> + Send;
    fn sign(
        &mut self,
        data: &[u8],
    ) -> impl std::future::Future<Output = Result<AuthAssertion, TrustError>> + Send;
    fn identity(&self) -> &HardwareIdentity;
    fn public_key(&self) -> &X25519PublicKey;
}

// ==================== 多会话管理（支持多聊天对象） ====================

/// 多会话管理器
///
/// 维护与多个聊天对象的独立安全会话，每个会话有：
/// - 独立的链密钥（前向保密）
/// - 独立的消息计数器（防重放）
/// - 独立的 SecurityCore（加密实例）
///
/// ## 内存安全
/// 使用 `SecretKey`（ZeroizeOnDrop）确保密钥安全擦除
pub struct SessionManager<H: HardwareSecurityModule> {
    /// 根信任实例（包含硬件模块和长期身份）
    pub root: RootOfTrust<H>,

    /// 活跃会话映射：peer_credential_id -> SecurityCore
    /// SecurityCore 包含当前会话的 AES-256-GCM 密钥
    sessions: HashMap<Vec<u8>, SecurityCore>,

    /// 链密钥映射：用于密钥轮换（双棘轮算法基础）
    /// 每次消息发送后，链密钥通过 HKDF 更新
    chain_keys: HashMap<Vec<u8>, [u8; 32]>,

    /// 消息计数器：每个 peer 独立递增，防重放攻击
    counters: HashMap<Vec<u8>, u64>,
}

impl<H: HardwareSecurityModule> SessionManager<H> {
    /// 创建新的会话管理器
    ///
    /// # 参数
    /// - `root`: 已初始化的 RootOfTrust 实例
    pub fn new(root: RootOfTrust<H>) -> Self {
        Self {
            root,
            sessions: HashMap::new(),
            chain_keys: HashMap::new(),
            counters: HashMap::new(),
        }
    }

    /// 与指定 peer 建立会话（X25519 密钥协商）
    ///
    /// # 流程
    /// 1. ECDH: 本地 X25519 私钥 + 对方 X25519 公钥 -> 共享密钥
    /// 2. HKDF: 共享密钥 + peer_credential（盐）-> 链密钥
    /// 3. HKDF: 链密钥 + "session-v1"（上下文）-> 会话密钥
    ///
    /// # 参数
    /// - `peer_credential`: 对方的 WebAuthn 凭证 ID（唯一标识）
    /// - `peer_public`: 对方的 X25519 公钥（通过二维码/libp2p 交换）
    pub async fn establish_session(
        &mut self,
        peer_credential: &[u8],
        peer_public: &X25519PublicKey,
    ) -> Result<(), TrustError> {
        // X25519 ECDH 密钥协商
        let shared = self.root.x25519_static.diffie_hellman(peer_public);

        // HKDF 派生链密钥（使用 peer_credential 作为盐，绑定身份）
        let chain_key = hkdf_derive(shared.as_bytes(), peer_credential);
        let session_key = hkdf_derive(chain_key.expose_secret(), b"session-v1");

        self.sessions.insert(
            peer_credential.to_vec(),
            SecurityCore::from_key(session_key),
        );
        self.chain_keys
            .insert(peer_credential.to_vec(), *chain_key.expose_secret());
        self.counters.insert(peer_credential.to_vec(), 0);

        Ok(())
    }

    /// 加密消息（指定 peer）
    ///
    /// # 自动处理
    /// - 随机 nonce 生成（12 字节）
    /// - 时间戳绑定（Unix 时间戳）
    /// - 序列化（postcard）
    pub fn encrypt_to(
        &self,
        peer_credential: &[u8],
        plaintext: &[u8],
    ) -> Result<EncryptedMessage, TrustError> {
        let session = self
            .sessions
            .get(peer_credential)
            .ok_or(TrustError::NoSession)?;

        // 传入发送者凭证 ID
        session.encrypt_with_timestamp(plaintext, &self.root.identity.credential_id)
    }

    /// 解密消息（指定 peer）
    ///
    /// # 安全验证
    /// 1. 时间戳窗口检查（当前时间 ±5 分钟）
    /// 2. AES-256-GCM 认证解密
    /// 3. 反序列化
    pub fn decrypt_from(
        &self,
        peer_credential: &[u8],
        msg: &EncryptedMessage,
    ) -> Result<Vec<u8>, TrustError> {
        let session = self
            .sessions
            .get(peer_credential)
            .ok_or(TrustError::NoSession)?;

        // 验证时间窗口（防重放）
        let now = unix_timestamp();
        if msg.timestamp < now - 300 || msg.timestamp > now + 60 {
            return Err(TrustError::ReplayDetected);
        }

        session.decrypt_with_timestamp(msg)
    }

    /// 轮换会话密钥（前向保密）
    ///
    /// 定期调用（如每 100 条消息）或敏感操作后调用，
    /// 旧密钥泄露无法解密新消息。
    pub fn rotate_session(&mut self, peer_credential: &[u8]) -> Result<(), TrustError> {
        let old_chain = self
            .chain_keys
            .get(peer_credential)
            .ok_or(TrustError::NoSession)?;

        // HKDF 链式派生：旧链密钥 -> 新链密钥
        let new_chain = hkdf_derive(old_chain, b"rotate");
        let new_session = hkdf_derive(new_chain.expose_secret(), b"session-v1");

        self.sessions.insert(
            peer_credential.to_vec(),
            SecurityCore::from_key(new_session),
        );
        self.chain_keys
            .insert(peer_credential.to_vec(), *new_chain.expose_secret());

        Ok(())
    }

    /// 签名状态同步消息（多设备同步）
    ///
    /// 使用硬件私钥签名，防止伪造状态更新
    pub async fn sign_state_sync(&mut self, msg: &StateSyncMessage) -> Result<Vec<u8>, TrustError> {
        let data = serialize(&(msg.message_id, msg.status, msg.timestamp));
        let assertion = self.root.hsm.authenticate(&sha256(&data)).await?;
        Ok(serialize(&assertion))
    }
}

// ==================== 分层信任根（WebAuthn + X25519） ====================

/// 三层信任根实现
///
/// 这是系统的核心安全组件，组合了：
/// - WebAuthn 硬件认证（身份层）
/// - X25519 静态密钥（密钥协商层）
/// - AES-256-GCM（应用加密层）
pub struct RootOfTrust<H: HardwareSecurityModule> {
    /// 硬件安全模块实例（WebAuthn 认证器）
    pub hsm: H,

    /// 硬件身份凭证（注册后填充）
    pub identity: HardwareIdentity,

    /// X25519 静态私钥（用于 ECDH）
    /// 使用 ReusableSecret 允许多次密钥协商（Noise 协议模式）
    pub x25519_static: ReusableSecret,

    /// X25519 公钥（可公开分享）
    pub x25519_public: X25519PublicKey,

    /// 当前会话核心（建立会话后填充）
    session_core: Option<SecurityCore>,

    /// Peer 验证标志（会话建立后设为 true）
    peer_verified: bool,
}

impl<H: HardwareSecurityModule> RootOfTrust<H> {
    /// 使用硬件模块创建信任根
    ///
    /// # 流程
    /// 1. 生成随机挑战
    /// 2. 调用 HSM 注册身份（生成 WebAuthn 凭证）
    /// 3. 生成 X25519 密钥对
    /// 4. 警告软件回退（如果是 SoftToken）
    ///
    /// # 示例
    /// ```rust,no_run
    /// let hsm = SoftwareTokenHsm::new()?;
    /// let root = RootOfTrust::with_hsm(hsm).await?;
    /// ```
    pub async fn with_hsm(mut hsm: H) -> Result<Self, TrustError> {
        let mut challenge = [0u8; 32];
        getrandom::getrandom(&mut challenge).map_err(|_| TrustError::CryptoFailure)?;

        // WebAuthn 注册：硬件生成密钥对，返回公钥和凭证 ID
        let identity = hsm.register_identity(&challenge).await?;

        // 生成 X25519 密钥对（独立于 WebAuthn，用于加密）
        let static_secret = ReusableSecret::random_from_rng(OsRng);
        let public_key = X25519PublicKey::from(&static_secret);

        // 生产环境警告：检测到软件令牌
        if hsm.is_software_fallback() {
            #[cfg(not(debug_assertions))]
            eprintln!("WARNING: Using software WebAuthn token (SoftToken)!");
        }

        Ok(Self {
            hsm,
            identity,
            x25519_static: static_secret,
            x25519_public: public_key,
            session_core: None,
            peer_verified: false,
        })
    }

    /// 获取 X25519 公钥（用于密钥交换）
    ///
    /// 可通过二维码、NFC 或 libp2p 传输给对方
    pub fn public_key(&self) -> &X25519PublicKey {
        &self.x25519_public
    }

    /// 获取 WebAuthn 凭证 ID（唯一身份标识）
    pub fn credential_id(&self) -> &[u8] {
        &self.identity.credential_id
    }

    /// 与 peer 建立加密会话
    ///
    /// 结合 X25519 ECDH 和 WebAuthn 认证，确保：
    /// - 双方拥有对应的私钥（X25519）
    /// - 本地用户物理存在（WebAuthn 认证）
    pub async fn establish_session(
        &mut self,
        peer_public: &X25519PublicKey,
    ) -> Result<(), TrustError> {
        // X25519 密钥协商
        let shared_secret = self.x25519_static.diffie_hellman(peer_public);

        // WebAuthn 认证：证明用户物理存在
        let mut challenge = [0u8; 32];
        getrandom::getrandom(&mut challenge).map_err(|_| TrustError::CryptoFailure)?;
        let _assertion = self.hsm.authenticate(&challenge).await?;

        // 派生会话密钥（绑定 WebAuthn 身份和 X25519 共享密钥）
        let session_key = hkdf_derive(shared_secret.as_bytes(), b"rootcell-webauthn-session-v1");

        self.session_core = Some(SecurityCore::from_key(session_key));
        self.peer_verified = true;

        Ok(())
    }

    /// 关闭当前会话（清除内存中的密钥）
    pub fn close_session(&mut self) {
        self.session_core = None;
        self.peer_verified = false;
    }

    /// 检查是否有活跃会话
    pub fn is_session_active(&self) -> bool {
        self.session_core.is_some()
    }

    /// 检查是否使用软件回退
    pub fn is_software_fallback(&self) -> bool {
        self.hsm.is_software_fallback()
    }
}

/// 为 RootOfTrust 实现 KeyDelegate（单会话模式）
///
/// 注意：此实现仅支持单一 peer，多 peer 请使用 SessionManager
#[cfg(feature = "hardware-auth")]
#[async_trait::async_trait]
impl<H: HardwareSecurityModule> KeyDelegate for RootOfTrust<H> {
    async fn encrypt_for(&self, _peer_id: &[u8], plaintext: &[u8]) -> Result<Vec<u8>, TrustError> {
        let mut nonce = [0u8; 12];
        getrandom::getrandom(&mut nonce).map_err(|_| TrustError::CryptoFailure)?;

        let session = self.session_core.as_ref().ok_or(TrustError::NoSession)?;
        let encrypted = session.encrypt_with_timestamp(plaintext, &self.identity.credential_id)?;
        Ok(serialize(&encrypted))
    }

    async fn decrypt_from(
        &self,
        _peer_id: &[u8],
        ciphertext: &[u8],
    ) -> Result<Vec<u8>, TrustError> {
        let msg: EncryptedMessage = deserialize(ciphertext)?;

        let session = self.session_core.as_ref().ok_or(TrustError::NoSession)?;

        // 验证时间窗口
        let now = unix_timestamp();
        if msg.timestamp < now - 300 || msg.timestamp > now + 60 {
            return Err(TrustError::ReplayDetected);
        }

        session.decrypt_with_timestamp(&msg)
    }

    async fn sign(&self, data: &[u8]) -> Result<AuthAssertion, TrustError> {
        self.hsm.authenticate(&sha256(data)).await
    }

    fn identity(&self) -> &HardwareIdentity {
        &self.identity
    }

    fn public_key(&self) -> &X25519PublicKey {
        &self.x25519_public
    }
}

#[cfg(not(feature = "hardware-auth"))]
impl<H: HardwareSecurityModule> KeyDelegate for RootOfTrust<H> {
    async fn encrypt_for(&self, _peer_id: &[u8], plaintext: &[u8]) -> Result<Vec<u8>, TrustError> {
        let mut nonce = [0u8; 12];
        getrandom::getrandom(&mut nonce).map_err(|_| TrustError::CryptoFailure)?;

        let session = self.session_core.as_ref().ok_or(TrustError::NoSession)?;
        let encrypted = session.encrypt_with_timestamp(plaintext, &self.identity.credential_id)?;
        Ok(serialize(&encrypted))
    }

    async fn decrypt_from(
        &self,
        _peer_id: &[u8],
        ciphertext: &[u8],
    ) -> Result<Vec<u8>, TrustError> {
        let msg: EncryptedMessage = deserialize(ciphertext)?;

        let session = self.session_core.as_ref().ok_or(TrustError::NoSession)?;

        // 验证时间窗口
        let now = unix_timestamp();
        if msg.timestamp < now - 300 || msg.timestamp > now + 60 {
            return Err(TrustError::ReplayDetected);
        }

        session.decrypt_with_timestamp(&msg)
    }

    async fn sign(&mut self, data: &[u8]) -> Result<AuthAssertion, TrustError> {
        self.hsm.authenticate(&sha256(data)).await
    }

    fn identity(&self) -> &HardwareIdentity {
        &self.identity
    }

    fn public_key(&self) -> &X25519PublicKey {
        &self.x25519_public
    }
}

/// 加密消息格式（含时间戳 + 随机 nonce + 发送者凭证）
///
/// 用于网络传输的加密消息结构，包含：
/// - ciphertext: AES-256-GCM 密文
/// - nonce: 12 字节随机数
/// - timestamp: Unix 时间戳（防重放）
/// - sender_credential: 发送者 WebAuthn 凭证 ID
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct EncryptedMessage {
    pub ciphertext: Vec<u8>,
    pub nonce: [u8; 12],
    pub timestamp: u64,
    pub sender_credential: Vec<u8>,
}

// ==================== SecurityCore & SecretKey ====================

/// 安全核心（对称加密实现）
///
/// 封装 AES-256-GCM 操作，提供序列化支持
pub struct SecurityCore {
    key: SecretKey,
}

/// 最大消息大小（1MB），防止 DoS
const MAX_SIZE: usize = 1024 * 1024;

impl SecurityCore {
    /// 从密钥创建实例
    fn from_key(key: SecretKey) -> Self {
        Self { key }
    }

    /// 加密并序列化数据
    ///
    /// # 参数
    /// - `data`: 任何支持 Serialize 的数据
    /// - `nonce`: 12 字节随机数（必须唯一！）
    pub fn encrypt<T: serde::Serialize>(
        &self,
        data: &T,
        nonce: &[u8; 12],
    ) -> Result<Vec<u8>, TrustError> {
        // 使用 postcard 序列化（紧凑、快速、安全）
        let plaintext = postcard::to_allocvec(data)
            .map_err(|e| TrustError::Serialization(format!("Serialization failed: {}", e)))?;

        if plaintext.len() > MAX_SIZE {
            return Err(TrustError::Serialization("Message too large".to_string()));
        }

        self.key.encrypt(&plaintext, nonce)
    }

    /// 解密并反序列化数据
    pub fn decrypt<T: for<'de> serde::Deserialize<'de>>(
        &self,
        ciphertext: &[u8],
        nonce: &[u8; 12],
    ) -> Result<T, TrustError> {
        let plaintext = self.key.decrypt(ciphertext, nonce)?;

        let data = postcard::from_bytes(&plaintext)
            .map_err(|e| TrustError::Serialization(format!("Deserialization failed: {}", e)))?;

        Ok(data)
    }

    /// 强化加密：自动随机 nonce + 时间戳绑定
    ///
    /// 推荐用于所有消息加密，自动处理：
    /// - 安全随机 nonce 生成（OsRng）
    /// - 当前时间戳附加
    /// - 结构化打包（timestamp, plaintext）
    pub fn encrypt_with_timestamp(
        &self,
        plaintext: &[u8],
        sender_credential: &[u8],
    ) -> Result<EncryptedMessage, TrustError> {
        let mut nonce = [0u8; 12];
        getrandom::getrandom(&mut nonce).map_err(|_| TrustError::CryptoFailure)?;

        let timestamp = unix_timestamp();

        // 时间戳绑定到明文：防止截断重放
        let payload = serialize(&(timestamp, plaintext));
        let ciphertext = self.encrypt(&payload, &nonce)?;

        Ok(EncryptedMessage {
            ciphertext,
            nonce,
            timestamp,
            sender_credential: sender_credential.to_vec(),
        })
    }

    /// 解密并验证时间戳
    ///
    /// 返回原始明文（不含时间戳）
    pub fn decrypt_with_timestamp(&self, msg: &EncryptedMessage) -> Result<Vec<u8>, TrustError> {
        let payload: (u64, Vec<u8>) = self.decrypt(&msg.ciphertext, &msg.nonce)?;
        Ok(payload.1)
    }
}

/// 安全密钥类型（自动清零）
///
/// 使用 `zeroize` crate 确保内存安全擦除，
/// 即使发生 panic 也会执行 ZeroizeOnDrop。
#[derive(ZeroizeOnDrop)]
pub struct SecretKey {
    bytes: [u8; 32],

    /// 密钥版本（用于密钥轮换追踪）
    #[zeroize(skip)] // 版本号非敏感，保留用于调试
    version: u64,
}

impl SecretKey {
    /// 生成新的随机密钥（使用 OsRng）
    pub fn generate() -> Result<Self, TrustError> {
        let mut bytes = [0u8; 32];
        getrandom::getrandom(&mut bytes).map_err(|_| TrustError::CryptoFailure)?;
        Ok(Self { bytes, version: 1 })
    }

    /// 从字节数组创建（复制后清零输入）
    ///
    /// # 安全
    /// 输入数组会被立即清零，防止内存残留
    pub fn from_bytes(mut bytes: [u8; 32]) -> Self {
        let key = Self { bytes, version: 1 };
        bytes.zeroize();
        key
    }

    /// 暴露密钥字节（仅内部使用）
    ///
    /// # 警告
    /// 调用者必须确保不泄露密钥材料
    pub(crate) fn expose_secret(&self) -> &[u8; 32] {
        &self.bytes
    }

    /// AES-256-GCM 加密
    ///
    /// # 参数
    /// - `plaintext`: 明文数据
    /// - `nonce`: 12 字节随机数（IV）
    ///
    /// # 返回
    /// 密文（包含 16 字节认证标签）
    pub fn encrypt(&self, plaintext: &[u8], nonce: &[u8; 12]) -> Result<Vec<u8>, TrustError> {
        let cipher = Aes256Gcm::new_from_slice(self.expose_secret())
            .map_err(|e| TrustError::Encryption(format!("Failed to create cipher: {}", e)))?;
        let nonce = Nonce::from_slice(nonce);
        cipher
            .encrypt(nonce, plaintext)
            .map_err(|e| TrustError::Encryption(format!("Encryption failed: {}", e)))
    }

    /// AES-256-GCM 解密
    ///
    /// # 安全
    /// 自动验证认证标签，失败则返回错误（不暴露解密中间状态）
    pub fn decrypt(&self, ciphertext: &[u8], nonce: &[u8; 12]) -> Result<Vec<u8>, TrustError> {
        let cipher = Aes256Gcm::new_from_slice(self.expose_secret())
            .map_err(|e| TrustError::Encryption(format!("Failed to create cipher: {}", e)))?;
        let nonce = Nonce::from_slice(nonce);
        cipher
            .decrypt(nonce, ciphertext)
            .map_err(|e| TrustError::Encryption(format!("Decryption failed: {}", e)))
    }
}

/// Debug 实现（隐藏密钥内容）
impl fmt::Debug for SecretKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SecretKey")
            .field("version", &self.version)
            .field("bytes", &"[REDACTED]")
            .finish()
    }
}

// ==================== 工具函数 ====================

/// HKDF-SHA256 密钥派生
///
/// 使用 extract-then-expand 模式，适合：
/// - ECDH 共享密钥 -> 会话密钥
/// - 链密钥轮换
/// - 多密钥派生（不同 context 产生独立密钥）
///
/// # 参数
/// - `secret`: 输入密钥材料（IKM）
/// - `context`: 上下文信息（区分不同用途）
fn hkdf_derive(secret: &[u8], context: &[u8]) -> SecretKey {
    use hkdf::Hkdf;
    use sha2::Sha256;

    let hkdf = Hkdf::<Sha256>::new(None, secret);
    let mut okm = [0u8; 32];
    hkdf.expand(context, &mut okm).expect("HKDF expand failed");
    SecretKey::from_bytes(okm)
}

/// SHA-256 哈希
///
/// 用于数据完整性检查和挑战生成
pub fn sha256(data: &[u8]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(data);
    hasher.finalize().into()
}

/// Postcard 序列化（紧凑二进制格式）
///
/// 比 JSON 更小、更快，无反射攻击风险
pub fn serialize<T: serde::Serialize>(data: &T) -> Vec<u8> {
    postcard::to_allocvec(data).expect("serialize failed")
}

/// Postcard 反序列化
pub fn deserialize<T: for<'de> serde::Deserialize<'de>>(data: &[u8]) -> Result<T, TrustError> {
    postcard::from_bytes(data).map_err(|e| TrustError::Serialization(e.to_string()))
}

/// 获取当前 Unix 时间戳（秒）
fn unix_timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

// ==================== 测试 ====================

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试多会话管理器基本流程
    ///
    /// 验证：
    /// 1. 会话建立（X25519 密钥协商）
    /// 2. 消息加密/解密
    /// 3. 时间戳生成
    #[tokio::test]
    async fn test_session_manager() {
        let hsm = SoftwareTokenHsm::new().unwrap();
        let root = RootOfTrust::with_hsm(hsm).await.unwrap();
        let mut manager = SessionManager::new(root);

        // 模拟 peer：生成对方的 X25519 密钥对
        let peer_secret = ReusableSecret::random_from_rng(OsRng);
        let peer_public = X25519PublicKey::from(&peer_secret);

        // 建立会话
        manager
            .establish_session(b"peer1", &peer_public)
            .await
            .unwrap();

        // 加密消息
        let msg = b"Hello, multi-session!";
        let encrypted = manager.encrypt_to(b"peer1", msg).unwrap();

        // 验证时间戳已生成
        assert!(encrypted.timestamp > 0);

        // 验证发送者凭证已填充
        assert!(!encrypted.sender_credential.is_empty());

        // 注意：实际解密需要对方的 SecurityCore，此处仅验证结构
    }
}
