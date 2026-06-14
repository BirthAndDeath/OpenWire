use openwire_core::storage;
use openwire_core::{ChatCommand, ChatMessageType, IncomingMessage, MessageEvent};
use serde::Serialize;
use std::path::PathBuf;
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager, RunEvent};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons};
use tokio::sync::mpsc;
use tokio::sync::RwLock;

/// 发送消息到指定联系人
///
/// # 参数
/// - `mldsa_pubkey_hex`: 接收方的 ML-DSA 公钥 hex 编码（作为联系人唯一标识）
/// - `message`: 消息文本内容
#[tauri::command]
async fn send(
    state: tauri::State<'_, AppData>,
    mldsa_pubkey_hex: &str, // ML-DSA公钥的hex编码，作为联系人标识
    message: &str,
) -> Result<bool, String> {
    // 验证 mldsa_pubkey_hex 格式（应该是 hex 编码的 ML-DSA 公钥）
    if mldsa_pubkey_hex.is_empty() {
        return Err("联系人标识不能为空".to_string());
    }

    // 尝试解析为 hex，验证格式
    let _public_key_bytes =
        hex::decode(mldsa_pubkey_hex).map_err(|e| format!("无效的 pubkey 格式: {}", e))?;

    // 验证联系人是否存在
    if let Some(pool) = storage::pool() {
        let owner_identity_id = storage::get_current_identity(pool)
            .await
            .ok()
            .flatten()
            .unwrap_or_default();
        if !owner_identity_id.is_empty() {
            match storage::is_contact_exists(pool, &owner_identity_id, mldsa_pubkey_hex).await {
                Ok(false) => {
                    return Err("该联系人不存在，请先添加联系人".to_string());
                }
                Err(e) => {
                    tracing::warn!("检查联系人存在性失败: {}", e);
                }
                _ => {}
            }
        }
    }

    // 让核心全权处理消息持久化和发送，不再在此处保存消息到数据库
    // 核心的 send_text() 会在发送成功/失败时通过 MessageEvent 通知前端
    let inner = state.inner.read().await;
    let cmd_tx = inner
        .cmd_tx
        .clone()
        .ok_or_else(|| "核心尚未初始化".to_string())?;
    drop(inner);

    let result = cmd_tx
        .send(ChatCommand::SendMessage {
            mldsa_pubkey_hex: mldsa_pubkey_hex.to_string(),
            msgtype: ChatMessageType::Text,
            data: message.to_string().into_bytes(),
        })
        .await;

    match result {
        Ok(_) => Ok(true),
        Err(e) => Err(format!("发送消息失败: {}", e)),
    }
}

/// 发送文件到指定联系人
///
/// 打开文件选择对话框后调用此命令，核心会计算文件 hash、构建 FileHashInfo、
/// 注册文件路径、然后通过 SendMessage 发送 FileHash 消息给接收方。
#[tauri::command]
async fn send_file(
    state: tauri::State<'_, AppData>,
    mldsa_pubkey_hex: &str,
    file_path: &str,
) -> Result<bool, String> {
    if mldsa_pubkey_hex.is_empty() {
        return Err("联系人标识不能为空".to_string());
    }
    let _public_key_bytes =
        hex::decode(mldsa_pubkey_hex).map_err(|e| format!("无效的 pubkey 格式: {}", e))?;

    // 验证联系人是否存在
    if let Some(pool) = storage::pool() {
        let owner_identity_id = storage::get_current_identity(pool)
            .await
            .ok()
            .flatten()
            .unwrap_or_default();
        if !owner_identity_id.is_empty() {
            match storage::is_contact_exists(pool, &owner_identity_id, mldsa_pubkey_hex).await {
                Ok(false) => {
                    return Err("该联系人不存在，请先添加联系人".to_string());
                }
                Err(e) => {
                    tracing::warn!("检查联系人存在性失败: {}", e);
                }
                _ => {}
            }
        }
    }

    // 检查文件是否存在
    let path = std::path::PathBuf::from(file_path);
    if !path.exists() {
        return Err(format!("文件不存在: {}", file_path));
    }

    // 计算文件 hash（SHA256）
    let file_hash = openwire_core::transfer::compute_file_hash(&path)
        .await
        .map_err(|e| format!("计算文件 hash 失败: {}", e))?;

    // 使用文件 hash 作为 file_id
    let file_id = file_hash;

    // 获取文件元数据
    let metadata = std::fs::metadata(&path).map_err(|e| format!("获取文件信息失败: {}", e))?;
    let total_size = metadata.len();
    let filename = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown")
        .to_string();

    // 构建 FileHashInfo
    let file_info =
        openwire_core::message::FileHashInfo::new(filename, total_size, file_hash, file_id);
    let file_info_bytes = postcard::to_allocvec(&file_info)
        .map_err(|e| format!("序列化 FileHashInfo 失败: {}", e))?;

    // 注册文件路径供后续下载请求使用
    let inner = state.inner.read().await;
    let cmd_tx = inner
        .cmd_tx
        .clone()
        .ok_or_else(|| "核心尚未初始化".to_string())?;
    drop(inner);

    // 先注册文件路径
    cmd_tx
        .send(ChatCommand::RegisterFileForDownload {
            file_id,
            file_path: path,
        })
        .await
        .map_err(|e| format!("注册文件失败: {}", e))?;

    // 发送 FileHash 消息
    let result = cmd_tx
        .send(ChatCommand::SendMessage {
            mldsa_pubkey_hex: mldsa_pubkey_hex.to_string(),
            msgtype: ChatMessageType::FileHash,
            data: file_info_bytes,
        })
        .await;

    match result {
        Ok(_) => Ok(true),
        Err(e) => Err(format!("发送文件消息失败: {}", e)),
    }
}

/// 联系人 DTO，暴露给前端
#[derive(Serialize)]
struct ContactDto {
    /// 联系人的 ML-DSA 公钥 hex（唯一标识）
    mldsa_pubkey_hex: String,
    /// 联系人名称
    name: String,
    /// 添加时间（Unix 时间戳）
    added_at: i64,
}

#[tauri::command]
async fn list_contacts() -> Result<Vec<ContactDto>, String> {
    let pool = storage::pool().ok_or_else(|| "数据库尚未初始化".to_string())?;
    let owner_identity_id = match storage::get_current_identity(pool).await {
        Ok(Some(id)) => id,
        Ok(None) => {
            // 核心尚未初始化或无当前身份时返回空列表，而非报错
            return Ok(Vec::new());
        }
        Err(e) => return Err(format!("获取当前身份失败: {}", e)),
    };
    let contacts = storage::list_contacts(pool, &owner_identity_id)
        .await
        .map_err(|e| format!("加载联系人失败: {}", e))?
        .into_iter()
        .map(|contact| ContactDto {
            mldsa_pubkey_hex: contact.mldsa_pubkey_hex.clone(),
            name: contact
                .name
                .unwrap_or_else(|| contact.mldsa_pubkey_hex.clone()),
            added_at: contact.added_at,
        })
        .collect();
    Ok(contacts)
}

/// 消息 DTO，暴露给前端
#[derive(Serialize)]
struct MessageDto {
    id: i64,
    /// 联系人的 ML-DSA 公钥 hex（前端兼容字段名）
    mldsa_pubkey_hex: String,
    content: String,
    is_outgoing: bool,
    ts: i64,
    /// 消息发送状态: 0=已送达, 1=待发送(pending), 2=发送失败
    pending: i32,
}

#[tauri::command]
async fn load_messages(
    mldsa_pubkey_hex: &str,
    before: Option<i64>,
    before_id: Option<i64>,
    after: Option<i64>,
    after_id: Option<i64>,
    limit: Option<i64>,
) -> Result<Vec<MessageDto>, String> {
    let pool = storage::pool().ok_or_else(|| "数据库尚未初始化".to_string())?;
    let owner_identity_id = match storage::get_current_identity(pool).await {
        Ok(Some(id)) => id,
        Ok(None) => {
            // 核心尚未初始化或无当前身份时返回空列表
            return Ok(Vec::new());
        }
        Err(e) => return Err(format!("获取当前身份失败: {}", e)),
    };
    let msgs = storage::get_messages_range(
        pool,
        &owner_identity_id,
        mldsa_pubkey_hex,
        before,
        before_id,
        after,
        after_id,
        limit.unwrap_or(50),
    )
    .await
    .map_err(|e| format!("加载消息失败: {}", e))?
    .into_iter()
    .map(|msg| MessageDto {
        id: msg.id,
        mldsa_pubkey_hex: msg.peer_pubkey_hex,
        content: msg.content,
        is_outgoing: msg.is_outgoing != 0,
        ts: msg.ts,
        pending: msg.pending,
    })
    .collect();
    Ok(msgs)
}

/// 身份 DTO，暴露给前端
///
/// 注意：`identity_id` 是 ML-DSA 公钥的 hex 编码（持久化身份标识）。
#[derive(Serialize)]
struct IdentityDto {
    id: i64,
    /// ML-DSA 公钥 hex（身份唯一标识）
    identity_id: String,
    /// 是否为当前激活的身份
    is_current: bool,
    /// 当前会话的 ML-KEM 公钥 hex（仅当前身份有值）
    mlkem_pubkey_hex: Option<String>,
}

/// 获取当前身份的 ML-DSA 公钥原始字节（用于生成二维码）
///
/// 返回 ML-DSA 公钥的原始二进制数据（Vec<u8>），
/// 前端直接编码为 QR 码，数据量最小（ML-DSA 65 = 1952 字节）。
#[tauri::command]
async fn get_identity_qr_data(_state: tauri::State<'_, AppData>) -> Result<Vec<u8>, String> {
    let pool = storage::pool().ok_or_else(|| "数据库尚未初始化".to_string())?;
    let identity_id = storage::get_current_identity(pool)
        .await
        .map_err(|e| format!("获取当前身份失败: {}", e))?
        .ok_or_else(|| "未选择身份".to_string())?;

    // identity_id 是 ML-DSA 公钥的 hex 编码，解码为原始字节
    hex::decode(&identity_id).map_err(|e| format!("解码公钥失败: {}", e))
}

#[tauri::command]
async fn list_identities(state: tauri::State<'_, AppData>) -> Result<Vec<IdentityDto>, String> {
    let pool = storage::pool().ok_or_else(|| "数据库尚未初始化".to_string())?;
    let identities = storage::list_identities(pool)
        .await
        .map_err(|e| format!("加载身份失败: {}", e))?;

    // 从 DHT 数据库读取当前身份的 ML-KEM 公钥
    let inner = state.inner.read().await;
    let mlkem_pubkey = inner.mlkem_pubkey_hex.clone();
    drop(inner);

    let dtos = identities
        .into_iter()
        .map(|id| {
            let is_current = id.is_current == 1;
            IdentityDto {
                id: id.id,
                identity_id: id.identity_id.clone(),
                is_current,
                // 仅当前身份返回 ML-KEM 公钥
                mlkem_pubkey_hex: if is_current {
                    mlkem_pubkey.clone()
                } else {
                    None
                },
            }
        })
        .collect();
    Ok(dtos)
}

#[tauri::command]
async fn select_identity(
    _app: tauri::AppHandle,
    state: tauri::State<'_, AppData>,
    identity_id: &str,
) -> Result<(), String> {
    // 检查身份是否存在
    let pool = storage::pool().ok_or_else(|| "数据库尚未初始化".to_string())?;
    let identities = storage::list_identities(pool)
        .await
        .map_err(|e| format!("加载身份失败: {}", e))?;
    if !identities.iter().any(|id| id.identity_id == identity_id) {
        return Err("身份不存在".to_string());
    }

    // 检查是否已经是当前身份
    if identities
        .iter()
        .any(|id| id.identity_id == identity_id && id.is_current == 1)
    {
        return Ok(());
    }

    let inner = state.inner.read().await;
    let result = inner
        .cmd_tx
        .as_ref()
        .ok_or_else(|| "核心尚未初始化".to_string())?
        .send(ChatCommand::SelectIdentity {
            identity_id: identity_id.to_string(),
        })
        .await;
    match result {
        Ok(_) => {
            // 核心内部已处理 swarm 重新初始化（生成新 Ed25519 密钥对、重建 libp2p 节点）
            // 不再需要重启整个应用，避免白屏和状态丢失
            // 前端应刷新联系人列表和消息列表
            Ok(())
        }
        Err(e) => Err(format!("切换身份失败: {}", e)),
    }
}

#[tauri::command]
async fn delete_identity(
    state: tauri::State<'_, AppData>,
    identity_id: &str,
) -> Result<bool, String> {
    let pool = storage::pool().ok_or_else(|| "数据库尚未初始化".to_string())?;
    let identities = storage::list_identities(pool)
        .await
        .map_err(|e| format!("加载身份失败: {}", e))?;

    // 检查身份是否存在
    let target = identities.iter().find(|id| id.identity_id == identity_id);
    match target {
        None => return Err("身份不存在".to_string()),
        Some(id) if id.is_current == 1 => {
            return Err("不能删除当前正在使用的身份，请先切换到其他身份".to_string());
        }
        _ => {}
    }

    let inner = state.inner.read().await;
    let result = inner
        .cmd_tx
        .as_ref()
        .ok_or_else(|| "核心尚未初始化".to_string())?
        .send(ChatCommand::DeleteIdentity {
            identity_id: identity_id.to_string(),
        })
        .await;
    match result {
        Ok(_) => Ok(true),
        Err(e) => Err(format!("删除身份失败: {}", e)),
    }
}

/// 添加联系人（好友）
///
/// # 参数
/// - `mldsa_pubkey_hex`: 联系人的ML-DSA 公钥（作为唯一标识）
/// - `name`: 可选的联系人名称
/// - `mlkem_pubkey_hex`: 可选的 ML-KEM 公钥 hex（带外交互，留空则通过 DHT 自动查找）
#[tauri::command]
async fn add_contact(
    state: tauri::State<'_, AppData>,
    mldsa_pubkey_hex: &str,
    name: Option<String>,
    mlkem_pubkey_hex: Option<String>,
) -> Result<bool, String> {
    // 验证 mldsa_pubkey_hex 格式
    if mldsa_pubkey_hex.is_empty() {
        return Err("联系人标识不能为空".to_string());
    }

    // 验证 hex 格式
    let _mldsa_public_key =
        hex::decode(mldsa_pubkey_hex).map_err(|e| format!("无效的 pubkey 格式: {}", e))?;

    // 如果提供了 ML-KEM 公钥，验证并解码
    let mlkem_public_key = if let Some(hex_str) = mlkem_pubkey_hex {
        if hex_str.is_empty() {
            Vec::new()
        } else {
            hex::decode(&hex_str).map_err(|e| format!("无效的 ML-KEM 公钥格式: {}", e))?
        }
    } else {
        Vec::new()
    };

    let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
    let inner = state.inner.read().await;
    let result = inner
        .cmd_tx
        .as_ref()
        .ok_or_else(|| "核心尚未初始化".to_string())?
        .send(ChatCommand::AddContact {
            mldsa_pubkey_hex: mldsa_pubkey_hex.to_string(),
            mlkem_public_key,
            name,
            resp: resp_tx,
        })
        .await;
    match result {
        Ok(_) => match resp_rx.await {
            Ok(true) => Ok(true),
            Ok(false) => Err("添加好友失败：数据库保存失败".to_string()),
            Err(_) => Err("添加好友失败：未收到核心响应".to_string()),
        },
        Err(e) => Err(format!("添加好友失败: {}", e)),
    }
}

/// 通过 DHT 发现并添加联系人
///
/// 在 DHT 网络中查询联系人的 PeerID 和 ML-KEM 公钥，
/// 如果找到则自动添加联系人。
#[tauri::command]
async fn discover_contact(
    state: tauri::State<'_, AppData>,
    mldsa_pubkey_hex: &str,
    name: Option<String>,
) -> Result<bool, String> {
    if mldsa_pubkey_hex.is_empty() {
        return Err("联系人标识不能为空".to_string());
    }
    let inner = state.inner.read().await;
    let result = inner
        .cmd_tx
        .as_ref()
        .ok_or_else(|| "核心尚未初始化".to_string())?
        .send(ChatCommand::DiscoverContact {
            mldsa_pubkey_hex: mldsa_pubkey_hex.to_string(),
            name,
        })
        .await;
    match result {
        Ok(_) => Ok(true),
        Err(e) => Err(format!("发现联系人失败: {}", e)),
    }
}

/// 删除联系人
///
/// 从数据库中删除指定联系人。前端会先弹出确认对话框，后端再次确认后执行删除。
#[tauri::command]
async fn delete_contact(
    app_handle: tauri::AppHandle,
    _state: tauri::State<'_, AppData>,
    mldsa_pubkey_hex: &str,
) -> Result<bool, String> {
    // 验证参数
    if mldsa_pubkey_hex.is_empty() {
        return Err("联系人标识不能为空".to_string());
    }

    // 后端再次确认：验证 hex 格式
    hex::decode(mldsa_pubkey_hex).map_err(|e| format!("无效的 pubkey 格式: {}", e))?;

    // 使用 tauri_plugin_dialog 弹出确认对话框（Rust 端确认）
    let (tx, rx) = tokio::sync::oneshot::channel::<bool>();
    app_handle
        .dialog()
        .message(
            "确定要删除该联系人吗？\n删除后聊天记录将一并删除，且需要重新添加联系人才能发送消息。",
        )
        .title("删除联系人")
        .buttons(MessageDialogButtons::YesNo)
        .show(move |confirmed| {
            let _ = tx.send(confirmed);
        });

    // 等待用户确认
    let confirmed = rx.await.map_err(|_| "对话框通信失败".to_string())?;
    if !confirmed {
        tracing::info!("用户取消了删除联系人操作");
        return Err("用户取消了操作".to_string());
    }

    // 获取当前身份并执行删除
    let pool = storage::pool().ok_or_else(|| "数据库连接不可用".to_string())?;
    let owner_identity_id = storage::get_current_identity(pool)
        .await
        .ok()
        .flatten()
        .ok_or_else(|| "未选择身份，无法删除联系人".to_string())?;

    // 先删除与该联系人的所有聊天记录，再删除联系人
    match storage::delete_messages_by_peer(pool, &owner_identity_id, mldsa_pubkey_hex).await {
        Ok(deleted_msgs) => {
            tracing::info!(
                "已删除 {} 条与 {} 的聊天记录",
                deleted_msgs,
                &mldsa_pubkey_hex[..16]
            );
        }
        Err(e) => {
            tracing::error!("删除聊天记录失败: {}", e);
            // 不阻断流程，继续删除联系人
        }
    }

    match storage::delete_contact(pool, &owner_identity_id, mldsa_pubkey_hex).await {
        Ok(affected) => {
            if affected > 0 {
                tracing::info!("已删除联系人 {}", &mldsa_pubkey_hex[..16]);
                Ok(true)
            } else {
                Err("未找到该联系人".to_string())
            }
        }
        Err(e) => {
            tracing::error!("删除联系人失败: {}", e);
            Err(format!("删除联系人失败: {}", e))
        }
    }
}

/// 删除单条消息
#[tauri::command]
async fn delete_message(message_id: i64) -> Result<bool, String> {
    let pool = storage::pool().ok_or_else(|| "数据库连接不可用".to_string())?;
    match storage::delete_message(pool, message_id).await {
        Ok(affected) => {
            if affected > 0 {
                tracing::info!("已删除消息 id={}", message_id);
                Ok(true)
            } else {
                Err("未找到该消息".to_string())
            }
        }
        Err(e) => {
            tracing::error!("删除消息失败: {}", e);
            Err(format!("删除消息失败: {}", e))
        }
    }
}

/// 生成新身份（ML-DSA + ML-KEM 密钥对）
///
/// 返回新生成身份的 identity_id（ML-DSA 公钥 hex）
#[tauri::command]
async fn generate_identity(state: tauri::State<'_, AppData>) -> Result<bool, String> {
    let inner = state.inner.read().await;
    let result = inner
        .cmd_tx
        .as_ref()
        .ok_or_else(|| "核心尚未初始化".to_string())?
        .send(ChatCommand::GenerateIdentity)
        .await;
    match result {
        Ok(_) => Ok(true),
        Err(e) => Err(format!("生成身份失败: {}", e)),
    }
}

/// 请求文件下载（接收方发起）
///
/// 用户点击 FileHash 消息后调用此命令，核心会向发送方发送 FileDownloadRequest，
/// 发送方收到后开始传输文件分片。
///
/// 安全说明：download_dir 参数已被忽略，下载目录由 set_download_dir 命令统一管理。
/// 保留参数仅为了前端 API 兼容性，实际不会使用。
#[tauri::command]
async fn request_file_download(
    state: tauri::State<'_, AppData>,
    sender_mldsa_pubkey_hex: &str,
    file_id_hex: &str,
    _download_dir: Option<String>,
) -> Result<(), String> {
    // 解码 file_id（hex -> [u8; 32]）
    let file_id_bytes =
        hex::decode(file_id_hex).map_err(|e| format!("无效的 file_id hex: {}", e))?;
    if file_id_bytes.len() != 32 {
        return Err("file_id 长度必须为 32 字节".to_string());
    }
    let mut file_id = [0u8; 32];
    file_id.copy_from_slice(&file_id_bytes);

    let inner = state.inner.read().await;
    let result = inner
        .cmd_tx
        .as_ref()
        .ok_or_else(|| "核心尚未初始化".to_string())?
        .send(ChatCommand::RequestFileDownload {
            sender_mldsa_pubkey_hex: sender_mldsa_pubkey_hex.to_string(),
            file_id,
        })
        .await;
    match result {
        Ok(_) => Ok(()),
        Err(e) => Err(format!("请求文件下载失败: {}", e)),
    }
}

/// 设置下载目录
#[tauri::command]
async fn set_download_dir(state: tauri::State<'_, AppData>, path: &str) -> Result<bool, String> {
    let download_path = PathBuf::from(path);
    // 先发送命令到核心
    let inner = state.inner.read().await;
    let result = inner
        .cmd_tx
        .as_ref()
        .ok_or_else(|| "核心尚未初始化".to_string())?
        .send(ChatCommand::SetDownloadDir {
            path: download_path.clone(),
        })
        .await;
    match result {
        Ok(_) => {
            // 同步更新 AppDataInner 中的 download_dir，确保 get_download_dir 能返回正确值
            drop(inner);
            let mut inner = state.inner.write().await;
            inner.download_dir = Some(download_path);
            Ok(true)
        }
        Err(e) => Err(format!("设置下载目录失败: {}", e)),
    }
}

/// 获取当前下载目录
///
/// 优先返回用户设置的下载目录，否则 fallback 到系统下载文件夹。
#[tauri::command]
async fn get_download_dir(
    app_handle: tauri::AppHandle,
    state: tauri::State<'_, AppData>,
) -> Result<String, String> {
    let inner = state.inner.read().await;
    // 优先返回用户设置的下载目录
    if let Some(ref download_dir) = inner.download_dir {
        return Ok(download_dir.to_string_lossy().to_string());
    }
    drop(inner);

    // Fallback: 使用系统下载文件夹
    match app_handle.path().download_dir() {
        Ok(dir) => Ok(dir.to_string_lossy().to_string()),
        Err(e) => {
            tracing::warn!("获取系统下载文件夹失败: {}, 回退到 data_dir/downloads", e);
            let inner = state.inner.read().await;
            let data_dir = inner.data_dir.clone();
            drop(inner);
            let default_download_dir = data_dir.join("downloads");
            Ok(default_download_dir.to_string_lossy().to_string())
        }
    }
}

/// 检查 Keyring 是否可用（隔离层检查）。
///
/// 如果 Keyring 可用，前端应隐藏密码相关 UI。
/// 通过 `rootcell::identity::PrivateKeyHandle::load_master_key()` 判断：
/// - `Ok(Some(_))` → Keyring 可用且已有主密钥 → 返回 `true`
/// - `Ok(None)` → Keyring 可能不可用或没有主密钥 → 保守返回 `false`
/// - `Err(_)` → Keyring 不可用 → 返回 `false`
/// 前端轮询检查 Core 是否已初始化完成。
///
/// 由于 Tauri 的 emit 是 fire-and-forget，如果前端尚未注册 listener，
/// core-ready 事件会丢失。此命令让前端通过轮询可靠地检测 Core 就绪状态。
#[tauri::command]
async fn check_core_ready(state: tauri::State<'_, AppData>) -> Result<bool, String> {
    let inner = state.inner.read().await;
    Ok(inner.core_ready)
}

/// 获取节点配置（bootstrap 和 relay 节点列表）
///
/// 从 data_dir/nodes.json 读取节点配置并返回给前端。
/// 返回 JSON 字符串格式：{"relay_nodes": [["peer_id", "addr"], ...], "bootstrap_nodes": [...]}
#[tauri::command]
async fn get_nodes_config(state: tauri::State<'_, AppData>) -> Result<String, String> {
    let inner = state.inner.read().await;
    let data_dir = inner.data_dir.clone();
    drop(inner);

    let nodes_config = openwire_core::p2p::nodes::NodesConfig::load(&data_dir);
    // 手动序列化为 JSON 字符串
    let json = nodes_config.to_json_string();
    Ok(json)
}

/// 保存节点配置（bootstrap 和 relay 节点列表）
///
/// 前端修改后调用此命令保存到 data_dir/nodes.json。
/// 注意：修改后需要重启应用才能生效。
/// 参数 relay_nodes 和 bootstrap_nodes 都是 [[peer_id, multiaddr], ...] 格式。
#[tauri::command]
async fn save_nodes_config(
    state: tauri::State<'_, AppData>,
    relay_nodes: Vec<Vec<String>>,
    bootstrap_nodes: Vec<Vec<String>>,
) -> Result<(), String> {
    let inner = state.inner.read().await;
    let data_dir = inner.data_dir.clone();
    drop(inner);

    // 转换 Vec<Vec<String>> 为 Vec<[String; 2]>
    let relay: Vec<[String; 2]> = relay_nodes
        .into_iter()
        .map(|v| {
            if v.len() != 2 {
                Err("每个 relay 节点必须包含 peer_id 和 multiaddr".to_string())
            } else {
                Ok([v[0].clone(), v[1].clone()])
            }
        })
        .collect::<Result<Vec<_>, _>>()?;

    let bootstrap: Vec<[String; 2]> = bootstrap_nodes
        .into_iter()
        .map(|v| {
            if v.len() != 2 {
                Err("每个 bootstrap 节点必须包含 peer_id 和 multiaddr".to_string())
            } else {
                Ok([v[0].clone(), v[1].clone()])
            }
        })
        .collect::<Result<Vec<_>, _>>()?;

    let config = openwire_core::p2p::nodes::NodesConfig {
        relay_nodes: relay,
        bootstrap_nodes: bootstrap,
    };

    config.save(&data_dir).map_err(|e| format!("保存节点配置失败: {}", e))?;
    tracing::info!("节点配置已更新，重启后生效");
    Ok(())
}

/// 重置节点配置为默认值
///
/// 将 data_dir/nodes.json 重置为默认的 bootstrap 和 relay 节点列表。
/// 返回重置后的节点配置 JSON 字符串，前端可直接更新 UI。
#[tauri::command]
async fn reset_nodes_config(state: tauri::State<'_, AppData>) -> Result<String, String> {
    let inner = state.inner.read().await;
    let data_dir = inner.data_dir.clone();
    drop(inner);

    let config = openwire_core::p2p::nodes::NodesConfig::reset_to_default(&data_dir)
        .map_err(|e| format!("重置节点配置失败: {}", e))?;
    let json = config.to_json_string();
    tracing::info!("节点配置已重置为默认值，重启后生效");
    Ok(json)
}

#[tauri::command]
async fn is_keyring_available() -> Result<bool, String> {
    let available = rootcell::identity::PrivateKeyHandle::check_keyring_available();
    if available {
        tracing::debug!("Keyring is available");
    } else {
        tracing::debug!("Keyring is not available");
    }
    Ok(available)
}

/// 设置用户密码（Keyring 降级模式）。
///
/// 在 Keyring 不可用时，使用此密码派生密钥加密私钥文件。
/// 密码由 rootcell 内部使用 Argon2id 派生为 256 位 hex 密钥。
#[tauri::command]
async fn set_password(state: tauri::State<'_, AppData>, password: &str) -> Result<bool, String> {
    // 使用统一的 Argon2id KDF 派生密钥（rootcell 内部完成 hex 编码）
    let key_hex = rootcell::identity::PrivateKeyHandle::derive_key_from_password(password);
    let mut inner = state.inner.write().await;
    inner.passwd = Some(key_hex);
    tracing::info!("用户密码已设置（Keyring 降级模式，Argon2id KDF）");
    Ok(true)
}

/// 重试 Core 初始化（在用户通过前端设置密码后调用）。
///
/// 当 Keyring 不可用且无密码时，前端会收到 `need-password` 事件。
/// 用户输入密码后，前端调用 `set_password` 设置密码，然后调用此命令重试初始化。
#[tauri::command]
async fn retry_init(app_handle: tauri::AppHandle) -> Result<bool, String> {
    let (data_dir, passwd) = {
        let app_data = app_handle.state::<AppData>();
        let inner = app_data.inner.read().await;
        let data_dir = inner.data_dir.clone();
        let passwd = inner.passwd.clone();
        drop(inner);
        (data_dir, passwd)
    };

    let mut cfg = openwire_core::CoreConfig {
        data_dir,
        path_to_log: None,
        log_level: Some("info".to_string()),
        download_dir: None,
        passwd,
        relay_nodes: Vec::new(),
        bootstrap_nodes: Vec::new(),
    };
    cfg.load_nodes_config();

    match openwire_core::ChatCore::try_init(cfg.clone()).await {
        Ok(mut chat_core_instance) => {
            // 更新 AppData
            {
                let app_data = app_handle.state::<AppData>();
                let mut inner = app_data.inner.write().await;
                inner.cmd_tx = Some(chat_core_instance.core_handle.cmd_tx.clone());
                inner.data_dir = cfg.data_dir.clone();
                inner.mlkem_pubkey_hex = chat_core_instance.mlkem_pubkey_hex.clone();
                inner.need_password = false;
            }

            // 发送核心就绪事件，通知前端可以安全加载数据
            app_handle.emit("core-ready", true).ok();

            let mut rx = match chat_core_instance.take_rx_message() {
                Some(rx) => rx,
                None => {
                    tracing::error!("重试初始化失败：无法获取消息接收器");
                    return Err("内部错误：无法获取消息接收器".to_string());
                }
            };

            let app_handle_for_events = app_handle.clone();
            chat_core_instance.run();

            // 启动事件循环
            tauri::async_runtime::spawn(async move {
                while let Some(msg) = rx.recv().await {
                    match msg {
                        MessageEvent::Log(data) => {
                            app_handle_for_events.emit("log", data).ok();
                        }
                        MessageEvent::ReceiveMessage(msg) => {
                            // DeliveryReceipt 是送达回执，不显示在消息历史中，
                            // 而是作为独立事件发送，让前端将对应 pending 消息标记为已送达
                            if let IncomingMessage::DeliveryReceipt {
                                ref message_hash, ..
                            } = msg
                            {
                                app_handle_for_events
                                    .emit("delivery-receipt", message_hash)
                                    .ok();
                            } else {
                                // 将其他 IncomingMessage 枚举序列化为 JSON 字符串发送给前端
                                let json = serde_json::to_string(&msg).unwrap_or_default();
                                app_handle_for_events.emit("chat-message", json).ok();
                            }
                        }
                        MessageEvent::OnlineStatus { online_contacts } => {
                            // 在线状态更新：发送在线联系人 ML-DSA 公钥 hex 列表
                            // 前端据此更新每个联系人的 online 状态指示器
                            app_handle_for_events
                                .emit("online-status", online_contacts)
                                .ok();
                        }
                        MessageEvent::Warning(data) => {
                            app_handle_for_events.emit("warning", data).ok();
                        }
                        MessageEvent::Error(data) => {
                            app_handle_for_events.emit("error", data).ok();
                        }
                        MessageEvent::FileTransferProgress(progress) => {
                            app_handle_for_events
                                .emit("file-transfer-progress", progress)
                                .ok();
                        }
                        MessageEvent::ContactOnlineStatus {
                            mldsa_pubkey_hex,
                            online,
                        } => {
                            let payload = serde_json::json!({
                                "mldsa_pubkey_hex": mldsa_pubkey_hex,
                                "online": online,
                            });
                            app_handle_for_events
                                .emit("contact-online-status", payload.to_string())
                                .ok();
                        }
                    }
                }
            });

            tracing::info!("✅ Core 重试初始化成功");
            Ok(true)
        }
        Err(e) => {
            let err_msg = format!("Core 初始化失败: {}", e);
            tracing::error!("{}", err_msg);
            Err(err_msg)
        }
    }
}

pub struct AppData {
    pub inner: Arc<RwLock<AppDataInner>>,
}

pub struct AppDataInner {
    pub cmd_tx: Option<mpsc::Sender<ChatCommand>>,
    pub data_dir: PathBuf,
    /// 当前会话的 ML-KEM 公钥 hex（用于前端显示）
    pub mlkem_pubkey_hex: Option<String>,
    /// 用户密码派生密钥 hex（由 rootcell::derive_key_from_password 派生）
    pub passwd: Option<String>,
    /// 是否需要密码（Keyring 不可用标志），前端据此弹出密码输入框
    pub need_password: bool,
    /// 当前下载目录（由 set_download_dir 设置，get_download_dir 读取）
    pub download_dir: Option<PathBuf>,
    /// Core 是否已初始化完成（前端通过 check_core_ready 命令轮询）
    pub core_ready: bool,
}

/// 用于在核心初始化完成前占位的初始状态
fn create_placeholder_appdata() -> AppData {
    AppData {
        inner: Arc::new(RwLock::new(AppDataInner {
            cmd_tx: None,
            data_dir: PathBuf::new(),
            mlkem_pubkey_hex: None,
            passwd: None,
            need_password: false,
            download_dir: None,
            core_ready: false,
        })),
    }
}
#[cfg_attr(mobile, tauri::mobile_entry_point)]
/// 初始化成功后设置 AppData、启动事件循环。
///
/// 此函数被 `run()` 中的初始化闭包调用，避免代码重复。
async fn setup_core_and_event_loop(
    mut chat_core_instance: openwire_core::ChatCore,
    cfg: openwire_core::CoreConfig,
    apphandle: tauri::AppHandle,
) {
    // 用真实的 AppData 替换占位状态
    let app_data = apphandle.state::<AppData>();
    let mut inner = app_data.inner.write().await;
    inner.cmd_tx = Some(chat_core_instance.core_handle.cmd_tx.clone());
    inner.data_dir = cfg.data_dir.clone();
    inner.mlkem_pubkey_hex = chat_core_instance.mlkem_pubkey_hex.clone();
    inner.need_password = false;
    inner.core_ready = true;
    // passwd 保持之前设置的值，不需要覆盖
    drop(inner);

    // 发送核心就绪事件，通知前端可以安全加载数据
    apphandle.emit("core-ready", true).ok();
    tracing::info!("✅ Core 初始化完成，已发送 core-ready 事件");

    let mut rx = match chat_core_instance.take_rx_message() {
        Some(rx) => rx,
        None => {
            tracing::error!("Failed to take message receiver");
            return;
        }
    };
    // 启动核心服务（在独立线程中运行）
    let app_handle_for_events = apphandle.clone();
    chat_core_instance.run();

    // 主事件循环 — 直接等待消息，无需心跳
    while let Some(msg) = rx.recv().await {
        match msg {
            MessageEvent::Log(data) => {
                app_handle_for_events.emit("log", data).ok();
            }
            MessageEvent::ReceiveMessage(msg) => {
                // DeliveryReceipt 是送达回执，不显示在消息历史中，
                // 而是作为独立事件发送，让前端将对应 pending 消息标记为已送达
                if let IncomingMessage::DeliveryReceipt {
                    ref message_hash, ..
                } = msg
                {
                    app_handle_for_events
                        .emit("delivery-receipt", message_hash)
                        .ok();
                } else if let IncomingMessage::MessageSent {
                    ref message_hash,
                    ref peer_id,
                } = msg
                {
                    // MessageSent 是消息已发送通知，包含消息哈希，
                    // 前端用此哈希更新对应消息的 message_hash 字段，
                    // 以便后续送达回执能精确匹配
                    let payload = serde_json::json!({
                        "message_hash": message_hash,
                        "peer_id": peer_id,
                    });
                    app_handle_for_events
                        .emit("message-sent", payload.to_string())
                        .ok();
                } else {
                    // 将其他 IncomingMessage 枚举序列化为 JSON 字符串发送给前端
                    let json = serde_json::to_string(&msg).unwrap_or_default();
                    app_handle_for_events.emit("chat-message", json).ok();
                }
            }
            MessageEvent::OnlineStatus { online_contacts } => {
                // 在线状态更新：发送在线联系人 ML-DSA 公钥 hex 列表
                // 前端据此更新每个联系人的 online 状态指示器
                app_handle_for_events
                    .emit("online-status", online_contacts)
                    .ok();
            }
            MessageEvent::Warning(data) => {
                app_handle_for_events.emit("warning", data).ok();
            }
            MessageEvent::Error(data) => {
                app_handle_for_events.emit("error", data).ok();
            }
            MessageEvent::FileTransferProgress(progress) => {
                app_handle_for_events
                    .emit("file-transfer-progress", progress)
                    .ok();
            }
            MessageEvent::ContactOnlineStatus {
                mldsa_pubkey_hex,
                online,
            } => {
                let payload = serde_json::json!({
                    "mldsa_pubkey_hex": mldsa_pubkey_hex,
                    "online": online,
                });
                app_handle_for_events
                    .emit("contact-online-status", payload.to_string())
                    .ok();
            }
        }
    }
}

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_store::Builder::new().build())
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let apphandle = app.handle().clone();

            // 先 manage 一个占位 AppData，确保 state() 不会 panic
            apphandle.manage(create_placeholder_appdata());

            // 使用 Tauri 的 async runtime 启动核心服务
            tauri::async_runtime::spawn(async move {
                let data_dir = match apphandle.path().app_data_dir() {
                    Ok(dir) => dir,
                    Err(e) => {
                        tracing::error!("Failed to get app data directory: {}", e);
                        return;
                    }
                };
                let log_path = match apphandle.path().app_log_dir() {
                    Ok(dir) => dir,
                    Err(e) => {
                        tracing::error!("Failed to get app log directory: {}", e);
                        return;
                    }
                };

                // 确保目录存在
                std::fs::create_dir_all(&data_dir).ok();
                std::fs::create_dir_all(&log_path).ok();
                #[cfg(debug_assertions)]
                let log_level = "debug";
                #[cfg(not(debug_assertions))]
                let log_level = "info";

                // 从 AppData 获取密码（由前端通过 set_password 命令设置）
                let app_data_state = apphandle.state::<AppData>();
                let passwd = app_data_state.inner.read().await.passwd.clone();
                drop(app_data_state);

                let mut cfg =
                    openwire_core::CoreConfig::new(data_dir, Some(log_path), Some(log_level));
                cfg.passwd = passwd;
                cfg.load_nodes_config();

                match openwire_core::ChatCore::try_init(cfg.clone()).await {
                    Ok(chat_core_instance) => {
                        setup_core_and_event_loop(chat_core_instance, cfg, apphandle).await;
                    }
                    Err(e) => {
                        let err_msg = format!("Core 初始化失败: {}", e);
                        tracing::error!("{}", err_msg);

                        // 判断是否是 Keyring 不可用导致的失败（无密码）
                        // 覆盖所有 Keyring 相关错误场景：
                        // - "Keyring unavailable" — Keyring 服务不可用
                        // - "Private key not found" — 私钥文件存在但 Keyring 无法解密
                        // - "Failed to create Keyring entry" — 生成新身份时 Keyring 写入失败
                        // - "Failed to get password from Keyring" — Keyring 读取失败
                        let needs_password = err_msg.contains("Keyring unavailable")
                            || err_msg.contains("Private key not found")
                            || err_msg.contains("Failed to create Keyring entry")
                            || err_msg.contains("Failed to get password from Keyring");

                        if needs_password {
                            // 更新 AppData 标记需要密码
                            {
                                let app_data = apphandle.state::<AppData>();
                                let mut inner = app_data.inner.write().await;
                                inner.need_password = true;
                                inner.data_dir = cfg.data_dir.clone();
                                // app_data 在此处 drop，释放对 apphandle 的借用
                            }

                            // 发送 need-password 事件给前端
                            apphandle.emit("need-password", true).ok();
                            tracing::info!("已发送 need-password 事件，等待前端输入密码...");

                            // 轮询等待密码被设置（最多等待 5 分钟）
                            let poll_interval = std::time::Duration::from_millis(500);
                            let max_attempts = 600;
                            for _ in 0..max_attempts {
                                tokio::time::sleep(poll_interval).await;
                                let (passwd, still_needed) = {
                                    let app_data = apphandle.state::<AppData>();
                                    let inner = app_data.inner.read().await;
                                    let passwd = inner.passwd.clone();
                                    let still_needed = inner.need_password;
                                    drop(inner);
                                    drop(app_data);
                                    (passwd, still_needed)
                                };

                                if !still_needed && passwd.is_some() {
                                    tracing::info!("密码已设置，重试 Core 初始化...");
                                    cfg.passwd = passwd;
                                    match openwire_core::ChatCore::try_init(cfg.clone()).await {
                                        Ok(chat_core_instance) => {
                                            setup_core_and_event_loop(
                                                chat_core_instance,
                                                cfg,
                                                apphandle,
                                            )
                                            .await;
                                            return;
                                        }
                                        Err(e2) => {
                                            tracing::error!("重试 Core 初始化仍然失败: {}", e2);
                                            apphandle
                                                .emit("warning", format!("密码验证失败: {}", e2))
                                                .ok();
                                            // 重置 need_password 让前端可以再次输入
                                            {
                                                let app_data = apphandle.state::<AppData>();
                                                let mut inner = app_data.inner.write().await;
                                                inner.need_password = true;
                                            }
                                            apphandle.emit("need-password", true).ok();
                                        }
                                    }
                                }
                            }

                            tracing::error!("等待密码超时，Core 初始化失败");
                        }
                    }
                }
            });

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            send,
            send_file,
            list_contacts,
            list_identities,
            select_identity,
            delete_identity,
            generate_identity,
            add_contact,
            discover_contact,
            request_file_download,
            set_download_dir,
            get_download_dir,
            load_messages,
            get_identity_qr_data,
            set_password,
            retry_init,
            is_keyring_available,
            check_core_ready,
            delete_contact,
            delete_message,
            get_nodes_config,
            save_nodes_config,
            reset_nodes_config
        ])

        .build(tauri::generate_context!())
        .expect("error while running tauri application")
        .run(|apphandle, event| match event {
            RunEvent::Exit => cleanup(apphandle),
            RunEvent::Ready => {}
            _ => {}
        })
}
fn cleanup(app: &AppHandle) {
    // 安全获取 AppData，如果核心尚未初始化则跳过清理
    let app_data = match app.try_state::<AppData>() {
        Some(data) => data,
        None => {
            tracing::warn!("AppData not initialized yet, skipping cleanup");
            return;
        }
    };

    let inner = app_data.inner.blocking_read();

    // 如果 cmd_tx 尚未设置（核心未初始化），跳过清理
    let cmd_tx = match &inner.cmd_tx {
        Some(tx) => tx,
        None => {
            tracing::warn!("Core not initialized yet, skipping cleanup");
            return;
        }
    };

    // 使用 try_send 发送关闭命令，避免在退出时可能死锁
    if let Err(e) = cmd_tx.try_send(openwire_core::ChatCommand::Shutdown) {
        tracing::error!("Error sending shutdown command: {}", e);
        app.emit("warning", format!("Error sending shutdown command: {e}"))
            .ok();
    }
}