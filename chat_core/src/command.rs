use libp2p::PeerId;

use crate::message::ChatMessageType;

/// 控制命令：外部向核心发送的指令
#[derive(Debug)]
pub enum ChatCommand {
    /// 发送消息到网络,由核心封装签名/时间戳/hash
    SendMessage {
        peerid: PeerId,
        msgtype: ChatMessageType,
        data: Vec<u8>,
    },

    /// 添加好友（交换公钥）
    AddContact {
        peer_id: String,
        public_key: Vec<u8>,
        name: Option<String>,
    },

    /// 生成新身份
    GenerateIdentity,
    /// 选择当前身份
    SelectIdentity { peer_id: String },
    /// 删除身份
    DeleteIdentity { peer_id: String },
    /// 优雅关闭核心
    Shutdown,
}

/// 消息事件类型：用于向外部（UI）通知状态
pub enum MessageEvent {
    /// 收到新消息
    ReceiveMessage,
    /// 发生错误
    Error,
    /// 日志信息（连接状态等）
    Log,
    /// 警告信息
    Warning,
}

/// 通道消息结构：核心向外部（UI）发送的事件包装
pub struct ChatcoreEvent {
    pub event: MessageEvent,
    pub data: String,
}
