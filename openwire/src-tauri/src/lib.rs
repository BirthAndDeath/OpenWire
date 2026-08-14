use openwire_core::storage;
use openwire_core::{ChatCommand, ChatMessageType, IncomingMessage, MessageEvent};
use serde::Serialize;
use std::path::PathBuf;
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager, RunEvent};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons};
use tokio::sync::mpsc;
use tokio::sync::RwLock;

#[derive(Debug, Clone, Serialize)]
struct SentFileDto {
    file_hash: String,
    filename: String,
    total_size: i64,
    sent_at: i64,
}

/// 将 `[N] hex` 格式的消息内容转换为用于前端显示的 JSON 字符串
fn decode_message_content(content: &str) -> String {
    // 尝试解析 `[N] hex` 格式（FileHash, FileStream, FileDownloadRequest 等）
    if let Some(stripped) = content.strip_prefix('[') {
        if let Some(rest) = stripped.split(']').next() {
            if let Ok(msgtype) = rest.parse::<u8>() {
                let hex_part = content.find(']').map_or(content, |i| &content[i + 1..]).trim();
                if msgtype == ChatMessageType::FileHash as u8 {
                    // 尝试解析 FileHashInfo
                    if let Ok(bytes) = hex::decode(hex_part) {
                        if let Ok(info) =
                            postcard::from_bytes::<openwire_core::message::FileHashInfo>(&bytes)
                        {
                            if let Ok(json) = serde_json::to_string(&serde_json::json!({
                                "file_hash": hex::encode(info.file_hash),
                                "file_id": hex::encode(info.file_id),
                                "filename": info.filename,
                                "total_size": info.total_size,
                            })) {
                                return json;
                            }
                        }
                    }
                }
            }
        }
    }
    content.to_string()
}

/// 验证联系人存在性：检查 hex 格式 + 数据库中的联系人记录
async fn validate_contact(mldsa_pubkey_hex: &str) -> Result<(), String> {
    if mldsa_pubkey_hex.is_empty() {
        return Err("联系人标识不能为空".to_string());
    }
    let _ = hex::decode(mldsa_pubkey_hex).map_err(|e| format!("无效的 pubkey 格式: {}", e))?;
    let pool = storage::pool().ok_or_else(|| "数据库不可用，无法验证联系人".to_string())?;
    let owner_identity_id = storage::get_current_identity(pool)
        .await
        .ok()
        .flatten()
        .unwrap_or_default();
    if !owner_identity_id.is_empty() {
        match storage::is_contact_exists(pool, &owner_identity_id, mldsa_pubkey_hex).await {
            Ok(false) => return Err("该联系人不存在，请先添加联系人".to_string()),
            Err(e) => return Err(format!("检查联系人存在性失败: {}", e)),
            _ => {}
        }
    }
    Ok(())
}

#[tauri::command]
async fn send(
    state: tauri::State<'_, AppData>,
    mldsa_pubkey_hex: &str,
    message: &str,
) -> Result<bool, String> {
    validate_contact(mldsa_pubkey_hex).await?;

    let inner = state.inner.read().await;
    let cmd_tx = inner
        .cmd_tx
        .clone()
        .ok_or_else(|| "核心尚未初始化".to_string())?;
    drop(inner);

    let data = message.as_bytes();
    if data.len() > 65536 {
        return Err("消息内容过长（最大 65536 字节）".to_string());
    }

    let result = cmd_tx
        .send(ChatCommand::SendMessage {
            mldsa_pubkey_hex: mldsa_pubkey_hex.to_string(),
            msgtype: ChatMessageType::Text,
            data: data.to_vec(),
        })
        .await;

    match result {
        Ok(_) => Ok(true),
        Err(e) => Err(format!("发送消息失败: {}", e)),
    }
}

#[tauri::command]
async fn send_file(
    state: tauri::State<'_, AppData>,
    mldsa_pubkey_hex: &str,
    file_path: &str,
) -> Result<bool, String> {
    validate_contact(mldsa_pubkey_hex).await?;

    let path = std::path::PathBuf::from(file_path);
    let canon = path.canonicalize().map_err(|e| format!("无效的文件路径: {}", e))?;
    if !canon.exists() {
        return Err(format!("文件不存在: {}", file_path));
    }
    // 限制只允许发送下载目录内的文件，防止路径遍历
    let data_dir = {
        let inner = state.inner.read().await;
        inner.data_dir.clone()
    };
    let downloads_dir = data_dir.join("downloads");
    let downloads_canon = downloads_dir.canonicalize().unwrap_or(downloads_dir);
    if !canon.starts_with(&downloads_canon) {
        return Err("只能发送下载目录内的文件".to_string());
    }
    drop(data_dir);

    let file_hash = openwire_core::transfer::compute_file_hash(&path)
        .await
        .map_err(|e| format!("计算文件 hash 失败: {}", e))?;

    let file_id = file_hash;

    let metadata = std::fs::metadata(&path).map_err(|e| format!("获取文件信息失败: {}", e))?;
    let total_size = metadata.len();
    let filename = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown")
        .to_string();

    let file_info =
        openwire_core::message::FileHashInfo::new(filename.clone(), total_size, file_hash, file_id);
    let file_info_bytes = postcard::to_allocvec(&file_info)
        .map_err(|e| format!("序列化 FileHashInfo 失败: {}", e))?;

    // 记录到已发送文件历史（sent_files 表），使对方下载请求时能验证文件合法性
    if let Some(pool) = openwire_core::storage::pool() {
        if let Err(e) = openwire_core::storage::add_sent_file(
            pool,
            &file_hash,
            path.to_str().unwrap_or(""),
            &filename,
            total_size,
        )
        .await
        {
            tracing::warn!("记录已发送文件失败: {e}");
        }
    }

    let inner = state.inner.read().await;
    let cmd_tx = inner
        .cmd_tx
        .clone()
        .ok_or_else(|| "核心尚未初始化".to_string())?;
    drop(inner);

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

#[derive(Serialize)]
struct ContactDto {
    mldsa_pubkey_hex: String,
    name: String,
    added_at: i64,
}

#[tauri::command]
async fn list_contacts() -> Result<Vec<ContactDto>, String> {
    let pool = storage::pool().ok_or_else(|| "数据库尚未初始化".to_string())?;
    let owner_identity_id = match storage::get_current_identity(pool).await {
        Ok(Some(id)) => id,
        Ok(None) => {
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

#[derive(Serialize)]
struct MessageDto {
    id: i64,
    mldsa_pubkey_hex: String,
    content: String,
    is_outgoing: bool,
    ts: i64,
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
        limit.map(|l| l.clamp(0, openwire_core::storage::MAX_MESSAGE_PAGE_SIZE)).unwrap_or(50),
    )
    .await
    .map_err(|e| format!("加载消息失败: {}", e))?
    .into_iter()
    .map(|msg| MessageDto {
        id: msg.id,
        mldsa_pubkey_hex: msg.peer_pubkey_hex,
        content: decode_message_content(&msg.content),
        is_outgoing: msg.is_outgoing != 0,
        ts: msg.ts,
        pending: msg.pending,
    })
    .collect();
    Ok(msgs)
}

#[derive(Serialize)]
struct IdentityDto {
    id: i64,
    identity_id: String,
    is_current: bool,
    mlkem_pubkey_hex: Option<String>,
}

#[tauri::command]
async fn get_identity_qr_data(_state: tauri::State<'_, AppData>) -> Result<Vec<u8>, String> {
    let pool = storage::pool().ok_or_else(|| "数据库尚未初始化".to_string())?;
    let identity_id = storage::get_current_identity(pool)
        .await
        .map_err(|e| format!("获取当前身份失败: {}", e))?
        .ok_or_else(|| "未选择身份".to_string())?;

    hex::decode(&identity_id).map_err(|e| format!("解码公钥失败: {}", e))
}

#[tauri::command]
async fn list_identities(state: tauri::State<'_, AppData>) -> Result<Vec<IdentityDto>, String> {
    let pool = storage::pool().ok_or_else(|| "数据库尚未初始化".to_string())?;
    let identities = storage::list_identities(pool)
        .await
        .map_err(|e| format!("加载身份失败: {}", e))?;

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
    let pool = storage::pool().ok_or_else(|| "数据库尚未初始化".to_string())?;
    let identities = storage::list_identities(pool)
        .await
        .map_err(|e| format!("加载身份失败: {}", e))?;
    if !identities.iter().any(|id| id.identity_id == identity_id) {
        return Err("身份不存在".to_string());
    }

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
        Ok(_) => Ok(()),
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

#[tauri::command]
async fn add_contact(
    state: tauri::State<'_, AppData>,
    mldsa_pubkey_hex: &str,
    name: Option<String>,
    mlkem_pubkey_hex: Option<String>,
) -> Result<bool, String> {
    if mldsa_pubkey_hex.is_empty() {
        return Err("联系人标识不能为空".to_string());
    }

    let _mldsa_public_key =
        hex::decode(mldsa_pubkey_hex).map_err(|e| format!("无效的 pubkey 格式: {}", e))?;

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

#[tauri::command]
async fn delete_contact(
    app_handle: tauri::AppHandle,
    _state: tauri::State<'_, AppData>,
    mldsa_pubkey_hex: &str,
) -> Result<bool, String> {
    if mldsa_pubkey_hex.is_empty() {
        return Err("联系人标识不能为空".to_string());
    }

    hex::decode(mldsa_pubkey_hex).map_err(|e| format!("无效的 pubkey 格式: {}", e))?;

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

    let confirmed = tokio::time::timeout(std::time::Duration::from_secs(30), rx)
        .await
        .map_err(|_| "对话框超时，删除操作已取消".to_string())?
        .map_err(|_| "对话框通信失败".to_string())?;
    if !confirmed {
        tracing::info!("用户取消了删除联系人操作");
        return Err("用户取消了操作".to_string());
    }

    let pool = storage::pool().ok_or_else(|| "数据库连接不可用".to_string())?;
    let owner_identity_id = storage::get_current_identity(pool)
        .await
        .ok()
        .flatten()
        .ok_or_else(|| "未选择身份，无法删除联系人".to_string())?;

    let deleted_msgs = storage::delete_messages_by_peer(pool, &owner_identity_id, mldsa_pubkey_hex)
        .await
        .map_err(|e| format!("删除聊天记录失败: {}", e))?;
    tracing::info!(
        "已删除 {} 条与 {} 的聊天记录",
        deleted_msgs,
        &mldsa_pubkey_hex[..16.min(mldsa_pubkey_hex.len())]
    );

    match storage::delete_contact(pool, &owner_identity_id, mldsa_pubkey_hex).await {
        Ok(affected) => {
            if affected > 0 {
                tracing::info!("已删除联系人 {}", &mldsa_pubkey_hex[..16.min(mldsa_pubkey_hex.len())]);
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

#[tauri::command]
async fn delete_message(message_id: i64) -> Result<bool, String> {
    let pool = storage::pool().ok_or_else(|| "数据库连接不可用".to_string())?;
    // 验证消息属于当前用户
    let owner_identity_id = storage::get_current_identity(pool)
        .await
        .ok()
        .flatten()
        .ok_or_else(|| "未选择身份，无法删除消息".to_string())?;
    let msg = storage::get_message(pool, message_id)
        .await
        .map_err(|e| format!("查找消息失败: {}", e))?
        .ok_or_else(|| "未找到该消息".to_string())?;
    if msg.owner_identity_id != owner_identity_id {
        return Err("无权删除此消息".to_string());
    }
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

#[tauri::command]
async fn request_file_download(
    state: tauri::State<'_, AppData>,
    sender_mldsa_pubkey_hex: &str,
    file_hash_hex: &str,
    save_path: String,
) -> Result<(), String> {
    let file_hash_bytes =
        hex::decode(file_hash_hex).map_err(|e| format!("无效的 file_hash hex: {}", e))?;
    if file_hash_bytes.len() != 32 {
        return Err("file_hash 长度必须为 32 字节".to_string());
    }
    let mut file_hash = [0u8; 32];
    file_hash.copy_from_slice(&file_hash_bytes);

    if save_path.trim().is_empty() {
        return Err("保存路径不能为空".to_string());
    }

    // 限制保存路径必须在下载目录内，防止路径遍历
    let data_dir = {
        let inner = state.inner.read().await;
        inner.data_dir.clone()
    };
    let downloads_dir = data_dir.join("downloads");
    let save_canon = {
        let parent = std::path::Path::new(&save_path).parent().unwrap_or(std::path::Path::new(""));
        std::fs::create_dir_all(downloads_dir.clone())
            .map_err(|e| format!("创建下载目录失败: {}", e))?;
        parent.canonicalize().map_err(|e| format!("无效的保存路径: {}", e))?
    };
    let downloads_canon = downloads_dir
        .canonicalize()
        .map_err(|e| format!("下载目录不可用: {}", e))?;
    if !save_canon.starts_with(&downloads_canon) {
        return Err("保存路径必须在下载目录内".to_string());
    }
    drop(data_dir);

    let inner = state.inner.read().await;
    let result = inner
        .cmd_tx
        .as_ref()
        .ok_or_else(|| "核心尚未初始化".to_string())?
        .send(ChatCommand::RequestFileDownload {
            sender_mldsa_pubkey_hex: sender_mldsa_pubkey_hex.to_string(),
            file_hash,
            save_path: PathBuf::from(save_path),
        })
        .await;
    match result {
        Ok(_) => Ok(()),
        Err(e) => Err(format!("请求文件下载失败: {}", e)),
    }
}

#[tauri::command]
async fn check_core_ready(state: tauri::State<'_, AppData>) -> Result<(), String> {
    let inner = state.inner.read().await;
    if inner.core_ready {
        Ok(())
    } else {
        Err("核心尚未就绪".to_string())
    }
}

#[tauri::command]
async fn list_sent_files() -> Result<Vec<SentFileDto>, String> {
    let pool = storage::pool().ok_or_else(|| "数据库未初始化".to_string())?;
    let files = storage::list_all_sent_files(pool)
        .await
        .map_err(|e| format!("查询已发送文件失败: {e}"))?;
    Ok(files
        .into_iter()
        .map(|f| SentFileDto {
            file_hash: hex::encode(f.file_hash),
            filename: f.filename,
            total_size: f.total_size,
            sent_at: f.sent_at,
        })
        .collect())
}

#[tauri::command]
async fn delete_sent_file(file_hash_hex: &str) -> Result<(), String> {
    let pool = storage::pool().ok_or_else(|| "数据库未初始化".to_string())?;
    let hash = hex::decode(file_hash_hex).map_err(|e| format!("无效的哈希格式: {e}"))?;
    storage::delete_sent_file(pool, &hash)
        .await
        .map_err(|e| format!("撤销发送权限失败: {e}"))?;
    Ok(())
}

#[tauri::command]
async fn get_nodes_config(state: tauri::State<'_, AppData>) -> Result<String, String> {
    let inner = state.inner.read().await;
    let data_dir = inner.data_dir.clone();
    drop(inner);

    let nodes_config = openwire_core::p2p::nodes::NodesConfig::load(&data_dir);
    let json = nodes_config.to_json_string();
    Ok(json)
}

#[tauri::command]
async fn save_nodes_config(
    state: tauri::State<'_, AppData>,
    relay_nodes: Vec<Vec<String>>,
    bootstrap_nodes: Vec<Vec<String>>,
) -> Result<(), String> {
    let inner = state.inner.read().await;
    let data_dir = inner.data_dir.clone();
    drop(inner);

    let relay: Vec<[String; 2]> = relay_nodes
        .into_iter()
        .map(|v| {
            if v.len() != 2 {
                return Err("每个 relay 节点必须包含 peer_id 和 multiaddr".to_string());
            }
            // 验证 multiaddr 格式
            v[1].parse::<libp2p::Multiaddr>()
                .map_err(|_| format!("无效的 multiaddr 格式: {}", v[1]))?;
            Ok([v[0].clone(), v[1].clone()])
        })
        .collect::<Result<Vec<_>, _>>()?;

    let bootstrap: Vec<[String; 2]> = bootstrap_nodes
        .into_iter()
        .map(|v| {
            if v.len() != 2 {
                return Err("每个 bootstrap 节点必须包含 peer_id 和 multiaddr".to_string());
            }
            // 验证 multiaddr 格式
            v[1].parse::<libp2p::Multiaddr>()
                .map_err(|_| format!("无效的 multiaddr 格式: {}", v[1]))?;
            Ok([v[0].clone(), v[1].clone()])
        })
        .collect::<Result<Vec<_>, _>>()?;

    let config = openwire_core::p2p::nodes::NodesConfig {
        relay_nodes: relay,
        bootstrap_nodes: bootstrap,
    };

    config
        .save(&data_dir)
        .map_err(|e| format!("保存节点配置失败: {}", e))?;
    tracing::info!("节点配置已更新，重启后生效");
    Ok(())
}

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

/// 查询网络状态（用于前端网络监控组件）
#[tauri::command]
async fn get_network_status(state: tauri::State<'_, AppData>) -> Result<String, String> {
    let cmd_tx = {
        let inner = state.inner.read().await;
        inner.cmd_tx.clone()
    };
    let Some(cmd_tx) = cmd_tx else {
        return Ok(openwire_core::NetworkStatusData::error_json(
            openwire_core::NetworkStatusData::ERR_CORE_NOT_INITIALIZED,
            "OpenWire core is not initialized yet",
        ));
    };

    let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
    if let Err(e) = cmd_tx
        .try_send(openwire_core::ChatCommand::GetNetworkStatus { resp: resp_tx })
    {
        return Ok(openwire_core::NetworkStatusData::error_json(
            openwire_core::NetworkStatusData::ERR_CORE_CHANNEL_CLOSED,
            &format!("Core command channel closed: {}", e),
        ));
    }

    match resp_rx.await {
        Ok(json) => Ok(json),
        Err(e) => Ok(openwire_core::NetworkStatusData::error_json(
            openwire_core::NetworkStatusData::ERR_CORE_NO_RESPONSE,
            &format!("Core did not respond to status query: {}", e),
        )),
    }
}

/// 导出路由表到指定文件（仅含 PeerID 和 Multiaddr，不含任何密钥）
#[tauri::command]
async fn export_routing_table(
    state: tauri::State<'_, AppData>,
    save_path: String,
) -> Result<String, String> {
    let cmd_tx = {
        let inner = state.inner.read().await;
        inner.cmd_tx.clone()
    };

    let raw_path = std::path::PathBuf::from(&save_path);
    // 先创建父目录，确保 canonicalize 能成功
    if let Some(parent) = raw_path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|e| format!("创建目录失败: {}", e))?;
        }
    }
    let resolved = reject_sensitive_path(&raw_path)?;

    let cmd_tx = cmd_tx.ok_or_else(|| "核心尚未初始化".to_string())?;
    let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
    cmd_tx
        .try_send(openwire_core::ChatCommand::ExportRoutingTable { resp: resp_tx })
        .map_err(|e| format!("发送导出命令失败: {}", e))?;

    let json = resp_rx
        .await
        .map_err(|_| "核心未响应导出请求".to_string())?;

    std::fs::write(&resolved, &json).map_err(|e| format!("写入导出文件失败: {}", e))?;
    tracing::info!("路由表已导出到 {:?} ({} bytes)", resolved, json.len());
    Ok(json)
}

/// 导入路由表（将导出的 peers 加入本地路由表，不覆盖本地密钥/配置）
#[tauri::command]
async fn import_routing_table(
    state: tauri::State<'_, AppData>,
    data: String,
) -> Result<String, String> {
    if data.len() > 10 * 1024 * 1024 {
        return Err("路由表文件过大".to_string());
    }

    let cmd_tx = {
        let inner = state.inner.read().await;
        inner.cmd_tx.clone()
    };
    let cmd_tx = cmd_tx.ok_or_else(|| "核心尚未初始化".to_string())?;

    let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
    cmd_tx
        .try_send(openwire_core::ChatCommand::ImportRoutingTable { data, resp: resp_tx })
        .map_err(|e| format!("发送导入命令失败: {}", e))?;

    resp_rx
        .await
        .map_err(|_| "核心未响应导入请求".to_string())
}

pub struct AppData {
    pub inner: Arc<RwLock<AppDataInner>>,
}

pub struct AppDataInner {
    pub cmd_tx: Option<mpsc::Sender<ChatCommand>>,
    pub data_dir: PathBuf,
    pub mlkem_pubkey_hex: Option<String>,
    pub core_ready: bool,
}

fn create_placeholder_appdata(data_dir: PathBuf) -> AppData {
    AppData {
        inner: Arc::new(RwLock::new(AppDataInner {
            cmd_tx: None,
            data_dir,
            mlkem_pubkey_hex: None,
            core_ready: false,
        })),
    }
}

async fn setup_core_and_event_loop(
    mut chat_core_instance: openwire_core::ChatCore,
    cfg: openwire_core::CoreConfig,
    apphandle: tauri::AppHandle,
) {
    let app_data = apphandle.state::<AppData>();
    let mut inner = app_data.inner.write().await;
    inner.cmd_tx = Some(chat_core_instance.core_handle.cmd_tx.clone());
    inner.data_dir = cfg.data_dir.clone();
    inner.mlkem_pubkey_hex = chat_core_instance.mlkem_pubkey_hex.clone();
    inner.core_ready = true;
    drop(inner);

    apphandle.emit("core-ready", true).ok();
    tracing::info!("Core 初始化完成，已发送 core-ready 事件");

    let mut rx = match chat_core_instance.take_rx_message() {
        Some(rx) => rx,
        None => {
            tracing::error!("Failed to take message receiver");
            return;
        }
    };
    let app_handle_for_events = apphandle.clone();
    chat_core_instance.run();

    while let Some(msg) = rx.recv().await {
        match msg {
            MessageEvent::Log(data) => {
                app_handle_for_events.emit("log", data).ok();
            }
            MessageEvent::ReceiveMessage(msg) => {
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
                    let payload = serde_json::json!({
                        "message_hash": message_hash,
                        "peer_id": peer_id,
                    });
                    app_handle_for_events
                        .emit("message-sent", payload.to_string())
                        .ok();
                } else {
                    let json = serde_json::to_string(&msg).unwrap_or_default();
                    app_handle_for_events.emit("chat-message", json).ok();
                }
            }
            MessageEvent::OnlineStatus { online_contacts } => {
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

#[tauri::command]
async fn copy_file(state: tauri::State<'_, AppData>, src: String, dst: String) -> Result<(), String> {
    let data_dir = {
        let inner = state.inner.read().await;
        inner.data_dir.clone()
    };
    let data_canon = data_dir.canonicalize().map_err(|e| format!("data dir unavailable: {e}"))?;
    let src_path = std::path::PathBuf::from(&src);
    let src_canon = src_path.canonicalize().map_err(|e| format!("source file not found: {e}"))?;
    let ext = src_canon.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
    if !matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp") {
        return Err(format!("unsupported source file type: .{ext}"));
    }
    let dst_path = std::path::PathBuf::from(&dst);
    let parent = dst_path.parent().ok_or("no parent directory")?;
    std::fs::create_dir_all(parent).map_err(|e| format!("create dir failed: {e}"))?;
    let parent_canon = parent.canonicalize().map_err(|e| format!("destination dir unavailable: {e}"))?;
    if !parent_canon.starts_with(&data_canon) {
        return Err("destination path outside app data directory".into());
    }
    // 用父目录 canonical + 文件名构造目标路径。
    // 不能直接对 dst_path.canonicalize()：目标文件尚不存在时会失败。
    let file_name = dst_path
        .file_name()
        .ok_or("invalid destination filename")?;
    let dst_canon = parent_canon.join(file_name);
    if !dst_canon.starts_with(&data_canon) {
        return Err("destination path outside app data directory after resolution".into());
    }
    // 文件复制，最多重试 3 次（Windows 下目标文件可能被短暂占用）
    let mut last_err = None;
    for attempt in 0..3 {
        match std::fs::copy(&src_canon, &dst_canon) {
            Ok(_) => {
                // 复制成功后清理同目录下其他 background* 文件，防止堆积
                cleanup_old_backgrounds(&parent_canon, &dst_canon);
                return Ok(());
            }
            Err(e) => {
                last_err = Some(e);
                if attempt < 2 {
                    std::thread::sleep(std::time::Duration::from_millis(200));
                }
            }
        }
    }
    Err(format!(
        "copy file failed after 3 attempts: {}",
        last_err.map(|e| e.to_string()).unwrap_or_else(|| "unknown error".into())
    ))
}

/// 删除 dir 下除 keep 之外的所有 background* 文件
fn cleanup_old_backgrounds(dir: &std::path::Path, keep: &std::path::Path) {
    let keep_name = keep.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n,
            None => continue,
        };
        if name == keep_name {
            continue;
        }
        if name.starts_with("background") {
            let _ = std::fs::remove_file(&path);
        }
    }
}

/// 校验路径不指向系统敏感目录（服务端防线，防止绕过对话框/dir-isolation 直接读写系统文件）。
/// 返回规范化的路径（用于后续写入，避免 TOCTOU）。
/// 若路径不存在，尝试规范化最近存在的父目录；若完全不可解析，则回退到原始路径。
fn reject_sensitive_path(p: &std::path::Path) -> Result<std::path::PathBuf, String> {
    let canon = p
        .canonicalize()
        .or_else(|_| -> Result<std::path::PathBuf, String> {
            // 文件不存在（如导出新建文件），尝试规范化父目录
            let parent = p.parent().ok_or_else(|| format!("无法解析路径: {}", p.display()))?;
            let canon_parent = parent
                .canonicalize()
                .map_err(|_| format!("无法解析路径: {}", p.display()))?;
            Ok(canon_parent.join(p.file_name().unwrap_or_default()))
        })?;
    // 统一为正斜杠，保证 Windows 反斜杠路径与凭据目录模式匹配
    let s = canon
        .to_string_lossy()
        .to_lowercase()
        .replace('\\', "/");
    // 通用敏感目录：用户凭据与密钥目录（各平台统一拦截）
    for frag in [
        "/.ssh/",
        "/.gnupg/",
        "/.aws/",
        "/.config/gcloud/",
        "/.config/gh/",
        "/.config/github-copilot/",
        "/.npmrc",
        "/.pypirc",
        "/.netrc",
    ] {
        if s.contains(frag) {
            return Err("不允许访问用户凭据目录".to_string());
        }
    }
    #[cfg(windows)]
    {
        if s.contains(":/windows")
            || s.contains(":/program files")
            || s.contains(":/programdata")
            || s.contains(":/documents and settings")
            || s.contains("/windows/")
            || s.contains("/users/") && s.contains("/appdata/roaming/microsoft/")
        {
            return Err("不允许访问系统敏感路径".to_string());
        }
        // 浏览器配置文件（Cookies/历史记录等）
        if s.contains("/user data/")
            && (s.contains("/chrome") || s.contains("/edge") || s.contains("/brave"))
        {
            return Err("不允许访问浏览器配置文件".to_string());
        }
        if s.contains("/mozilla/firefox/") {
            return Err("不允许访问浏览器配置文件".to_string());
        }
    }
    #[cfg(not(windows))]
    {
        if s.starts_with("/etc/")
            || s.starts_with("/usr/")
            || s.starts_with("/var/")
            || s.starts_with("/root/")
            || s.starts_with("/bin/")
            || s.starts_with("/sbin/")
            || s.starts_with("/boot/")
            || s.starts_with("/dev/")
            || s.starts_with("/proc/")
            || s.starts_with("/sys/")
        {
            return Err("不允许访问系统敏感路径".to_string());
        }
        // 浏览器配置目录（Cookies/历史记录等）
        if s.contains("/.config/google-chrome/")
            || s.contains("/.config/microsoft-edge/")
            || s.contains("/.config/brave-browser/")
            || s.contains("/.mozilla/firefox/")
        {
            return Err("不允许访问浏览器配置文件".to_string());
        }
    }
    Ok(canon)
}

/// 读取文本文件（仅限用户通过对话框选择的导入文件，禁止系统敏感目录）
#[tauri::command]
async fn read_text_file(path: String) -> Result<String, String> {
    let resolved = reject_sensitive_path(std::path::Path::new(&path))?;
    std::fs::read_to_string(&resolved).map_err(|e| format!("read file failed: {}", e))
}

/// 设置计费网络检测模式（free / paid / disabled）
#[tauri::command]
async fn set_paid_network(state: tauri::State<'_, AppData>, mode: String) -> Result<(), String> {
    if !["free", "paid", "disabled"].contains(&mode.as_str()) {
        return Err(format!("未知的计费网络模式: {mode}"));
    }
    let cmd_tx = {
        let inner = state.inner.read().await;
        inner.cmd_tx.clone()
    };
    let cmd_tx = cmd_tx.ok_or_else(|| "核心尚未初始化".to_string())?;
    cmd_tx
        .try_send(openwire_core::ChatCommand::SetPaidNetworkMode(mode))
        .map_err(|e| format!("发送命令失败: {}", e))?;
    Ok(())
}

/// 拨号到指定节点（手动连接，PeerID + Multiaddr）
#[tauri::command]
async fn dial_peer(state: tauri::State<'_, AppData>, peer_id: String, addr: String) -> Result<(), String> {
    let peer_id: libp2p::PeerId = peer_id
        .parse()
        .map_err(|_| "无效的 PeerID".to_string())?;
    let multiaddr: libp2p::Multiaddr = addr
        .parse()
        .map_err(|_| "无效的 Multiaddr".to_string())?;
    let cmd_tx = {
        let inner = state.inner.read().await;
        inner.cmd_tx.clone()
    };
    let cmd_tx = cmd_tx.ok_or_else(|| "核心尚未初始化".to_string())?;
    cmd_tx
        .try_send(openwire_core::ChatCommand::DialPeer {
            peer_id: peer_id.to_string(),
            addr: multiaddr.to_string(),
        })
        .map_err(|e| format!("发送拨号命令失败: {e}"))?;
    Ok(())
}

/// 设置中继角色（server / client / off，互斥）
#[tauri::command]
async fn set_relay_role(state: tauri::State<'_, AppData>, role: String) -> Result<(), String> {
    if !["server", "client", "off"].contains(&role.as_str()) {
        return Err(format!("未知的中继角色: {role}"));
    }
    let cmd_tx = {
        let inner = state.inner.read().await;
        inner.cmd_tx.clone()
    };
    let cmd_tx = cmd_tx.ok_or_else(|| "核心尚未初始化".to_string())?;
    cmd_tx
        .try_send(openwire_core::ChatCommand::SetRelayRole(role))
        .map_err(|e| format!("发送命令失败: {}", e))?;
    Ok(())
}

#[tauri::command]
async fn get_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_store::Builder::new().build())
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let apphandle = app.handle().clone();

            let data_dir = apphandle.path().app_data_dir().unwrap_or_default();
            std::fs::create_dir_all(&data_dir).ok();
            apphandle.manage(create_placeholder_appdata(data_dir.clone()));

            tauri::async_runtime::spawn(async move {
                let log_path = match apphandle.path().app_log_dir() {
                    Ok(dir) => dir,
                    Err(e) => {
                        tracing::error!("Failed to get app log directory: {}", e);
                        return;
                    }
                };

                std::fs::create_dir_all(&log_path).ok();
                #[cfg(debug_assertions)]
                let log_level = "debug";
                #[cfg(not(debug_assertions))]
                let log_level = "info";

                let cfg =
                    openwire_core::CoreConfig::new(data_dir, Some(log_path), Some(log_level));
                let mut cfg = cfg;

                // 检查 Keyring 是否可用，如果不可用则发送错误事件让前端显示提示
                if !rootcell::identity::PrivateKeyHandle::check_keyring_available() {
                    tracing::error!("Keyring is not available. OpenWire requires a system keyring to store encryption keys.");
                    let err_msg = "系统密钥环（Keyring）不可用。OpenWire 需要系统密钥环来安全存储加密密钥。\
                        \n\n请确保：\
                        \n  - Windows: Credential Manager（通常默认可用）\
                        \n  - macOS: Keychain（通常默认可用）\
                        \n  - Linux: 安装 gnome-keyring 或 kwallet\
                        \n  - Android/iOS: 平台内置密钥环".to_string();
                    tracing::error!("{}", err_msg);
                    apphandle.emit("keyring-unavailable", &err_msg).ok();
                    apphandle.emit("core-init-failed", err_msg.clone()).ok();
                    apphandle.emit("warning", err_msg).ok();
                    return;
                }

                cfg.load_nodes_config();

                match openwire_core::ChatCore::try_init(cfg.clone()).await {
                    Ok(chat_core_instance) => {
                        setup_core_and_event_loop(chat_core_instance, cfg, apphandle).await;
                    }
                    Err(e) => {
                        let err_msg = format!("Core 初始化失败: {}", e);
                        tracing::error!("{}", err_msg);
                        apphandle.emit("core-init-failed", err_msg.clone()).ok();
                        apphandle.emit("warning", err_msg).ok();
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
            load_messages,
            get_identity_qr_data,
            is_keyring_available,
            check_core_ready,
            delete_contact,
            delete_message,
            get_nodes_config,
            save_nodes_config,
            reset_nodes_config,
            list_sent_files,
            delete_sent_file,
copy_file,
            get_version,
            get_network_status,
            export_routing_table,
            import_routing_table,
read_text_file,
            set_paid_network,
set_relay_role,
            dial_peer
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
    let app_data = match app.try_state::<AppData>() {
        Some(data) => data,
        None => {
            tracing::warn!("AppData not initialized yet, skipping cleanup");
            return;
        }
    };

    let inner = app_data.inner.blocking_read();

    let cmd_tx = match &inner.cmd_tx {
        Some(tx) => tx,
        None => {
            tracing::warn!("Core not initialized yet, skipping cleanup");
            return;
        }
    };

    if let Err(e) = cmd_tx.try_send(openwire_core::ChatCommand::Shutdown) {
        tracing::error!("Error sending shutdown command: {}", e);
        app.emit("warning", format!("Error sending shutdown command: {e}"))
            .ok();
    }
}

#[cfg(target_os = "android")]
#[no_mangle]
pub extern "system" fn Java_com_openwire_app_MainActivity_initNdkContext<'local>(
    mut env: jni::EnvUnowned<'local>,
    _class: jni::objects::JClass<'local>,
    context: jni::objects::JObject<'local>,
) {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        use std::ffi::c_void;
        let _ = env.with_env(|env| {
            match env.new_global_ref(context) {
                Ok(global_ref) => match env.get_java_vm() {
                    Ok(vm) => {
                        let vm_ptr = vm.get_raw() as *mut c_void;
                        let context_ptr = global_ref.into_raw() as *mut c_void;
                        unsafe {
                            ndk_context::initialize_android_context(vm_ptr, context_ptr);
                        }
                        tracing::info!("Android NDK context initialized for keyring");
                        rootcell::identity::setup_default_keyring();
                        Ok(())
                    }
                    Err(e) => {
                        tracing::error!("Failed to get Java VM: {e}");
                        Err(e)
                    }
                },
                Err(e) => {
                    tracing::error!("Failed to create global ref for Android context: {e}");
                    Err(e)
                }
            }
        });
    }));
    if let Err(e) = result {
        tracing::error!("JNI initNdkContext panicked: {e:?}");
    }
}
