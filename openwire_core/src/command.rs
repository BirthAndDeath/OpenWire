use crate::message::ChatMessageType;
use std::path::PathBuf;
use tokio::sync::oneshot;

/// 文件分享消息在数据库 content 字段中的存储格式前缀
/// 格式: "[文件] {filename} [hash:{64-hex-chars}]"
/// 解析方通过此格式反序列化出文件元信息。
pub const FILE_SHARE_CONTENT_PREFIX: &str = "[文件] ";
/// 文件分享消息在数据库 content 字段中 hash 部分的前缀标记
pub const FILE_SHARE_HASH_PREFIX: &str = " [hash:";

/// 文件传输进度状态
#[derive(Debug, Clone, serde::Serialize)]
pub enum TransferProgressStatus {
    /// 下载中
    #[serde(rename = "downloading")]
    Downloading,
    /// 已完成
    #[serde(rename = "completed")]
    Completed,
    /// 失败
    #[serde(rename = "error")]
    Error,
}

impl std::fmt::Display for TransferProgressStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TransferProgressStatus::Downloading => write!(f, "downloading"),
            TransferProgressStatus::Completed => write!(f, "completed"),
            TransferProgressStatus::Error => write!(f, "error"),
        }
    }
}

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
    /// 文件名
    pub filename: String,
    /// 当前已完成的分片索引
    pub chunk_index: u32,
    /// 分片总数
    pub total_chunks: u32,
    /// 已接收的字节数
    pub received_bytes: u64,
    /// 文件总大小（字节）
    pub total_size: u64,
    /// 传输状态
    pub status: TransferProgressStatus,
}

/// 控制命令：外部向核心发送的指令
#[derive(Debug)]
pub enum ChatCommand {
    /// 发送消息到网络,由核心封装签名/时间戳/hash
    SendMessage {
        /// 接收方的 ML-DSA 公钥 hex（唯一标识联系人，用于查找 PeerID）
        mldsa_pubkey_hex: String,
        /// 消息类型
        msgtype: ChatMessageType,
        /// 消息负载数据
        data: Vec<u8>,
    },

    /// 添加好友（交换公钥）
    AddContact {
        /// 联系人的 ML-DSA 公钥 hex（唯一标识）
        mldsa_pubkey_hex: String,
        /// 联系人的 ML-KEM 公钥（临时密钥交换）
        mlkem_public_key: Vec<u8>,
        /// 联系人名称（可选）
        name: Option<String>,
        /// 响应通道：操作完成后发送结果 (true=成功, false=失败)
        resp: oneshot::Sender<bool>,
    },

    /// 生成新身份
    GenerateIdentity,
    /// 选择当前身份
    SelectIdentity {
        /// 要选择的身份 ID
        identity_id: String,
    },
    /// 删除身份
    DeleteIdentity {
        /// 要删除的身份 ID
        identity_id: String,
    },

    /// 请求文件下载（接收方发起）
    RequestFileDownload {
        /// 发送方的 ML-DSA 公钥 hex（谁分享的文件）
        sender_mldsa_pubkey_hex: String,
        /// 文件的 SHA256 哈希
        file_hash: [u8; 32],
        /// 保存路径（含文件名）
        save_path: PathBuf,
    },

    /// 通过 DHT 发布身份到 Kademlia 网络
    ///
    /// 使用 SHA256(ML-DSA 公钥) 作为 provider key，隐藏原始公钥。
    /// ML-KEM 公钥不再存入 DHT，改为通过 FriendOnline 直接传递。
    DhtPublishIdentity {
        /// ML-DSA 公钥 hex
        mldsa_pubkey_hex: String,
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

    /// 设置计费网络检测模式：free（非计费）/ paid（计费）/ disabled（禁用）
    /// 禁用时中继始终关闭，优先于 API 自动检测与用户手动选择
    SetPaidNetworkMode(String),
    /// 设置中继角色："server" / "client" / "off"（互斥，server 与 client 不能同时启用）
    SetRelayRole(String),

    /// 查询网络状态（用于前端网络监控组件）
    GetNetworkStatus {
        /// 响应通道：返回 JSON 序列化的 NetworkStatusData
        resp: tokio::sync::oneshot::Sender<String>,
    },

    /// 导出当前路由表（用于分享给其他节点）
    ExportRoutingTable {
        /// 响应通道：返回 JSON 序列化的 RoutingTableExport
        resp: tokio::sync::oneshot::Sender<String>,
    },
    /// 导入路由表（将其他节点导出的 peers 加入本地路由表）
    ImportRoutingTable {
        /// 导出的路由表 JSON 字符串
        data: String,
        /// 响应通道：返回 JSON 序列化的导入结果 { imported, error }
        resp: tokio::sync::oneshot::Sender<String>,
    },

    // ===== 定时器事件（由 timers.rs 触发，不对外暴露） =====
    /// 定时器：保存路由表到磁盘
    TimerSaveRoutingTable,
    /// 定时器：重新发现所有联系人的 DHT 记录
    TimerDiscoverAllContacts,
    /// 定时器：清理过期 DHT 记录
    TimerCleanupDht,
    /// 定时器：将当前身份重新发布到 DHT
    TimerPublishIdentity,
    /// 定时器：随机刷新路由表（随机桶查询，扩展路由表覆盖）
    TimerRefreshRoutingTable,
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
    /// 单个联系人的在线状态变更通知（来自 gossipsub）
    ContactOnlineStatus {
        /// 联系人 ML-DSA 公钥 hex
        mldsa_pubkey_hex: String,
        /// 是否在线
        online: bool,
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

// ============================================================================
// 路由表导出/导入
// ============================================================================

/// 导出文件中的单个节点信息
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RoutingTableExportPeer {
    pub peer_id: String,
    pub addresses: Vec<String>,
    pub is_bootstrap: bool,
    pub is_relay: bool,
}

/// 路由表导出文件格式（JSON）
/// 注意：此文件不含任何密钥，仅含公网可发现的 PeerID 和 Multiaddr。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RoutingTableExport {
    pub version: u32,
    pub exported_at: i64,
    pub self_peer_id: String,
    pub self_addresses: Vec<String>,
    pub peers: Vec<RoutingTableExportPeer>,
}

impl RoutingTableExport {
    pub const CURRENT_VERSION: u32 = 1;
}

// ============================================================================
// 网络状态查询
// ============================================================================

/// 单个节点的网络信息（用于拓扑图展示）
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PeerInfoDto {
    pub peer_id: String,
    /// 是否在线（当前有连接）
    pub connected: bool,
    /// 是否为中继节点
    pub is_relay: bool,
    /// 是否为 bootstrap 节点
    pub is_bootstrap: bool,
    /// 是否为本节点自身
    pub is_self: bool,
}

/// 网络状态汇总（前端网络监控组件使用）
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct NetworkStatusData {
    /// 错误码（OK 表示无错误）。取值见前端展示文档
    pub error_code: String,
    /// 错误人类可读原因（error_code 非 OK 时非空）
    pub error_message: Option<String>,
    /// 是否已连接到网络（至少有一个连接）
    pub online: bool,
    /// 是否为计费网络（如移动热点）
    pub is_paid_network: bool,
    /// 计费网络检测模式："free" / "paid" / "disabled"
    pub paid_network_mode: String,
    /// 中继服务是否已启用
    pub relay_enabled: bool,
    /// 中继角色："server" / "client" / "off"（互斥）
    pub relay_role: String,
    /// NAT 状态："Public", "Private", "Unknown"
    pub nat_status: String,
    /// UPnP 状态："Enabled", "Disabled", "Unknown"
    pub upnp_status: String,
    /// IPv4 地址列表
    pub ipv4: Vec<String>,
    /// IPv6 地址列表
    pub ipv6: Vec<String>,
    /// 公网 IP（如果有）
    pub public_ip: Option<String>,
    /// 已知节点列表（含本节点）
    pub known_peers: Vec<PeerInfoDto>,
    /// 是否已连接了中继
    pub relay_connected: bool,
    /// 是否 bootstrap 已完成
    pub bootstrap_ready: bool,
    /// 已连接的中继节点 PeerID
    pub connected_relay_peer: Option<String>,
    /// 外部地址列表
    pub external_addresses: Vec<String>,
    /// 本节点 PeerID
    pub local_peer_id: String,
    /// 已连接节点数
    pub connected_peer_count: u64,
}

impl NetworkStatusData {
    /// 错误码常量
    pub const OK: &'static str = "OK";
    pub const ERR_NOT_READY: &'static str = "not_ready";
    pub const ERR_DEGRADED_NO_PEERS: &'static str = "degraded_no_peers";
    pub const ERR_CORE_NOT_INITIALIZED: &'static str = "core_not_initialized";
    pub const ERR_CORE_CHANNEL_CLOSED: &'static str = "core_channel_closed";
    pub const ERR_CORE_NO_RESPONSE: &'static str = "core_no_response";
    pub const ERR_P2P_CHANNEL_CLOSED: &'static str = "p2p_channel_closed";
    pub const ERR_P2P_NO_RESPONSE: &'static str = "p2p_no_response";

    /// 生成包含最小完整字段集的错误 JSON（保证与 schema 一致）
    pub fn error_json(code: &str, msg: &str) -> String {
        serde_json::json!({
            "error_code": code,
            "error_message": msg,
            "online": false,
            "is_paid_network": false,
            "paid_network_mode": "paid",
            "relay_enabled": false,
            "relay_role": "client",
            "nat_status": "Unknown",
            "upnp_status": "Unknown",
            "ipv4": [],
            "ipv6": [],
            "public_ip": null,
            "known_peers": [],
            "relay_connected": false,
            "bootstrap_ready": false,
            "connected_relay_peer": null,
            "external_addresses": [],
            "local_peer_id": "",
            "connected_peer_count": 0u64,
        }).to_string()
    }
}
