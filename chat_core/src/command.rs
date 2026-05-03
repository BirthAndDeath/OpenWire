use crate::message::ChatMessageType;
use std::path::PathBuf;
use tokio::sync::oneshot;

/// 文件传输进度事件（结构化数据）
///
/// chat_core 通过此结构体向上层传递进度信息，
/// 上层（chat_cli/chat_tauri）负责序列化为 JSON 供前端消费。
///
/// # 序列化说明
/// - `Serialize` derive 用于上层（chat_cli/chat_tauri）的 JSON 序列化需求
/// - chat_core 自身不依赖 serde_json，仅使用 serde derive
#[derive(Debug, Clone, serde::Serialize)]
pub struct FileTransferProgress {
    pub filename: String,
    pub chunk_index: u32,
    pub total_chunks: u32,
    pub received_bytes: u64,
    pub total_size: u64,
    pub status: String, // "downloading" | "completed" | "error"
}

/// 控制命令：外部向核心发送的指令
#[derive(Debug)]
pub enum ChatCommand {
    /// 发送消息到网络,由核心封装签名/时间戳/hash
    SendMessage {
        /// 接收方的 ML-DSA 公钥 hex（唯一标识联系人，用于查找 PeerID）
        mldsa_pubkey_hex: String,
        msgtype: ChatMessageType,
        data: Vec<u8>,
    },

    /// 添加好友（交换公钥）
    AddContact {
        /// 联系人的 ML-DSA 公钥 hex（唯一标识）
        mldsa_pubkey_hex: String,
        /// 联系人的 ML-KEM 公钥（临时密钥交换）
        mlkem_public_key: Vec<u8>,
        name: Option<String>,
        /// 响应通道：操作完成后发送结果 (true=成功, false=失败)
        resp: oneshot::Sender<bool>,
    },

    /// 生成新身份
    GenerateIdentity,
    /// 选择当前身份
    SelectIdentity { identity_id: String },
    /// 删除身份
    DeleteIdentity { identity_id: String },

    /// 请求文件下载（接收方发起）
    ///
    /// 安全说明：下载目录由 SetDownloadDir 命令统一管理，
    /// 不从请求中接受 download_dir 参数，防止路径遍历攻击。
    RequestFileDownload {
        /// 发送方的 ML-DSA 公钥 hex（谁分享的文件）
        sender_mldsa_pubkey_hex: String,
        /// 文件唯一标识
        file_id: [u8; 32],
    },

    /// 设置下载目录
    SetDownloadDir { path: PathBuf },

    /// 注册文件供下载（发送方在发送 FileHash 后调用，记录文件路径）
    RegisterFileForDownload {
        /// 文件唯一标识
        file_id: [u8; 32],
        /// 本地文件路径
        file_path: PathBuf,
    },

    /// 通过 DHT 发布身份记录到 Kademlia 网络
    ///
    /// 将当前身份的 ML-DSA 公钥 -> PeerID 映射发布到 DHT 网络，
    /// 使其他节点可以通过公钥查询到当前节点的 PeerID。
    DhtPublishIdentity {
        /// ML-DSA 公钥 hex（记录键）
        mldsa_pubkey_hex: String,
        /// 当前 PeerID（记录值，序列化为字符串）
        peer_id: String,
        /// ML-KEM 公钥 hex（记录值，序列化为字符串）
        mlkem_pubkey_hex: String,
    },

    /// 通过 DHT 发现联系人
    ///
    /// 通过 Kademlia get_record 查询联系人的 PeerID 和 ML-KEM 公钥，
    /// 如果找到则自动添加联系人。
    DiscoverContact {
        /// 联系人的 ML-DSA 公钥 hex
        mldsa_pubkey_hex: String,
        /// 联系人名称（可选）
        name: Option<String>,
    },

    /// 重试发送所有待发送消息（离线消息队列）
    RetryPendingMessages,

    /// 优雅关闭核心
    Shutdown,
}

/// 消息事件类型：用于向外部（UI）通知状态
///
/// chat_core 通过此枚举向上层传递结构化数据，
/// 上层（chat_cli/chat_tauri）负责序列化为 JSON 供前端消费。
#[derive(Debug)]
pub enum MessageEvent {
    /// 收到新消息
    ReceiveMessage(String),
    /// 发生错误
    Error(String),
    /// 日志信息（连接状态等）
    Log(String),
    /// 警告信息
    Warning(String),
    /// 文件传输进度（结构化数据，上层负责序列化为 JSON）
    FileTransferProgress(FileTransferProgress),
}

/// 通道消息结构：核心向外部（UI）发送的事件包装
pub type ChatcoreEvent = MessageEvent;
