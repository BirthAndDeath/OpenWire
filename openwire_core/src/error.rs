//! # 错误类型定义
//!
//! 使用 thiserror 为各模块定义结构化错误类型。
//✅
use thiserror::Error;

// ============================================================
// 加密/解密错误
// ============================================================

/// 加密/解密模块错误
#[derive(Error, Debug)]
pub enum CryptoError {
    /// ML-KEM 公钥无效
    #[error("ML-KEM 公钥无效: {0:?}")]
    InvalidMlKemPublicKey(#[source] aws_lc_rs::error::KeyRejected),

    /// KEM 封装失败
    #[error("KEM 封装失败: {0:?}")]
    KemEncapsulationFailed(#[source] aws_lc_rs::error::Unspecified),

    /// AES-GCM 加密失败
    #[error("AES-GCM 加密失败: {0}")]
    AesGcmEncryptionFailed(String),

    /// 加密数据为空
    #[error("加密数据为空")]
    EncryptedDataEmpty,

    /// 不支持的加密版本
    #[error("不支持的加密版本: {version}, 当前版本为 {current}")]
    UnsupportedEncryptionVersion {
        /// 请求的加密版本
        version: u8,
        /// 当前支持的加密版本
        current: u8,
    },

    /// 加密数据太短，无法包含 KEM 密文
    #[error("加密数据太短，无法包含 KEM 密文")]
    EncryptedDataTooShort,

    /// KEM 解封装失败
    #[error("KEM 解封装失败: {0:?}")]
    KemDecapsulationFailed(#[source] aws_lc_rs::error::Unspecified),

    /// AES-GCM 解密失败
    #[error("AES-GCM 解密失败: {0}")]
    AesGcmDecryptionFailed(String),
}

// ============================================================
// 身份错误
// ============================================================

/// 身份模块错误
#[derive(Error, Debug)]
pub enum IdentityError {
    /// 数据库未初始化
    #[error("数据库未初始化")]
    DatabaseNotInitialized,

    /// 生成 ML-KEM-768 密钥对失败
    #[error("生成 ML-KEM-768 密钥对失败: {0:?}")]
    GenerateMlKemKeypairFailed(#[source] aws_lc_rs::error::Unspecified),

    /// 获取封装密钥失败
    #[error("获取封装密钥失败: {0:?}")]
    GetEncapsulationKeyFailed(#[source] aws_lc_rs::error::Unspecified),

    /// 序列化 ML-KEM 公钥失败
    #[error("序列化 ML-KEM 公钥失败: {0:?}")]
    SerializeMlKemPublicKeyFailed(#[source] aws_lc_rs::error::Unspecified),

    /// 解析 ML-DSA 私钥失败
    #[error("解析 ML-DSA 私钥失败: {0:?}")]
    ParseMlDsaPrivateKeyFailed(#[source] aws_lc_rs::error::KeyRejected),

    /// ML-KEM 公钥应临时生成，不能从私钥提取
    #[error("ML-KEM 公钥应临时生成，不能从私钥提取")]
    MlKemKeyNotExtractable,

    /// 根证书身份加载失败
    #[error("根证书身份加载失败: {0}")]
    RootCellIdentityLoadFailed(#[source] Box<dyn std::error::Error + Send + Sync>),

    /// 签名错误
    #[error("签名错误: {0}")]
    SignatureError(#[from] SignatureError),

    /// 存储错误
    #[error("存储错误: {0}")]
    StorageError(#[from] StorageError),
}

// ============================================================
// 签名错误
// ============================================================

/// 签名/验证模块错误
#[derive(Error, Debug)]
pub enum SignatureError {
    /// 生成 ML-DSA 密钥对失败
    #[error("生成 ML-DSA 密钥对失败: {0:?}")]
    GenerateMlDsaKeypairFailed(#[source] aws_lc_rs::error::Unspecified),

    /// 解析 ML-DSA 私钥失败
    #[error("解析 ML-DSA 私钥失败: {0:?}")]
    ParseMlDsaPrivateKeyFailed(#[source] aws_lc_rs::error::KeyRejected),

    /// 签名数据失败
    #[error("签名数据失败: {0:?}")]
    SignDataFailed(#[source] aws_lc_rs::error::Unspecified),

    /// 私钥为空
    #[error("私钥为空")]
    EmptyPrivateKey,
}

// ============================================================
// 压缩错误
// ============================================================

/// 压缩/解压缩模块错误
#[derive(Error, Debug)]
pub enum CompressionError {
    /// 压缩数据为空
    #[error("压缩数据为空")]
    CompressedDataEmpty,

    /// 压缩数据过大
    #[error("压缩数据过大: {0} 字节")]
    CompressedDataTooLarge(usize),

    /// 解压后数据超过大小限制
    #[error("解压后数据超过大小限制: {0} 字节")]
    DecompressedDataTooLarge(usize),

    /// 压缩失败
    #[error("压缩失败: {0}")]
    CompressFailed(#[source] std::io::Error),

    /// 解压失败
    #[error("解压失败: {0}")]
    DecompressFailed(#[source] std::io::Error),

    /// 文件 I/O 错误
    #[error("文件 I/O 错误: {0}")]
    FileIoError(#[source] std::io::Error),
}

// ============================================================
// 日志错误
// ============================================================

/// 日志模块错误
#[derive(Error, Debug)]
pub enum LogError {
    /// 无效的日志过滤器
    #[error("无效的日志过滤器: {0}")]
    InvalidLogFilter(#[source] tracing_subscriber::filter::ParseError),

    /// 创建滚动文件追加器失败
    #[error("创建滚动文件追加器失败: {0}")]
    CreateRollingFileAppenderFailed(#[source] Box<dyn std::error::Error + Send + Sync>),

    /// 日志初始化失败
    #[error("日志初始化失败: {0}")]
    LoggerInitFailed(#[source] Box<dyn std::error::Error + Send + Sync>),

    /// 路径遍历检测
    #[error("路径遍历检测: {0}")]
    PathTraversalDetected(String),

    /// 无效的日志路径
    #[error("无效的日志路径: 无法解析")]
    InvalidLogPath,
}

// ============================================================
// 存储错误
// ============================================================

/// 存储模块错误
#[derive(Error, Debug)]
pub enum StorageError {
    /// 无效路径
    #[error("无效路径: {0}")]
    InvalidPath(String),

    /// 连接池已初始化
    #[error("连接池已初始化")]
    PoolAlreadyInitialized,

    /// I/O 错误
    #[error("I/O 错误: {0}")]
    IoError(#[from] std::io::Error),

    /// SQLite 错误
    #[error("SQLite 错误: {0}")]
    SqlxError(#[from] sqlx::Error),

    /// 数据库迁移失败
    #[error("数据库迁移失败: {0}")]
    MigrationFailed(#[source] sqlx::migrate::MigrateError),
}

// ============================================================
// DHT / P2P 错误
// ============================================================

/// DHT 存储错误
#[derive(Error, Debug)]
pub enum DhtError {
    /// 序列化/反序列化失败
    #[error("序列化/反序列化失败: {0}")]
    SerializationError(#[from] postcard::Error),

    /// PeerID 解析失败
    #[error("PeerID 解析失败: {0}")]
    PeerIdParseError(#[source] Box<dyn std::error::Error + Send + Sync>),

    /// redb 数据库未初始化
    #[cfg(feature = "redb_dht")]
    #[error("redb 数据库未初始化")]
    DatabaseNotInitialized,

    /// redb 写入事务失败
    #[cfg(feature = "redb_dht")]
    #[error("redb 写入事务失败: {0}")]
    WriteTransactionFailed(#[from] redb::TransactionError),

    /// redb 读取事务失败
    #[cfg(feature = "redb_dht")]
    #[error("redb 读取事务失败: {0}")]
    ReadTransactionFailed(redb::TransactionError),

    /// redb 表操作失败
    #[cfg(feature = "redb_dht")]
    #[error("redb 表操作失败: {0}")]
    TableError(#[from] redb::TableError),

    /// redb 提交失败
    #[cfg(feature = "redb_dht")]
    #[error("redb 提交失败: {0}")]
    CommitError(#[from] redb::CommitError),

    /// redb 存储错误
    #[cfg(feature = "redb_dht")]
    #[error("redb 存储错误: {0}")]
    StorageError(#[from] redb::StorageError),
}

/// P2P 网络错误
#[derive(Error, Debug)]
pub enum P2pError {
    /// Swarm 初始化失败
    #[error("Swarm 初始化失败: {0}")]
    SwarmInitFailed(#[source] Box<dyn std::error::Error + Send + Sync>),

    /// ML-DSA 私钥未缓存
    #[error("ML-DSA 私钥未缓存")]
    MlDsaPrivateKeyNotCached,

    /// 查询联系人 ML-KEM 公钥失败
    #[error("查询联系人 ML-KEM 公钥失败: {0}")]
    MlKemQueryFailed(String),

    /// DHT 中 ML-KEM 公钥格式无效
    #[error("DHT 中 ML-KEM 公钥格式无效: {0}")]
    InvalidMlKemFormat(#[source] hex::FromHexError),

    /// 联系人 ML-KEM 公钥未缓存
    #[error("联系人 ML-KEM 公钥未缓存")]
    MlKemKeyNotCached,
}

// ============================================================
// 消息错误
// ============================================================

/// 消息模块错误
#[derive(Error, Debug)]
pub enum MessageError {
    /// 消息签名失败
    #[error("消息签名失败: {0}")]
    SignFailed(#[source] Box<dyn std::error::Error + Send + Sync>),

    /// 消息验证失败
    #[error("消息验证失败: {0}")]
    VerifyFailed(#[source] Box<dyn std::error::Error + Send + Sync>),

    /// 时间错误
    #[error("时间错误: {0}")]
    TimeError(#[from] std::time::SystemTimeError),

    /// 未从文件读取到数据
    #[error("未从文件读取到数据 (offset={offset})")]
    NoDataRead {
        /// 读取的偏移量
        offset: u64,
    },

    /// 文件 I/O 错误
    #[error("文件 I/O 错误: {0}")]
    FileIoError(#[from] std::io::Error),
}

// ============================================================
// 文件传输错误
// ============================================================

/// 文件传输模块错误
#[derive(Error, Debug)]
pub enum FileTransferError {
    /// 文件 I/O 错误
    #[error("文件 I/O 错误: {0}")]
    FileIoError(#[from] std::io::Error),

    /// 压缩/解压缩错误
    #[error("压缩/解压缩错误: {0}")]
    CompressionError(#[from] CompressionError),

    /// 在指定偏移量未读取到数据
    #[error("在 offset={0} 处未读取到数据")]
    NoDataRead(u64),
}

// ============================================================
// Core 初始化错误
// ============================================================

/// Core 初始化错误
#[derive(Error, Debug)]
pub enum CoreError {
    /// 初始化失败
    #[error("初始化失败: {0}")]
    InitFailed(String),

    /// ML-DSA 私钥未缓存在内存中
    #[error("ML-DSA 私钥未缓存在内存中")]
    MlDsaPrivateKeyNotCached,

    /// 身份错误
    #[error("身份错误: {0}")]
    IdentityError(#[from] IdentityError),

    /// 存储错误
    #[error("存储错误: {0}")]
    StorageError(#[from] StorageError),

    /// P2P 错误
    #[error("P2P 错误: {0}")]
    P2pError(#[from] P2pError),

    /// 签名错误
    #[error("签名错误: {0}")]
    SignatureError(#[from] SignatureError),

    /// 消息错误
    #[error("消息错误: {0}")]
    MessageError(#[from] MessageError),

    /// 文件传输错误
    #[error("文件传输错误: {0}")]
    FileTransferError(#[from] FileTransferError),

    /// I/O 错误
    #[error("I/O 错误: {0}")]
    IoError(#[from] std::io::Error),

    /// 压缩/解压缩错误
    #[error("压缩/解压缩错误: {0}")]
    CompressionError(#[from] CompressionError),

    /// 加密错误
    #[error("加密错误: {0}")]
    CryptoError(#[from] CryptoError),

    /// 联系人离线
    #[error("联系人离线: {0}")]
    ContactOffline(String),

    /// ML-KEM 公钥查询失败
    #[error("ML-KEM 公钥查询失败: {0}")]
    MlKemQueryFailed(String),

    /// ML-KEM 公钥未缓存
    #[error("ML-KEM 公钥未缓存: {0}")]
    MlKemKeyNotCached(String),

    /// 数据库连接不可用
    #[error("数据库连接不可用")]
    DatabaseNotAvailable,

    /// ML-KEM 公钥格式无效
    #[error("ML-KEM 公钥格式无效: {0}")]
    InvalidMlKemFormat(#[source] hex::FromHexError),

    /// 文件传输错误
    #[error("文件传输错误: {0}")]
    FileTransferFailed(String),

    /// 文件哈希验证失败
    #[error("文件哈希验证失败")]
    FileHashMismatch,

    /// 重命名临时文件失败
    #[error("重命名临时文件失败: {0}")]
    FileRenameFailed(String),
}

// ============================================================
// 便捷类型别名
// ============================================================

/// 加密模块 Result 类型
pub type CryptoResult<T> = Result<T, CryptoError>;

/// 身份模块 Result 类型
pub type IdentityResult<T> = Result<T, IdentityError>;

/// 签名模块 Result 类型
pub type SignatureResult<T> = Result<T, SignatureError>;

/// 压缩模块 Result 类型
pub type CompressionResult<T> = Result<T, CompressionError>;

/// 日志模块 Result 类型
pub type LogResult<T> = Result<T, LogError>;

/// 存储模块 Result 类型
pub type StorageResult<T> = Result<T, StorageError>;

/// DHT 模块 Result 类型
pub type DhtResult<T> = Result<T, DhtError>;

/// P2P 模块 Result 类型
pub type P2pResult<T> = Result<T, P2pError>;

/// 消息模块 Result 类型
pub type MessageResult<T> = Result<T, MessageError>;

/// 文件传输模块 Result 类型
pub type FileTransferResult<T> = Result<T, FileTransferError>;

/// Core 模块 Result 类型
pub type CoreResult<T> = Result<T, CoreError>;
