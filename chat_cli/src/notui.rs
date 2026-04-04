use crate::App;
use chat_core::ChatCommand;
use chat_core::ChatMessage;
use chat_core::ChatMessageType;
use tokio::io::{self, AsyncBufReadExt, BufReader};
pub async fn no_tui_run(app: &mut App) -> std::io::Result<()> {
    // 获取对 core 的引用并启动它
    let mut core = app.core.take().unwrap();
    let mut rx = core
        .rx_message
        .take()
        .ok_or(std::io::Error::new(
            std::io::ErrorKind::Other,
            "消息通道问题",
        ))
        .expect("消息通道问题");

    // 启动核心服务
    let core_joinhandle = core.run();

    // 创建一个任务来处理传入的消息
    let message_handler = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            println!("[网络] {}", msg.data);
        }
    });

    // 创建一个任务来处理用户输入
    let cmd_tx = app.core_handle.cmd_tx.clone();
    let input_handler = tokio::spawn(async move {
        let stdin = io::stdin();
        let mut reader = BufReader::new(stdin);
        let mut buffer = String::new();

        println!("输入消息回车发送，Ctrl+C 退出：");

        loop {
            buffer.clear();
            if let Err(_) = reader.read_line(&mut buffer).await {
                break;
            }

            let line = buffer.trim();
            if !line.is_empty() {
                // 发送消息
                if let Err(_) = cmd_tx
                    .send(ChatCommand::SendMessage {
                        message: {
                            ChatMessage {
                                msgtype: ChatMessageType::Text,
                                data: line.to_string().into_bytes(),
                            }
                        },
                    })
                    .await
                {
                    eprintln!("无法发送消息");
                    break;
                }

                // 显示自己发送的消息
                println!("[我] {}", line);
            }
        }
    });

    // 等待输入处理任务完成（实际上不会完成，直到用户中断）
    let _ = tokio::join!(message_handler, input_handler);

    // 发送关闭命令
    let _ = app.core_handle.cmd_tx.try_send(ChatCommand::Shutdown);

    // 等待核心服务结束
    let result = core_joinhandle.join();

    println!("exit {:?}", result);
    Ok(())
}
