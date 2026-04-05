use crate::App;
use chat_core::ChatCommand;
use chat_core::ChatMessage;
use chat_core::ChatMessageType;

use chat_core::MessageEvent;
use std::io::Write;

pub async fn no_tui_run(app: &mut App) -> std::io::Result<()> {
    // 获取对 core 的引用并启动它
    let mut core = app.core.take().unwrap();
    let mut rx = core.rx_message.take().unwrap();
    let handle = core.core_handle.clone();

    // 启动核心服务
    let core_joinhandle = core.run();

    // 创建一个任务来处理传入的消息
    let message_handler = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            match msg.event {
                MessageEvent::ReceiveMessage => {
                    println!("[网络] {}", msg.data);
                }
                _ => {}
            }
        }
    });

    // 创建一个任务来处理用户输入
    let cmd_tx = app.core_handle.cmd_tx.clone();
    let input_handler = tokio::spawn(async move {
        println!("输入消息回车发送,Ctrl+C 退出：");

        loop {
            // 获取Peer ID
            print!("请输入对方Peer ID: ");
            std::io::stdout().flush().unwrap();

            let mut peer_id = String::new();
            std::io::stdin().read_line(&mut peer_id).unwrap();
            let peer_id = peer_id.trim();

            if peer_id.is_empty() {
                println!("Peer ID不能为空，退出程序");
                break;
            }

            // 获取消息内容
            print!("请输入消息: ");
            std::io::stdout().flush().unwrap();

            let mut message = String::new();
            std::io::stdin().read_line(&mut message).unwrap();
            let message = message.trim();

            if message.is_empty() {
                println!("消息不能为空，退出程序");
                break;
            }

            // 发送消息
            if let false = handle
                .send_msg(
                    peer_id,
                    ChatMessage {
                        msgtype: ChatMessageType::Text,
                        data: message.to_string().into_bytes(),
                    },
                )
                .await
            {
                eprintln!("无法发送消息");
                break;
            }

            // 显示自己发送的消息
            println!("[我] {}", message);
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
