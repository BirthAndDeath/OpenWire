use crate::App;
use chat_core::ChatCommand;
use chat_core::MessageEvent;
use chat_core::storage;
use chat_core::validate_mldsa_pubkey_hex;

use tokio::io::AsyncBufReadExt;
use tokio::io::AsyncWriteExt;
use tokio::io::BufReader;

pub async fn no_tui_run(app: &mut App) -> anyhow::Result<()> {
    // 获取对 core 的引用并启动它
    let mut core = app
        .core
        .take()
        .ok_or_else(|| anyhow::anyhow!("核心未初始化"))?;
    let rx = core
        .take_rx_message()
        .ok_or_else(|| anyhow::anyhow!("消息通道未初始化"))?;
    let handle = core.core_handle.clone();

    // 启动核心服务
    let core_joinhandle = core.run();

    // 创建一个任务来处理传入的消息
    let message_handler = tokio::spawn(async move {
        let mut rx = rx;
        while let Some(msg) = rx.recv().await {
            match msg {
                MessageEvent::ReceiveMessage(data) => {
                    // 尝试解析 JSON 消息（新格式包含 sender 信息），失败则作为纯文本显示
                    if data.starts_with('{') {
                        if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&data) {
                            let msg_type = parsed
                                .get("type")
                                .and_then(|v| v.as_str())
                                .unwrap_or("text");
                            if msg_type == "delivery_receipt" {
                                // 送达回执
                                println!("[系统] 消息已送达 ✓");
                                continue;
                            }
                            let display_text =
                                parsed.get("text").and_then(|v| v.as_str()).unwrap_or(&data);
                            println!("[网络] {display_text}");
                        } else {
                            println!("[网络] {data}");
                        }
                    } else {
                        println!("[网络] {data}");
                    }
                }
                MessageEvent::Log(data) => {
                    println!("[日志] {data}");
                }
                MessageEvent::Warning(data) => {
                    println!("[警告] {data}");
                }
                MessageEvent::Error(data) => {
                    eprintln!("[错误] {data}");
                }
                MessageEvent::FileTransferProgress(progress) => {
                    println!(
                        "[文件传输] {} ({}/{}) - {}",
                        progress.filename,
                        progress.received_bytes,
                        progress.total_size,
                        progress.status,
                    );
                }
            }
        }
    });

    let input_handler = tokio::spawn(async move {
        println!("输入消息回车发送，Ctrl+C 退出：");

        let stdin = tokio::io::stdin();
        let mut reader = BufReader::new(stdin);
        let mut line = String::new();

        loop {
            // 显示联系人列表（每次循环重新获取 pool 引用）
            if let Some(pool_ref) = storage::pool() {
                let owner = storage::get_current_identity(pool_ref)
                    .await
                    .ok()
                    .flatten()
                    .unwrap_or_default();
                match storage::list_contacts(pool_ref, &owner).await {
                    Ok(contacts) => {
                        if contacts.is_empty() {
                            println!("当前没有联系人，请先添加好友。");
                        } else {
                            println!("\n--- 联系人列表 ---");
                            for (i, c) in contacts.iter().enumerate() {
                                let name = c.name.as_deref().unwrap_or("(未命名)");
                                let short_pk = if c.mldsa_pubkey_hex.len() > 16 {
                                    format!("{}...", &c.mldsa_pubkey_hex[..16])
                                } else {
                                    c.mldsa_pubkey_hex.clone()
                                };
                                println!("  {}. {} ({})", i + 1, name, short_pk);
                            }
                            println!("-------------------");
                        }
                    }
                    Err(e) => {
                        println!("加载联系人失败: {}", e);
                    }
                }
            }

            // 获取对方的 ML-DSA 公钥 hex
            print!("\n请输入对方 ML-DSA 公钥 (3904字符hex编码): ");
            tokio::io::stdout().flush().await?;

            line.clear();
            reader.read_line(&mut line).await?;
            let pubkey_hex = line.trim().to_string();

            if pubkey_hex.is_empty() {
                println!("输入为空，退出程序");
                break;
            }

            if !validate_mldsa_pubkey_hex(&pubkey_hex) {
                println!("ML-DSA 公钥格式不正确（应为3904字符的hex编码），请重新输入");
                continue;
            }

            // 获取消息内容
            print!("请输入消息: ");
            tokio::io::stdout().flush().await?;

            line.clear();
            reader.read_line(&mut line).await?;
            let message = line.trim().to_string();

            if message.is_empty() {
                println!("消息不能为空，请重新输入");
                continue;
            }

            // 发送消息
            if !handle.send_msg(&pubkey_hex, &message).await {
                eprintln!("无法发送消息");
                continue;
            }

            // 显示自己发送的消息
            println!("[我] {}", message);
        }

        anyhow::Ok(())
    });

    // 等待输入处理任务完成（实际上不会完成，直到用户中断）
    let _ = tokio::join!(message_handler, input_handler);

    // 发送关闭命令
    app.core_handle.shutdown();

    // 使用 spawn_blocking 等待核心服务结束，避免阻塞 tokio 线程
    let result = tokio::task::spawn_blocking(move || core_joinhandle.join()).await;
    match result {
        Ok(join_result) => println!("exit {:?}", join_result),
        Err(e) => println!("等待后台线程失败: {:?}", e),
    }

    Ok(())
}
