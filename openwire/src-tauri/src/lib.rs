use openwire_core::storage;
use openwire_core::{ChatCommand, ChatMessageType, IncomingMessage, MessageEvent};
use serde::Serialize;
use std::path::PathBuf;
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager, RunEvent};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons};
use tokio::sync::mpsc;
use tokio::sync::RwLock;

/// 将 `[N] hex` 格式的消息内容转换为用于前端显示的 JSON 字符串
fn decode_message_content(content: &str) -> String {
    // 尝试解析 `[N] hex` 格式（FileHash, FileStream, FileDownloadRequest 等）
    if let Some(stripped) = content.strip_prefix('[') {
        if let Some(rest) = stripped.split(']').next() {
            if let Ok(msgtype) = rest.parse::<u8>() {
                let hex_part = content[content.find(']').unwrap() + 1..].trim();
                if msgtype == ChatMessageType::FileHash as u8 {
                    // 尝试解析 FileHashInfo
                    if let Ok(bytes) = hex::decode(hex_part) {
                        if let Ok(info) = postcard::from_bytes::<openwire_core::message::FileHashInfo>(&bytes) {
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

#[tauri::command]
async fn send(
    state: tauri::State<'_, AppData>,
    mldsa_pubkey_hex: &str,
    message: &str,
) -> Result<bool, String> {
    if mldsa_pubkey_hex.is_empty() {
        return Err("联系人标识不能为空".to_string());
    }

    let _public_key_bytes =
        hex::decode(mldsa_pubkey_hex).map_err(|e| format!("无效的 pubkey 格式: {}", e))?;

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

    let path = std::path::PathBuf::from(file_path);
    if !path.exists() {
        return Err(format!("文件不存在: {}", file_path));
    }

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
        openwire_core::message::FileHashInfo::new(filename, total_size, file_hash, file_id);
    let file_info_bytes = postcard::to_allocvec(&file_info)
        .map_err(|e| format!("序列化 FileHashInfo 失败: {}", e))?;

    let inner = state.inner.read().await;
    let cmd_tx = inner
        .cmd_tx
        .clone()
        .ok_or_else(|| "核心尚未初始化".to_string())?;
    drop(inner);

    cmd_tx
        .send(ChatCommand::RegisterFileForDownload {
            file_id,
            file_path: path,
        })
        .await
        .map_err(|e| format!("注册文件失败: {}", e))?;

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
        limit.unwrap_or(50),
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

    let confirmed = rx.await.map_err(|_| "对话框通信失败".to_string())?;
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

    match storage::delete_messages_by_peer(pool, &owner_identity_id, mldsa_pubkey_hex).await {
        Ok(deleted_msgs) => {
            tracing::info!("已删除 {} 条与 {} 的聊天记录", deleted_msgs, &mldsa_pubkey_hex[..16]);
        }
        Err(e) => {
            tracing::error!("删除聊天记录失败: {}", e);
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
    save_path: Option<String>,
) -> Result<(), String> {
    let file_hash_bytes =
        hex::decode(file_hash_hex).map_err(|e| format!("无效的 file_hash hex: {}", e))?;
    if file_hash_bytes.len() != 32 {
        return Err("file_hash 长度必须为 32 字节".to_string());
    }
    let mut file_hash = [0u8; 32];
    file_hash.copy_from_slice(&file_hash_bytes);

    let inner = state.inner.read().await;
    let result = inner
        .cmd_tx
        .as_ref()
        .ok_or_else(|| "核心尚未初始化".to_string())?
        .send(ChatCommand::RequestFileDownload {
            sender_mldsa_pubkey_hex: sender_mldsa_pubkey_hex.to_string(),
            file_hash,
            save_path: save_path.map(PathBuf::from),
        })
        .await;
    match result {
        Ok(_) => Ok(()),
        Err(e) => Err(format!("请求文件下载失败: {}", e)),
    }
}

#[tauri::command]
async fn set_download_dir(state: tauri::State<'_, AppData>, path: &str) -> Result<bool, String> {
    let download_path = PathBuf::from(path);
    let cmd_tx = {
        let mut inner = state.inner.write().await;
        inner.download_dir = Some(download_path.clone());
        inner.cmd_tx.clone()
    };

    if let Some(cmd_tx) = cmd_tx {
        cmd_tx
            .send(ChatCommand::SetDownloadDir {
                path: download_path.clone(),
            })
            .await
            .map_err(|e| format!("设置下载目录失败: {}", e))?;
    } else {
        tracing::info!("核心尚未初始化，已缓存下载目录: {}", download_path.display());
    }

    Ok(true)
}

#[tauri::command]
async fn get_download_dir(
    app_handle: tauri::AppHandle,
    state: tauri::State<'_, AppData>,
) -> Result<String, String> {
    let inner = state.inner.read().await;
    if let Some(ref download_dir) = inner.download_dir {
        return Ok(download_dir.to_string_lossy().to_string());
    }
    drop(inner);

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

pub struct AppData {
    pub inner: Arc<RwLock<AppDataInner>>,
}

pub struct AppDataInner {
    pub cmd_tx: Option<mpsc::Sender<ChatCommand>>,
    pub data_dir: PathBuf,
    pub mlkem_pubkey_hex: Option<String>,
    pub download_dir: Option<PathBuf>,
    pub core_ready: bool,
}

fn create_placeholder_appdata() -> AppData {
    AppData {
        inner: Arc::new(RwLock::new(AppDataInner {
            cmd_tx: None,
            data_dir: PathBuf::new(),
            mlkem_pubkey_hex: None,
            download_dir: None,
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

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_store::Builder::new().build())
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let apphandle = app.handle().clone();

            apphandle.manage(create_placeholder_appdata());

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

                std::fs::create_dir_all(&data_dir).ok();
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
            set_download_dir,
            get_download_dir,
            load_messages,
            get_identity_qr_data,
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
pub extern "system" fn Java_com_openwire_app_MainActivity_initNdkContext(
    env: jni::JNIEnv,
    _class: jni::objects::JClass,
    context: jni::objects::JObject,
) {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        use std::ffi::c_void;
        match env.new_global_ref(context) {
            Ok(global_ref) => {
                match env.get_java_vm() {
                    Ok(vm) => {
                        let vm_ptr = vm.get_java_vm_pointer() as *mut c_void;
                        unsafe {
                            ndk_context::initialize_android_context(vm_ptr, global_ref.as_obj().as_raw() as _);
                        }
                        tracing::info!("Android NDK context initialized for keyring");
                        rootcell::identity::setup_default_keyring();
                    }
                    Err(e) => {
                        tracing::error!("Failed to get Java VM: {e}");
                    }
                }
            }
            Err(e) => {
                tracing::error!("Failed to create global ref for Android context: {e}");
            }
        }
    }));
    if let Err(e) = result {
        tracing::error!("JNI initNdkContext panicked: {e:?}");
    }
}