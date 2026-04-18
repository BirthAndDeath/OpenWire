use tauri::{AppHandle, Emitter, Manager, RunEvent};
use serde::Serialize;
use chat_core::{ChatCommand, ChatMessageType, MessageEvent};
use chat_core::storage;
mod p2p_protocol;
// Learn more about Tauri commands at https://tauri.app/develop/calling-rust/
#[tauri::command]
async fn send(state: tauri::State<'_, AppData>, peer_id: &str, message: &str) -> Result<bool, String> {
    let peer_id: libp2p::PeerId = match peer_id.parse() {
        Ok(p) => p,
        Err(e) => return Err(format!("无效的 PeerId: {}", e)),
    };

    let pool = storage::pool().ok_or_else(|| "数据库尚未初始化".to_string())?;
    storage::upsert_contact(pool, &peer_id.to_string(), None)
        .await
        .map_err(|e| format!("保存联系人失败: {}", e))?;
    storage::add_message(pool, &peer_id.to_string(), message, true, false)
        .await
        .map_err(|e| format!("保存消息失败: {}", e))?;

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

#[derive(Serialize)]
struct IdentityDto {
    id: i64,
    peer_id: String,
    is_current: bool,
}

#[tauri::command]
async fn list_contacts() -> Result<Vec<ContactDto>, String> {
    let pool = storage::pool().ok_or_else(|| "数据库尚未初始化".to_string())?;
    let contacts = storage::list_contacts(pool)
        .await
        .map_err(|e| format!("加载联系人失败: {}", e))?
        .into_iter()
        .map(|c| ContactDto {
            peer_id: c.peer_id,
            name: c.name.unwrap_or_else(|| "未知联系人".to_string()),
            added_at: c.added_at,
        })
        .collect();
    Ok(contacts)
}

#[tauri::command]
async fn list_identities() -> Result<Vec<IdentityDto>, String> {
    let pool = storage::pool().ok_or_else(|| "数据库尚未初始化".to_string())?;
    let identities = storage::list_identities(pool)
        .await
        .map_err(|e| format!("加载身份失败: {}", e))?
        .into_iter()
        .map(|id| IdentityDto {
            id: id.id,
            peer_id: id.peer_id,
            is_current: id.is_current == 1,
        })
        .collect();
    Ok(identities)
}

#[tauri::command]
async fn select_identity(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppData>,
    peer_id: &str,
) -> Result<bool, String> {
    // Validate peer_id format
    let _peer_id: libp2p::PeerId = match peer_id.parse() {
        Ok(p) => p,
        Err(e) => return Err(format!("无效的 PeerId: {}", e)),
    };

    // Check if identity exists
    let pool = storage::pool().ok_or_else(|| "数据库尚未初始化".to_string())?;
    let identities = storage::list_identities(pool)
        .await
        .map_err(|e| format!("加载身份失败: {}", e))?;
    if !identities.iter().any(|id| id.peer_id == peer_id) {
        return Err("身份不存在".to_string());
    }

    let result = state
        .cmd_tx
        .send(ChatCommand::SelectIdentity {
            peer_id: peer_id.to_string(),
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
    peer_id: &str,
) -> Result<bool, String> {
    let _peer_id: libp2p::PeerId = match peer_id.parse() {
        Ok(p) => p,
        Err(e) => return Err(format!("无效的 PeerId: {}", e)),
    };

    let pool = storage::pool().ok_or_else(|| "数据库尚未初始化".to_string())?;
    let identities = storage::list_identities(pool)
        .await
        .map_err(|e| format!("加载身份失败: {}", e))?;
    if !identities.iter().any(|id| id.peer_id == peer_id) {
        return Err("身份不存在".to_string());
    }

    let result = state
        .cmd_tx
        .send(ChatCommand::DeleteIdentity {
            peer_id: peer_id.to_string(),
        })
        .await;
    match result {
        Ok(_) => Ok(true),
        Err(e) => Err(format!("删除身份失败: {}", e)),
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

use tokio::sync::mpsc;


pub struct AppData {
    pub cmd_tx: mpsc::Sender<ChatCommand>,
}
    #[cfg_attr(mobile, tauri::mobile_entry_point)]
    pub fn run() {
        let rt = tokio::runtime::Runtime::new().unwrap();
    
        let handle = rt.handle().clone();
        tauri::Builder::default()
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
    
                            let mut core = match chat_core::ChatCore::try_init(cfg).await {
                                Ok(c) => c,
                                Err(e) => {
                                    eprintln!("Core 初始化失败: {}", e);
                                    return;
                                }
                            };
                            apphandle.manage(AppData {
                                cmd_tx: core.core_handle.cmd_tx.clone(),

                            });
    
    
    
    
                            let mut rx = core.rx_message.take().unwrap();
                            // 启动事件转发任务（多线程安全）
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
            .invoke_handler(tauri::generate_handler![send, list_contacts, list_identities, select_identity, delete_identity, generate_identity])
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