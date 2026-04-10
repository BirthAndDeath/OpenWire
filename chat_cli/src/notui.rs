use crate::App;
use chat_core::ChatCommand;
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
                MessageEvent::Log => {
                    println!("[日志] {}", msg.data);
                }
                MessageEvent::Warning => {
                    println!("[警告] {}", msg.data);
                }
                MessageEvent::Error => {
                    eprintln!("[错误] {}", msg.data);
                }
            }
        }
    });

    let input_handler = tokio::spawn(async move {
        println!("输入消息回车发送,Ctrl+C 退出：");

        loop {
            // 获取Peer ID
            print!("请输入对方Peer ID: ");
            std::io::stdout().flush().unwrap();

            let mut peer_id = String::new();
            std::io::stdin().read_line(&mut peer_id).unwrap();
            let peer_id = peer_id.trim();

            if peer_id.parse::<libp2p::PeerId>().is_err() {
                println!("Peer ID格式不正确，请重新输入");
                continue;
            }

            // 获取消息内容
            print!("请输入消息: ");
            std::io::stdout().flush().unwrap();

            let mut message = String::new();
            std::io::stdin().read_line(&mut message).unwrap();
            let message = message.trim();

            if message.is_empty() {
                println!("消息不能为空，退出程序");
                continue;
            }

            // 发送消息
            if !handle.send_msg(peer_id, message).await {
                eprintln!("无法发送消息");
                continue;
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
