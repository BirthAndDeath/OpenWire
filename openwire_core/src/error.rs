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

    /// 生成临时 PeerID 失败
    #[error("生成临时 PeerID 失败: {0}")]
    GeneratePeerIdFailed(#[source] Box<dyn std::error::Error + Send + Sync>),

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

    /// 验证签名失败
    #[error("验证签名失败: {0:?}")]
    VerifySignatureFailed(#[source] aws_lc_rs::error::Unspecified),

    /// 时间错误
    #[error("时间错误: {0:?}")]
    TimeError(#[source] std::time::SystemTimeError),

    /// 生成盐值失败
    #[error("生成盐值失败: {0:?}")]
    GenerateSaltFailed(#[source] aws_lc_rs::error::Unspecified),

    /// 签名数据太短
    #[error("签名数据太短")]
    SignatureDataTooShort,

    /// 签名数据不完整
    #[error("签名数据不完整")]
    SignatureDataIncomplete,

    /// 无效的 ML-DSA 公钥长度
    #[error("无效的 ML-DSA 65 公钥长度: 期望 {expected}, 实际 {actual}")]
    InvalidPublicKeyLength {
        /// 期望的公钥长度
        expected: usize,
        /// 实际的公钥长度
        actual: usize,
    },

    /// 私钥为空
    #[error("私钥为空")]
    EmptyPrivateKey,

    /// 序列化 ML-DSA 公钥失败
    #[error("序列化 ML-DSA 公钥失败: {0:?}")]
    DeserializePublicKeyFailed(#[source] aws_lc_rs::error::Unspecified),

    /// 反序列化 ML-DSA 私钥失败
    #[error("反序列化 ML-DSA 私钥失败: {0:?}")]
    DeserializePrivateKeyFailed(#[source] aws_lc_rs::error::Unspecified),
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

    /// 数据库路径必须是文件
    #[error("数据库路径必须是文件")]
    DatabasePathMustBeFile,

    /// 连接池已初始化
    #[error("连接池已初始化")]
    PoolAlreadyInitialized,

    /// 数据库连接不可用
    #[error("数据库连接不可用")]
    DatabaseUnavailable,

    /// 批量大小过大
    #[error("批量大小过大")]
    BatchSizeTooLarge,

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
    /// 创建 DHT 数据库失败
    #[error("创建 DHT 数据库失败: {0}")]
    CreateDatabaseFailed(#[source] redb::DatabaseError),

    /// DHT 数据库连接未初始化
    #[error("DHT 数据库连接未初始化")]
    DatabaseNotInitialized,

    /// DHT 数据库写入事务失败
    #[error("DHT 数据库写入事务失败: {0}")]
    WriteTransactionFailed(#[from] redb::TransactionError),

    /// DHT 数据库读取事务失败
    #[error("DHT 数据库读取事务失败: {0}")]
    ReadTransactionFailed(#[source] redb::TransactionError),

    /// DHT 数据库表错误
    #[error("DHT 数据库表错误: {0}")]
    TableError(#[from] redb::TableError),

    /// DHT 提交事务失败
    #[error("DHT 提交事务失败: {0}")]
    CommitError(#[from] redb::CommitError),

    /// DHT 存储错误
    #[error("DHT 存储错误: {0}")]
    StoreError(#[from] libp2p::kad::store::Error),

    /// DHT 数据库存储操作失败
    #[error("DHT 数据库存储操作失败: {0}")]
    StorageError(#[from] redb::StorageError),

    /// 序列化/反序列化失败
    #[error("序列化/反序列化失败: {0}")]
    SerializationError(#[from] postcard::Error),

    /// Hex 解码失败
    #[error("Hex 解码失败: {0}")]
    HexDecodeError(#[from] hex::FromHexError),

    /// PeerID 解析失败
    #[error("PeerID 解析失败: {0}")]
    PeerIdParseError(#[source] Box<dyn std::error::Error + Send + Sync>),

    /// DHT 记录验证失败
    #[error("DHT 记录验证失败: {0}")]
    ValidationFailed(String),
}

/// P2P 网络错误
#[derive(Error, Debug)]
pub enum P2pError {
    /// Swarm 初始化失败
    #[error("Swarm 初始化失败: {0}")]
    SwarmInitFailed(#[source] Box<dyn std::error::Error + Send + Sync>),

    /// Kademlia 创建失败
    #[error("Kademlia 创建失败: {0}")]
    KademliaCreateFailed(#[source] Box<dyn std::error::Error + Send + Sync>),

    /// DNS 地址解析失败
    #[error("DNS 地址解析失败: {0}")]
    DnsResolveFailed(#[source] Box<dyn std::error::Error + Send + Sync>),

    /// ML-DSA 私钥未缓存
    #[error("ML-DSA 私钥未缓存")]
    MlDsaPrivateKeyNotCached,

    /// DHT 查询失败
    #[error("DHT 查询失败: {0}")]
    DhtQueryFailed(String),

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

    /// 序列化/反序列化失败
    #[error("序列化/反序列化失败: {0}")]
    SerializationError(#[from] postcard::Error),

    /// 文件 ID 不匹配
    #[error("文件 ID 不匹配: 期望 {expected:?}, 实际 {actual:?}")]
    FileIdMismatch {
        /// 期望的文件 ID
        expected: [u8; 32],
        /// 实际的文件 ID
        actual: [u8; 32],
    },

    /// 分片哈希不匹配
    #[error("分片 {chunk_index} 哈希不匹配: 数据完整性检查失败")]
    ChunkHashMismatch {
        /// 分片索引
        chunk_index: u32,
    },

    /// 未从文件读取到数据
    #[error("未从文件读取到数据 (offset={offset})")]
    NoDataRead {
        /// 读取的偏移量
        offset: u64,
    },

    /// 文件 I/O 错误
    #[error("文件 I/O 错误: {0}")]
    FileIoError(#[from] std::io::Error),

    /// DHT 错误
    #[error("DHT 错误: {0}")]
    DhtError(#[from] DhtError),
}

// ============================================================
// 文件传输错误
// ============================================================

/// 文件传输模块错误
#[derive(Error, Debug)]
pub enum FileTransferError {
    /// 拒绝不安全的文件名
    #[error("拒绝不安全的文件名: '{filename}' (file_id: {file_id}..)")]
    UnsafeFilename {
        /// 文件名
        filename: String,
        /// 文件 ID
        file_id: String,
    },

    /// 拒绝无效的分片元数据
    #[error("拒绝无效的分片元数据: total_chunks=0 (file_id: {0}..)")]
    InvalidChunkMetadata(String),

    /// 拒绝无效的分片索引
    #[error(
        "拒绝无效的分片索引: chunk_index={chunk_index} >= total_chunks={total_chunks} (file_id: {file_id}..)"
    )]
    InvalidChunkIndex {
        /// 分片索引
        chunk_index: u32,
        /// 分片总数
        total_chunks: u32,
        /// 文件 ID
        file_id: String,
    },

    /// 拒绝无效的分片大小
    #[error("拒绝无效的分片大小: chunk_size=0 (file_id: {0}..)")]
    InvalidChunkSize(String),

    /// 拒绝 offset 不匹配的分片
    #[error(
        "拒绝 offset 不匹配的分片: chunk_index={chunk_index}, offset={offset}, expected_offset={expected_offset} (file_id: {file_id}..)"
    )]
    OffsetMismatch {
        /// 分片索引
        chunk_index: u32,
        /// 分片偏移量
        offset: u64,
        /// 期望的分片偏移量
        expected_offset: u64,
        /// 文件 ID
        file_id: String,
    },

    /// 拒绝无效的文件总大小
    #[error("拒绝无效的文件总大小: total_size=0 (file_id: {0}..)")]
    InvalidTotalSize(String),

    /// 拒绝过大的文件
    #[error(
        "拒绝过大的文件: total_size={total_size} > MAX_FILE_SIZE={max_size} (file_id: {file_id}..)"
    )]
    FileTooLarge {
        /// 文件总大小
        total_size: u64,
        /// 允许的最大文件大小
        max_size: u64,
        /// 文件 ID
        file_id: String,
    },

    /// 拒绝最后一个分片：offset 超过 total_size
    #[error("拒绝最后一个分片: offset={offset} > total_size={total_size} (file_id: {file_id}..)")]
    LastChunkOffsetExceeded {
        /// 分片偏移量
        offset: u64,
        /// 文件总大小
        total_size: u64,
        /// 文件 ID
        file_id: String,
    },

    /// 拒绝过大的最后一个分片
    #[error(
        "拒绝过大的最后一个分片: decompressed_size={decompressed_size} > chunk_size={chunk_size} (file_id: {file_id}..)"
    )]
    LastChunkTooLarge {
        /// 解压后的数据大小
        decompressed_size: u64,
        /// 分片大小
        chunk_size: u64,
        /// 文件 ID
        file_id: String,
    },

    /// 拒绝导致文件过大的最后一个分片
    #[error(
        "拒绝导致文件过大的最后一个分片: offset={offset} + decompressed_size={decompressed_size} > total_size={total_size} (file_id: {file_id}..)"
    )]
    LastChunkWouldExceedFile {
        /// 分片偏移量
        offset: u64,
        /// 解压后的数据大小
        decompressed_size: u64,
        /// 文件总大小
        total_size: u64,
        /// 文件 ID
        file_id: String,
    },

    /// 拒绝大小不匹配的分片
    #[error(
        "拒绝大小不匹配的分片: decompressed_size={decompressed_size} != chunk_size={chunk_size} (file_id: {file_id}..)"
    )]
    ChunkSizeMismatch {
        /// 解压后的数据大小
        decompressed_size: u64,
        /// 分片大小
        chunk_size: u64,
        /// 文件 ID
        file_id: String,
    },

    /// 计算文件哈希失败
    #[error("计算文件哈希失败: {0}")]
    HashComputationFailed(#[source] std::io::Error),

    /// 文件哈希验证失败，文件可能已损坏
    #[error("文件哈希验证失败，文件可能已损坏")]
    HashVerificationFailed,

    /// 重命名临时文件失败
    #[error("重命名临时文件失败: {0}")]
    RenameTempFileFailed(#[source] std::io::Error),

    /// 文件 I/O 错误
    #[error("文件 I/O 错误: {0}")]
    FileIoError(#[from] std::io::Error),

    /// 序列化/反序列化失败
    #[error("序列化/反序列化失败: {0}")]
    SerializationError(#[from] postcard::Error),

    /// 压缩/解压缩错误
    #[error("压缩/解压缩错误: {0}")]
    CompressionError(#[from] CompressionError),

    /// 在指定偏移量未读取到数据
    #[error("在 offset={0} 处未读取到数据")]
    NoDataRead(u64),

    /// 文件 ID 不匹配
    #[error("文件 ID 不匹配: expected={expected:?}.., got={got:?}..")]
    FileIdMismatch {
        /// 期望的文件 ID
        expected: [u8; 32],
        /// 实际得到的文件 ID
        got: [u8; 32],
    },

    /// 分片哈希校验失败
    #[error("分片 {0} 哈希校验失败")]
    ChunkHashMismatch(u32),
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

    /// 日志初始化失败
    #[error("日志初始化失败: {0}")]
    LogInitFailed(#[source] LogError),

    /// 创建 DHT 数据库失败
    #[error("创建 DHT 数据库失败 {path:?}: {source}")]
    DhtDatabaseCreateFailed {
        /// 数据库路径
        path: std::path::PathBuf,
        /// 底层错误
        #[source]
        source: redb::Error,
    },

    /// DHT 数据库连接未初始化
    #[error("DHT 数据库连接未初始化")]
    DhtDatabaseNotInitialized,

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

    /// DHT 错误
    #[error("DHT 错误: {0}")]
    DhtError(#[from] DhtError),

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
