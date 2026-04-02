use tauri::{AppHandle, Emitter, Manager, RunEvent};
use chat_core::{ChatCommand, ChatMessageType};
use chat_core::{ChatMessage, MessageEvent};
mod p2p_protocol;
// Learn more about Tauri commands at https://tauri.app/develop/calling-rust/
#[tauri::command]
async fn send(state: tauri::State<'_, AppData>, message: &str) -> Result<bool, String> {
    let result = state
        .cmd_tx
        .send(ChatCommand::SendMessage {
            message:{ChatMessage{
                msgtype:ChatMessageType::Text,
                
                data: message.to_string().into_bytes(),
            }
            
        }
        })
        .await;

    match result {
        Ok(_) => Ok(true),
        Err(e) => Err(format!("发送消息失败: {}", e)),
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
                            // 创建通道用于这里 -> core 通信
                            let (cmd_tx,cmd_rx) = mpsc::channel::<ChatCommand>(64);
    
                           
                        
    
                            let db_path = apphandle
                                .path()
                                .app_data_dir()
                                .expect("获取数据目录失败")
                                .join("database.sqlite");
                            let log_path = apphandle
                                .path()
                                .app_log_dir()
                                .expect("获取log目录失败")
                                ;
    
                            // 确保目录存在
                            if let Some(parent) = db_path.parent() {
                                std::fs::create_dir_all(parent).ok();
                            }
                            if let Some(parent) = log_path.parent() {
                                std::fs::create_dir_all(parent).ok();
                            }
                            #[cfg(debug_assertions)]
            let log_level = "debug";
            #[cfg(not(debug_assertions))]
            let log_level = "info";
    
                            let cfg = chat_core::CoreConfig::new(
                                db_path,
                                cmd_rx,
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
                                cmd_tx: cmd_tx.clone(),//you can use this to send commands to here

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
                                        MessageEvent::NewMessage => {
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
            .invoke_handler(tauri::generate_handler![send])
            .build(tauri::generate_context!())
      .expect("error while running tauri application")
            .run(|apphandle, event| match event {
    RunEvent::Exit => cleanup(apphandle),
    RunEvent::Ready=> {},
    _=>{}
})
            
    }
const FORCE_EXIT_TIMEOUT :std::time::Duration= std::time::Duration::from_secs(30) ;
  fn cleanup(app: &AppHandle) {
    let start_time = std::time::Instant::now();
    while let Err(e) = app.state::<AppData>().cmd_tx.try_send(chat_core::ChatCommand::Shutdown) {
        if start_time.elapsed() >= FORCE_EXIT_TIMEOUT{
            tracing::error!("Shutdown command timeout after 30 seconds, forcing exit");
            app.emit("warning","Shutdown command timeout after 30 seconds, forcing exit" ).ok();
            std::thread::sleep(std::time::Duration::from_millis(100));
            break;
        }
        app.emit("warning",format!("Error sending shutdown command: {e}")).ok();
        tracing::error!("Error sending shutdown command: {}", e);
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}