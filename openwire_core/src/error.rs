//! # 错误类型定义
//!
//! 使用 thiserror 为各模块定义结构化错误类型。

use thiserror::Error;

// ============================================================
// 加密/解密错误
// ============================================================

/// 加密/解密模块错误
#[derive(Error, Debug)]
pub enum CryptoError {
    #[error("ML-KEM 公钥无效: {0:?}")]
    InvalidMlKemPublicKey(#[source] aws_lc_rs::error::KeyRejected),

    #[error("KEM 封装失败: {0:?}")]
    KemEncapsulationFailed(#[source] aws_lc_rs::error::Unspecified),

    #[error("AES-GCM 加密失败: {0}")]
    AesGcmEncryptionFailed(String),

    #[error("加密数据为空")]
    EncryptedDataEmpty,

    #[error("不支持的加密版本: {version}, 当前版本为 {current}")]
    UnsupportedEncryptionVersion { version: u8, current: u8 },

    #[error("加密数据太短，无法包含 KEM 密文")]
    EncryptedDataTooShort,

    #[error("KEM 解封装失败: {0:?}")]
    KemDecapsulationFailed(#[source] aws_lc_rs::error::Unspecified),

    #[error("AES-GCM 解密失败: {0}")]
    AesGcmDecryptionFailed(String),
}

// ============================================================
// 身份错误
// ============================================================

/// 身份模块错误
#[derive(Error, Debug)]
pub enum IdentityError {
    #[error("数据库未初始化")]
    DatabaseNotInitialized,

    #[error("生成 ML-KEM-768 密钥对失败: {0:?}")]
    GenerateMlKemKeypairFailed(#[source] aws_lc_rs::error::Unspecified),

    #[error("获取封装密钥失败: {0:?}")]
    GetEncapsulationKeyFailed(#[source] aws_lc_rs::error::Unspecified),

    #[error("序列化 ML-KEM 公钥失败: {0:?}")]
    SerializeMlKemPublicKeyFailed(#[source] aws_lc_rs::error::Unspecified),

    #[error("解析 ML-DSA 私钥失败: {0:?}")]
    ParseMlDsaPrivateKeyFailed(#[source] aws_lc_rs::error::KeyRejected),

    #[error("ML-KEM 公钥应临时生成，不能从私钥提取")]
    MlKemKeyNotExtractable,

    #[error("生成临时 PeerID 失败: {0}")]
    GeneratePeerIdFailed(#[source] Box<dyn std::error::Error + Send + Sync>),

    #[error("根证书身份加载失败: {0}")]
    RootCellIdentityLoadFailed(#[source] Box<dyn std::error::Error + Send + Sync>),

    #[error("签名错误: {0}")]
    SignatureError(#[from] SignatureError),

    #[error("存储错误: {0}")]
    StorageError(#[from] StorageError),
}

// ============================================================
// 签名错误
// ============================================================

/// 签名/验证模块错误
#[derive(Error, Debug)]
pub enum SignatureError {
    #[error("生成 ML-DSA 密钥对失败: {0:?}")]
    GenerateMlDsaKeypairFailed(#[source] aws_lc_rs::error::Unspecified),

    #[error("解析 ML-DSA 私钥失败: {0:?}")]
    ParseMlDsaPrivateKeyFailed(#[source] aws_lc_rs::error::KeyRejected),

    #[error("签名数据失败: {0:?}")]
    SignDataFailed(#[source] aws_lc_rs::error::Unspecified),

    #[error("验证签名失败: {0:?}")]
    VerifySignatureFailed(#[source] aws_lc_rs::error::Unspecified),

    #[error("时间错误: {0:?}")]
    TimeError(#[source] std::time::SystemTimeError),

    #[error("生成盐值失败: {0:?}")]
    GenerateSaltFailed(#[source] aws_lc_rs::error::Unspecified),

    #[error("签名数据太短")]
    SignatureDataTooShort,

    #[error("签名数据不完整")]
    SignatureDataIncomplete,

    #[error("无效的 ML-DSA 65 公钥长度: 期望 {expected}, 实际 {actual}")]
    InvalidPublicKeyLength { expected: usize, actual: usize },

    #[error("私钥为空")]
    EmptyPrivateKey,

    #[error("序列化 ML-DSA 公钥失败: {0:?}")]
    DeserializePublicKeyFailed(#[source] aws_lc_rs::error::Unspecified),

    #[error("反序列化 ML-DSA 私钥失败: {0:?}")]
    DeserializePrivateKeyFailed(#[source] aws_lc_rs::error::Unspecified),
}

// ============================================================
// 压缩错误
// ============================================================

/// 压缩/解压缩模块错误
#[derive(Error, Debug)]
pub enum CompressionError {
    #[error("压缩数据为空")]
    CompressedDataEmpty,

    #[error("压缩数据过大: {0} 字节")]
    CompressedDataTooLarge(usize),

    #[error("解压后数据超过大小限制: {0} 字节")]
    DecompressedDataTooLarge(usize),

    #[error("压缩失败: {0}")]
    CompressFailed(#[source] std::io::Error),

    #[error("解压失败: {0}")]
    DecompressFailed(#[source] std::io::Error),

    #[error("文件 I/O 错误: {0}")]
    FileIoError(#[source] std::io::Error),
}

// ============================================================
// 日志错误
// ============================================================

/// 日志模块错误
#[derive(Error, Debug)]
pub enum LogError {
    #[error("无效的日志过滤器: {0}")]
    InvalidLogFilter(#[source] tracing_subscriber::filter::ParseError),

    #[error("创建滚动文件追加器失败: {0}")]
    CreateRollingFileAppenderFailed(#[source] Box<dyn std::error::Error + Send + Sync>),

    #[error("日志初始化失败: {0}")]
    LoggerInitFailed(#[source] Box<dyn std::error::Error + Send + Sync>),

    #[error("路径遍历检测: {0}")]
    PathTraversalDetected(String),

    #[error("无效的日志路径: 无法解析")]
    InvalidLogPath,
}

// ============================================================
// 存储错误
// ============================================================

/// 存储模块错误
#[derive(Error, Debug)]
pub enum StorageError {
    #[error("无效路径: {0}")]
    InvalidPath(String),

    #[error("数据库路径必须是文件")]
    DatabasePathMustBeFile,

    #[error("连接池已初始化")]
    PoolAlreadyInitialized,

    #[error("数据库连接不可用")]
    DatabaseUnavailable,

    #[error("批量大小过大")]
    BatchSizeTooLarge,

    #[error("I/O 错误: {0}")]
    IoError(#[from] std::io::Error),

    #[error("SQLite 错误: {0}")]
    SqlxError(#[from] sqlx::Error),

    #[error("数据库迁移失败: {0}")]
    MigrationFailed(#[source] sqlx::migrate::MigrateError),
}

// ============================================================
// DHT / P2P 错误
// ============================================================

/// DHT 存储错误
#[derive(Error, Debug)]
pub enum DhtError {
    #[error("创建 DHT 数据库失败: {0}")]
    CreateDatabaseFailed(#[source] redb::DatabaseError),

    #[error("DHT 数据库连接未初始化")]
    DatabaseNotInitialized,

    #[error("DHT 数据库写入事务失败: {0}")]
    WriteTransactionFailed(#[from] redb::TransactionError),

    #[error("DHT 数据库读取事务失败: {0}")]
    ReadTransactionFailed(#[source] redb::TransactionError),

    #[error("DHT 数据库表错误: {0}")]
    TableError(#[from] redb::TableError),

    #[error("DHT 提交事务失败: {0}")]
    CommitError(#[from] redb::CommitError),

    #[error("DHT 存储错误: {0}")]
    StoreError(#[from] libp2p::kad::store::Error),

    #[error("DHT 数据库存储操作失败: {0}")]
    StorageError(#[from] redb::StorageError),

    #[error("序列化/反序列化失败: {0}")]
    SerializationError(#[from] postcard::Error),

    #[error("Hex 解码失败: {0}")]
    HexDecodeError(#[from] hex::FromHexError),

    #[error("PeerID 解析失败: {0}")]
    PeerIdParseError(#[source] Box<dyn std::error::Error + Send + Sync>),

    #[error("DHT 记录验证失败: {0}")]
    ValidationFailed(String),
}

/// P2P 网络错误
#[derive(Error, Debug)]
pub enum P2pError {
    #[error("Swarm 初始化失败: {0}")]
    SwarmInitFailed(#[source] Box<dyn std::error::Error + Send + Sync>),

    #[error("Kademlia 创建失败: {0}")]
    KademliaCreateFailed(#[source] Box<dyn std::error::Error + Send + Sync>),

    #[error("DNS 地址解析失败: {0}")]
    DnsResolveFailed(#[source] Box<dyn std::error::Error + Send + Sync>),

    #[error("ML-DSA 私钥未缓存")]
    MlDsaPrivateKeyNotCached,

    #[error("DHT 查询失败: {0}")]
    DhtQueryFailed(String),

    #[error("查询联系人 ML-KEM 公钥失败: {0}")]
    MlKemQueryFailed(String),

    #[error("DHT 中 ML-KEM 公钥格式无效: {0}")]
    InvalidMlKemFormat(#[source] hex::FromHexError),

    #[error("联系人 ML-KEM 公钥未缓存")]
    MlKemKeyNotCached,
}

// ============================================================
// 消息错误
// ============================================================

/// 消息模块错误
#[derive(Error, Debug)]
pub enum MessageError {
    #[error("消息签名失败: {0}")]
    SignFailed(#[source] Box<dyn std::error::Error + Send + Sync>),

    #[error("消息验证失败: {0}")]
    VerifyFailed(#[source] Box<dyn std::error::Error + Send + Sync>),

    #[error("时间错误: {0}")]
    TimeError(#[from] std::time::SystemTimeError),

    #[error("序列化/反序列化失败: {0}")]
    SerializationError(#[from] postcard::Error),

    #[error("文件 ID 不匹配: 期望 {expected:?}, 实际 {actual:?}")]
    FileIdMismatch {
        expected: [u8; 32],
        actual: [u8; 32],
    },

    #[error("分片 {chunk_index} 哈希不匹配: 数据完整性检查失败")]
    ChunkHashMismatch { chunk_index: u32 },

    #[error("未从文件读取到数据 (offset={offset})")]
    NoDataRead { offset: u64 },

    #[error("文件 I/O 错误: {0}")]
    FileIoError(#[from] std::io::Error),

    #[error("DHT 错误: {0}")]
    DhtError(#[from] DhtError),
}

// ============================================================
// 文件传输错误
// ============================================================

/// 文件传输模块错误
#[derive(Error, Debug)]
pub enum FileTransferError {
    #[error("拒绝不安全的文件名: '{filename}' (file_id: {file_id}..)")]
    UnsafeFilename { filename: String, file_id: String },

    #[error("拒绝无效的分片元数据: total_chunks=0 (file_id: {0}..)")]
    InvalidChunkMetadata(String),

    #[error(
        "拒绝无效的分片索引: chunk_index={chunk_index} >= total_chunks={total_chunks} (file_id: {file_id}..)"
    )]
    InvalidChunkIndex {
        chunk_index: u32,
        total_chunks: u32,
        file_id: String,
    },

    #[error("拒绝无效的分片大小: chunk_size=0 (file_id: {0}..)")]
    InvalidChunkSize(String),

    #[error(
        "拒绝 offset 不匹配的分片: chunk_index={chunk_index}, offset={offset}, expected_offset={expected_offset} (file_id: {file_id}..)"
    )]
    OffsetMismatch {
        chunk_index: u32,
        offset: u64,
        expected_offset: u64,
        file_id: String,
    },

    #[error("拒绝无效的文件总大小: total_size=0 (file_id: {0}..)")]
    InvalidTotalSize(String),

    #[error(
        "拒绝过大的文件: total_size={total_size} > MAX_FILE_SIZE={max_size} (file_id: {file_id}..)"
    )]
    FileTooLarge {
        total_size: u64,
        max_size: u64,
        file_id: String,
    },

    #[error("拒绝最后一个分片: offset={offset} > total_size={total_size} (file_id: {file_id}..)")]
    LastChunkOffsetExceeded {
        offset: u64,
        total_size: u64,
        file_id: String,
    },

    #[error(
        "拒绝过大的最后一个分片: decompressed_size={decompressed_size} > chunk_size={chunk_size} (file_id: {file_id}..)"
    )]
    LastChunkTooLarge {
        decompressed_size: u64,
        chunk_size: u64,
        file_id: String,
    },

    #[error(
        "拒绝导致文件过大的最后一个分片: offset={offset} + decompressed_size={decompressed_size} > total_size={total_size} (file_id: {file_id}..)"
    )]
    LastChunkWouldExceedFile {
        offset: u64,
        decompressed_size: u64,
        total_size: u64,
        file_id: String,
    },

    #[error(
        "拒绝大小不匹配的分片: decompressed_size={decompressed_size} != chunk_size={chunk_size} (file_id: {file_id}..)"
    )]
    ChunkSizeMismatch {
        decompressed_size: u64,
        chunk_size: u64,
        file_id: String,
    },

    #[error("计算文件哈希失败: {0}")]
    HashComputationFailed(#[source] std::io::Error),

    #[error("文件哈希验证失败，文件可能已损坏")]
    HashVerificationFailed,

    #[error("重命名临时文件失败: {0}")]
    RenameTempFileFailed(#[source] std::io::Error),

    #[error("文件 I/O 错误: {0}")]
    FileIoError(#[from] std::io::Error),

    #[error("序列化/反序列化失败: {0}")]
    SerializationError(#[from] postcard::Error),

    #[error("压缩/解压缩错误: {0}")]
    CompressionError(#[from] CompressionError),

    #[error("在 offset={0} 处未读取到数据")]
    NoDataRead(u64),

    #[error("文件 ID 不匹配: expected={expected:?}.., got={got:?}..")]
    FileIdMismatch { expected: [u8; 32], got: [u8; 32] },

    #[error("分片 {0} 哈希校验失败")]
    ChunkHashMismatch(u32),
}

// ============================================================
// Core 初始化错误
// ============================================================

/// Core 初始化错误
#[derive(Error, Debug)]
pub enum CoreError {
    #[error("初始化失败: {0}")]
    InitFailed(String),

    #[error("日志初始化失败: {0}")]
    LogInitFailed(#[source] LogError),

    #[error("创建 DHT 数据库失败 {path:?}: {source}")]
    DhtDatabaseCreateFailed {
        path: std::path::PathBuf,
        #[source]
        source: redb::Error,
    },

    #[error("DHT 数据库连接未初始化")]
    DhtDatabaseNotInitialized,

    #[error("ML-DSA 私钥未缓存在内存中")]
    MlDsaPrivateKeyNotCached,

    #[error("身份错误: {0}")]
    IdentityError(#[from] IdentityError),

    #[error("存储错误: {0}")]
    StorageError(#[from] StorageError),

    #[error("P2P 错误: {0}")]
    P2pError(#[from] P2pError),

    #[error("签名错误: {0}")]
    SignatureError(#[from] SignatureError),

    #[error("消息错误: {0}")]
    MessageError(#[from] MessageError),

    #[error("文件传输错误: {0}")]
    FileTransferError(#[from] FileTransferError),

    #[error("I/O 错误: {0}")]
    IoError(#[from] std::io::Error),

    #[error("压缩/解压缩错误: {0}")]
    CompressionError(#[from] CompressionError),

    #[error("加密错误: {0}")]
    CryptoError(#[from] CryptoError),

    #[error("DHT 错误: {0}")]
    DhtError(#[from] DhtError),

    #[error("联系人离线: {0}")]
    ContactOffline(String),

    #[error("ML-KEM 公钥查询失败: {0}")]
    MlKemQueryFailed(String),

    #[error("ML-KEM 公钥未缓存: {0}")]
    MlKemKeyNotCached(String),

    #[error("数据库连接不可用")]
    DatabaseNotAvailable,

    #[error("ML-KEM 公钥格式无效: {0}")]
    InvalidMlKemFormat(#[source] hex::FromHexError),

    #[error("文件传输错误: {0}")]
    FileTransferFailed(String),

    #[error("文件哈希验证失败")]
    FileHashMismatch,

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
