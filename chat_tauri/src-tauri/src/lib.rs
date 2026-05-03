use chat_core::storage;
use chat_core::{ChatCommand, ChatMessageType, MessageEvent};
use serde::Serialize;
use std::path::PathBuf;
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager, RunEvent};
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

    // 检查文件是否存在
    let path = std::path::PathBuf::from(file_path);
    if !path.exists() {
        return Err(format!("文件不存在: {}", file_path));
    }

    // 计算文件 hash（SHA256）
    let file_hash = chat_core::transfer::compute_file_hash(&path)
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
    let file_info = chat_core::message::FileHashInfo::new(filename, total_size, file_hash, file_id);
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
    let owner_identity_id = storage::get_current_identity(pool)
        .await
        .map_err(|e| format!("获取当前身份失败: {}", e))?
        .ok_or_else(|| "未选择身份".to_string())?;
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
}

#[tauri::command]
async fn load_messages(
    mldsa_pubkey_hex: &str,
    before: Option<i64>,
    limit: Option<i64>,
) -> Result<Vec<MessageDto>, String> {
    let pool = storage::pool().ok_or_else(|| "数据库尚未初始化".to_string())?;
    let owner_identity_id = storage::get_current_identity(pool)
        .await
        .map_err(|e| format!("获取当前身份失败: {}", e))?
        .ok_or_else(|| "未选择身份".to_string())?;
    let msgs = storage::get_messages(
        pool,
        &owner_identity_id,
        mldsa_pubkey_hex,
        before,
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
    let inner = state.inner.read().await;
    let result = inner
        .cmd_tx
        .as_ref()
        .ok_or_else(|| "核心尚未初始化".to_string())?
        .send(ChatCommand::SetDownloadDir {
            path: download_path,
        })
        .await;
    match result {
        Ok(_) => Ok(true),
        Err(e) => Err(format!("设置下载目录失败: {}", e)),
    }
}

/// 获取当前下载目录
#[tauri::command]
async fn get_download_dir(state: tauri::State<'_, AppData>) -> Result<String, String> {
    let inner = state.inner.read().await;
    // 从 AppDataInner 获取 data_dir，然后构造默认下载目录
    let data_dir = inner.data_dir.clone();
    drop(inner);

    // 默认下载目录为 data_dir/downloads
    let default_download_dir = data_dir.join("downloads");
    Ok(default_download_dir.to_string_lossy().to_string())
}

pub struct AppData {
    pub inner: Arc<RwLock<AppDataInner>>,
}

pub struct AppDataInner {
    pub cmd_tx: Option<mpsc::Sender<ChatCommand>>,
    pub data_dir: PathBuf,
    /// 当前会话的 ML-KEM 公钥 hex（用于前端显示）
    pub mlkem_pubkey_hex: Option<String>,
}

/// 用于在核心初始化完成前占位的初始状态
fn create_placeholder_appdata() -> AppData {
    AppData {
        inner: Arc::new(RwLock::new(AppDataInner {
            cmd_tx: None,
            data_dir: PathBuf::new(),
            mlkem_pubkey_hex: None,
        })),
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

                let cfg = chat_core::CoreConfig::new(data_dir, Some(log_path), Some(log_level));

                let mut core = match chat_core::ChatCore::try_init(cfg.clone()).await {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::error!("Core 初始化失败: {}", e);
                        return;
                    }
                };

                // 用真实的 AppData 替换占位状态
                let app_data = apphandle.state::<AppData>();
                let mut inner = app_data.inner.write().await;
                inner.cmd_tx = Some(core.core_handle.cmd_tx.clone());
                inner.data_dir = cfg.data_dir.clone();
                inner.mlkem_pubkey_hex = core.mlkem_pubkey_hex.clone();
                drop(inner);

                let mut rx = match core.take_rx_message() {
                    Some(rx) => rx,
                    None => {
                        tracing::error!("Failed to take message receiver");
                        return;
                    }
                };
                // 启动核心服务（在独立线程中运行）
                let app_handle_for_events = apphandle.clone();
                core.run();

                // 主事件循环 — 直接等待消息，无需心跳
                while let Some(msg) = rx.recv().await {
                    match msg {
                        MessageEvent::Log(data) => {
                            app_handle_for_events.emit("log", data).ok();
                        }
                        MessageEvent::ReceiveMessage(data) => {
                            app_handle_for_events.emit("chat-message", data).ok();
                        }
                        MessageEvent::Warning(data) => {
                            app_handle_for_events.emit("warning", data).ok();
                        }
                        MessageEvent::Error(data) => {
                            app_handle_for_events.emit("error", data).ok();
                        }
                        MessageEvent::FileTransferProgress(progress) => {
                            // 文件传输进度事件，data 是 FileTransferProgress 结构体
                            // Tauri 自动序列化为 JSON 发送到前端
                            app_handle_for_events
                                .emit("file-transfer-progress", progress)
                                .ok();
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
            get_identity_qr_data
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
    if let Err(e) = cmd_tx.try_send(chat_core::ChatCommand::Shutdown) {
        tracing::error!("Error sending shutdown command: {}", e);
        app.emit("warning", format!("Error sending shutdown command: {e}"))
            .ok();
    }
}
