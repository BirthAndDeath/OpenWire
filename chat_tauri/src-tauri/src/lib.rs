use chat_core::storage;
use chat_core::{ChatCommand, ChatMessageType, MessageEvent};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, RunEvent};
mod p2p_protocol;
// Learn more about Tauri commands at https://tauri.app/develop/calling-rust/
#[tauri::command]
async fn send(
    state: tauri::State<'_, AppData>,
    pubkey_identity_id: &str, // ML-KEM公钥的hex编码
    message: &str,
) -> Result<bool, String> {
    // 验证pubkey_identity_id格式（应该是hex编码的ML-KEM公钥）
    if pubkey_identity_id.is_empty() {
        return Err("pubkey身份ID不能为空".to_string());
    }

    // 尝试解析为hex，验证格式
    let public_key_bytes =
        hex::decode(pubkey_identity_id).map_err(|e| format!("无效的pubkey身份ID格式: {}", e))?;

    // 严格验证 ML-KEM-768 公钥长度 (1184 bytes)
    const MLKEM768_PUBLIC_KEY_SIZE: usize = 1184;
    if public_key_bytes.len() != MLKEM768_PUBLIC_KEY_SIZE {
        return Err(format!(
            "无效的ML-KEM公钥长度: 期望 {} 字节, 实际 {} 字节",
            MLKEM768_PUBLIC_KEY_SIZE,
            public_key_bytes.len()
        ));
    }

    let pool = storage::pool().ok_or_else(|| "数据库尚未初始化".to_string())?;

    // 保存消息到数据库（使用 pubkey 作为标识）
    storage::add_message(pool, pubkey_identity_id, message, true, false)
        .await
        .map_err(|e| format!("保存消息失败: {}", e))?;

    // 通过 chat_core 提供的优雅 API 从 DHT 查询 PeerID
    let peer_id = chat_core::lookup_peerid_by_pubkey(&state.data_dir, pubkey_identity_id)
        .map_err(|e| format!("查询 DHT 失败: {}", e))?
        .ok_or_else(|| {
            format!(
                "未找到 pubkey {} 对应的 PeerID，对方可能不在线",
                pubkey_identity_id
            )
        })?;

    let result = state
        .cmd_tx
        .send(ChatCommand::SendMessage {
            peerid: peer_id,
            msgtype: ChatMessageType::Text,
            data: message.to_string().into_bytes(),
        })
        .await;

    match result {
        Ok(_) => Ok(true),
        Err(e) => Err(format!("发送消息失败: {}", e)),
    }
}

#[derive(Serialize)]
struct ContactDto {
    peer_id: String,
    name: String,
    added_at: i64,
}

#[tauri::command]
async fn list_contacts() -> Result<Vec<ContactDto>, String> {
    let pool = storage::pool().ok_or_else(|| "数据库尚未初始化".to_string())?;
    let contacts = storage::list_contacts(pool)
        .await
        .map_err(|e| format!("加载联系人失败: {}", e))?
        .into_iter()
        .map(|contact| ContactDto {
            peer_id: contact.peer_id.clone(),
            name: contact.name.unwrap_or(contact.peer_id),
            added_at: contact.added_at,
        })
        .collect();
    Ok(contacts)
}

#[derive(Serialize)]
struct MlKemIdentityDto {
    id: i64,
    identity_id: String,
    public_key_hex: String,
    is_current: bool,
}

#[tauri::command]
async fn list_identities() -> Result<Vec<MlKemIdentityDto>, String> {
    let pool = storage::pool().ok_or_else(|| "数据库尚未初始化".to_string())?;
    let identities = storage::list_mlkem_identities(pool)
        .await
        .map_err(|e| format!("加载身份失败: {}", e))?
        .into_iter()
        .map(|id| MlKemIdentityDto {
            id: id.id,
            identity_id: id.identity_id.clone(),
            public_key_hex: hex::encode(&id.public_key),
            is_current: id.is_current == 1,
        })
        .collect();
    Ok(identities)
}

#[tauri::command]
async fn select_identity(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppData>,
    identity_id: &str,
) -> Result<(), String> {
    // Check if identity exists
    let pool = storage::pool().ok_or_else(|| "数据库尚未初始化".to_string())?;
    let identities = storage::list_mlkem_identities(pool)
        .await
        .map_err(|e| format!("加载身份失败: {}", e))?;
    if !identities.iter().any(|id| id.identity_id == identity_id) {
        return Err("身份不存在".to_string());
    }

    let result = state
        .cmd_tx
        .send(ChatCommand::SelectIdentity {
            peer_id: identity_id.to_string(),
        })
        .await;
    match result {
        Ok(_) => {
            // 切换身份成功后重启应用
            app.restart();
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
    let identities = storage::list_mlkem_identities(pool)
        .await
        .map_err(|e| format!("加载身份失败: {}", e))?;
    if !identities.iter().any(|id| id.identity_id == identity_id) {
        return Err("身份不存在".to_string());
    }

    let result = state
        .cmd_tx
        .send(ChatCommand::DeleteIdentity {
            peer_id: identity_id.to_string(),
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
    pubkey_identity_id: &str, // ML-KEM公钥的hex编码
    name: Option<String>,
) -> Result<bool, String> {
    // 验证pubkey_identity_id格式
    if pubkey_identity_id.is_empty() {
        return Err("pubkey身份ID不能为空".to_string());
    }

    // 验证hex格式
    let public_key =
        hex::decode(pubkey_identity_id).map_err(|e| format!("无效的pubkey身份ID格式: {}", e))?;

    // 严格验证 ML-KEM-768 公钥长度 (1184 bytes)
    const MLKEM768_PUBLIC_KEY_SIZE: usize = 1184;
    if public_key.len() != MLKEM768_PUBLIC_KEY_SIZE {
        return Err(format!(
            "无效的ML-KEM公钥长度: 期望 {} 字节, 实际 {} 字节",
            MLKEM768_PUBLIC_KEY_SIZE,
            public_key.len()
        ));
    }

    let result = state
        .cmd_tx
        .send(ChatCommand::AddContact {
            peer_id: pubkey_identity_id.to_string(), // 使用pubkey身份ID作为peer_id
            public_key,
            name,
        })
        .await;
    match result {
        Ok(_) => Ok(true),
        Err(e) => Err(format!("添加好友失败: {}", e)),
    }
}

#[tauri::command]
async fn generate_identity(state: tauri::State<'_, AppData>) -> Result<bool, String> {
    let result = state.cmd_tx.send(ChatCommand::GenerateIdentity).await;
    match result {
        Ok(_) => Ok(true),
        Err(e) => Err(format!("生成身份失败: {}", e)),
    }
}

use std::path::PathBuf;
use tokio::sync::mpsc;

pub struct AppData {
    pub cmd_tx: mpsc::Sender<ChatCommand>,
    pub data_dir: PathBuf,
}
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let rt = tokio::runtime::Runtime::new().unwrap();

    let handle = rt.handle().clone();
    tauri::Builder::default()
        .plugin(tauri_plugin_stronghold::Builder::new(|pass| todo!()).build())
        .plugin(tauri_plugin_store::Builder::new().build())
        /* .register_uri_scheme_protocol("p2p", |app, request| {
            let uri = request.uri().to_string();
            
            let parts: Vec<&str> = uri.strip_prefix("p2p://")
                .unwrap_or("")
                .splitn(2, '/')
                .collect();
            
            if parts.len() != 2 {
                return tauri::http::Response::builder()
                    .status(400)
                    .body(Vec::new())
                    .unwrap();
            }
            
            let peer_id = parts[0];
            let resource = format!("/{}", parts[1]);
            
            // 获取状态
            let p2p = app.state::<P2PState>();
            
            let result = tauri::async_runtime::block_on(async {
                // ... P2P 请求逻辑
                vec![]
            });
            
            tauri::http::Response::builder()
                .status(200)
                .header("Content-Type", "application/octet-stream")
                .body(result)
                .unwrap()
        })*/
            .plugin(tauri_plugin_opener::init())          
            .setup(|app| {              
                let apphandle = app.handle().clone();
                
                std::thread::spawn(move || {
                    handle.block_on(async {
                        let local = tokio::task::LocalSet::new();
                        let _result_local = local.run_until(async {
                            
    
                           
                        
    
let data_dir = apphandle
                                .path()
                                .app_data_dir()
                                .expect("获取数据目录失败");
                            let log_path = apphandle
                                .path()
                                .app_log_dir()
                                .expect("获取log目录失败")
                                ;

                            // 确保目录存在
                            std::fs::create_dir_all(&data_dir).ok();
                            std::fs::create_dir_all(&log_path).ok();
                            #[cfg(debug_assertions)]
                            let log_level = "debug";
                            #[cfg(not(debug_assertions))]
                            let log_level = "info";

                            let cfg = chat_core::CoreConfig::new(
                                data_dir,
                                
                                Some(log_path),
                                Some(log_level)
                            );
    
                            let mut core = match chat_core::ChatCore::try_init(cfg.clone()).await {
                                Ok(c) => c,
                                Err(e) => {
                                    eprintln!("Core 初始化失败: {}", e);
                                    return;
                                }
                            };
                            apphandle.manage(AppData {
                                cmd_tx: core.core_handle.cmd_tx.clone(),
                                data_dir: cfg.data_dir.clone(),

                            });
    
    
    
    
                            let mut rx = core.rx_message.take().unwrap();
                            // 启动核心服务（在独立线程中运行）
                            let app_handle_for_events = apphandle.clone();
                            core.run();

                            // 主事件循环
                            loop {
                        tokio::select! {
                               
                                Some(msg) = rx.recv()=> {
                                    match msg.event {
                                        MessageEvent::Log => {
                                          
                                        }
                                        MessageEvent::ReceiveMessage => {
                                            app_handle_for_events.emit("chat-message", msg.data).ok();
                                        }
                                        MessageEvent::Warning => {
                                            app_handle_for_events.emit("warning", msg.data).ok();
                                        }
                                        MessageEvent::Error => {
                                            app_handle_for_events.emit("error", msg.data).ok();
                                        }
                                        
                                    }
                                    
                                }
                                // 心跳
                                _ = tokio::time::sleep(std::time::Duration::from_millis(100)) => {}
                            }
                    }
    
    
                
                        }).await;
                    });
                });
    
                Ok(())
            })
            .invoke_handler(tauri::generate_handler![send, list_contacts, list_identities, select_identity, delete_identity, generate_identity, add_contact])
            .build(tauri::generate_context!())
      .expect("error while running tauri application")
            .run(|apphandle, event| match event {
    RunEvent::Exit => cleanup(apphandle),
    RunEvent::Ready=> {},
    _=>{}
})
}
const FORCE_EXIT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
fn cleanup(app: &AppHandle) {
    let start_time = std::time::Instant::now();
    while let Err(e) = app
        .state::<AppData>()
        .cmd_tx
        .try_send(chat_core::ChatCommand::Shutdown)
    {
        if start_time.elapsed() >= FORCE_EXIT_TIMEOUT {
            tracing::error!("Shutdown command timeout after 30 seconds, forcing exit");
            app.emit(
                "warning",
                "Shutdown command timeout after 30 seconds, forcing exit",
            )
            .ok();
            std::thread::sleep(std::time::Duration::from_millis(100));
            break;
        }
        app.emit("warning", format!("Error sending shutdown command: {e}"))
            .ok();
        tracing::error!("Error sending shutdown command: {}", e);
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}
