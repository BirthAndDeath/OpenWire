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

/// 收到的消息类型：chat_core 向上层传递的结构化数据
///
/// 上层（chat_cli/chat_tauri）负责序列化为 JSON 供前端消费。
/// chat_core 自身不依赖 serde_json，仅使用 serde derive。
///
/// 注意：OnlineStatus 已移出此枚举，改为 MessageEvent 的独立变体，
/// 避免在线状态更新被误当作聊天消息显示在消息历史中。
/// 使用 serde 内部标记枚举格式，序列化为 `{"type":"text","text":"...","sender":"..."}` 而非 `{"Text":{...}}`
#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "type")]
pub enum IncomingMessage {
    /// 文本消息
    #[serde(rename = "text")]
    Text {
        /// 消息文本内容
        text: String,
        /// 发送方的 ML-DSA 公钥 hex
        sender: String,
    },
    /// 文件分享消息
    #[serde(rename = "file_hash")]
    FileShare {
        /// 文件名
        filename: String,
        /// 文件唯一标识（hex）
        file_id: String,
        /// 文件哈希（hex）
        file_hash: String,
        /// 文件总大小
        total_size: u64,
        /// 发送方的 ML-DSA 公钥 hex
        sender: String,
    },
    /// 消息送达回执
    #[serde(rename = "delivery_receipt")]
    DeliveryReceipt {
        /// 已送达消息的哈希
        message_hash: String,
        /// 发送方的 ML-DSA 公钥 hex
        peer_id: String,
    },
    /// 消息已发送通知（包含消息哈希，用于前端匹配送达回执）
    #[serde(rename = "message_sent")]
    MessageSent {
        /// 已发送消息的哈希
        message_hash: String,
        /// 接收方的 ML-DSA 公钥 hex
        peer_id: String,
    },
}

/// 消息事件类型：用于向外部（UI）通知状态
///
/// chat_core 通过此枚举向上层传递结构化数据，
/// 上层（chat_cli/chat_tauri）负责序列化为 JSON 供前端消费。
#[derive(Debug)]
pub enum MessageEvent {
    /// 收到新消息（结构化数据，上层负责序列化）
    ReceiveMessage(IncomingMessage),
    /// 在线状态更新（独立事件，不混入消息历史）
    ///
    /// 包含当前所有在线联系人的 ML-DSA 公钥 hex 列表，
    /// 上层据此更新每个联系人的在线/离线状态指示器。
    OnlineStatus {
        /// 当前在线联系人的 ML-DSA 公钥 hex 列表
        online_contacts: Vec<String>,
    },
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
