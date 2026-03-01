
use tauri::{Emitter, Manager};
use chat_core::ChatCommand;
use libp2p::{

    gossipsub};
    use chat_core::{ChatMessage, MessageEvent};
// Learn more about Tauri commands at https://tauri.app/develop/calling-rust/
#[tauri::command]
async fn send(state: tauri::State<'_, AppData>, message: &str,receiver:Vec<u8>) -> Result<bool, String> {
    let result = state
        .cmd_tx
        .send(ChatCommand::SendMessage {
            message:{ChatMessage{
                receiver,
                data: message.to_string(),
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
    pub topic:gossipsub::IdentTopic,
}
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let rt = tokio::runtime::Runtime::new().unwrap();

    let handle = rt.handle().clone();
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let apphandle = app.handle().clone();
            std::thread::spawn(move || {
                handle.block_on(async {
                    let local = tokio::task::LocalSet::new();
                    let _result_local = local.run_until(async {
                        // 创建通道用于这里 -> core 通信
                        let (cmd_tx,cmd_rx) = mpsc::channel::<ChatCommand>(64);

                       
                        // ❗ Swarm 必须在 spawn_local 中（!Send）

                        let db_path = apphandle
                            .path()
                            .app_data_dir()
                            .expect("获取数据目录失败")
                            .join("database.sqlite");
                        let log_path = apphandle
                            .path()
                            .app_log_dir()
                            .expect("获取log目录失败")
                            .join("log.txt");

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
                            topic: core.topic.clone(),
                        });




                        let mut rx = core.rx_message.take().unwrap();
                        // 启动事件转发任务（多线程安全）
                        let app_handle_for_events = apphandle.clone();
                        core.run();

                        // 主事件循环
                        loop {
                    tokio::select! {
                           
                            Some(msg) = rx.recv()=> {
                                app_handle_for_events.emit("chat-message", msg.data).ok();
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
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
