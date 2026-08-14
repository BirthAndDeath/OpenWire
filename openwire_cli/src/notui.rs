use crate::App;
use crate::error::CliError;
use openwire_core::IncomingMessage;
use openwire_core::MessageEvent;
use openwire_core::storage;
use openwire_core::validate_mldsa_pubkey_hex;

use tokio::io::AsyncBufReadExt;
use tokio::io::AsyncWriteExt;
use tokio::io::BufReader;

/// 移除字符串中的终端转义序列，防止终端注入攻击
/// 保留可打印 ASCII 与普通非 ASCII 字符（CJK/emoji 等），剥离：
/// - C0/C1 控制字符（ESC 等）
/// - Unicode 格式控制字符（bidi 覆盖/定向符、零宽字符等，General_Category=Cf）
/// 使用 unicode-properties 的 General_Category API，而非硬编码码位范围，
/// 随 Unicode 数据表演进自动覆盖新增字符。
fn strip_escape(s: &str) -> String {
    use unicode_properties::GeneralCategory;
    use unicode_properties::UnicodeGeneralCategory;
    s.chars()
        .filter(|&c| {
            if c.is_control() {
                return false;
            }
            let gc = c.general_category();
            if gc == GeneralCategory::Format
                || gc == GeneralCategory::LineSeparator
                || gc == GeneralCategory::ParagraphSeparator
            {
                return false;
            }
            c.is_ascii_graphic() || c == ' ' || !c.is_ascii()
        })
        .collect()
}

pub async fn no_tui_run(app: &mut App) -> Result<(), CliError> {
    // 获取对 core 的引用并启动它
    let mut core = app.core.take().ok_or(CliError::CoreNotInitialized)?;
    let rx = core
        .take_rx_message()
        .ok_or(CliError::ChannelNotInitialized)?;
    let handle = core.core_handle.clone();

    // 启动核心服务
    let core_joinhandle = core.run();

    // 创建一个任务来处理传入的消息
    let message_handler = tokio::spawn(async move {
        let mut rx = rx;
        while let Some(msg) = rx.recv().await {
            match msg {
                MessageEvent::ReceiveMessage(incoming) => match incoming {
                    IncomingMessage::Text { text, sender } => {
                        let short_sender = if sender.len() > 16 {
                            format!("{}...", &sender[..16])
                        } else {
                            sender.clone()
                        };
                        println!("[{}] {}", short_sender, strip_escape(&text));
                    }
                    IncomingMessage::FileShare {
                        filename,
                        file_id,
                        total_size,
                        sender,
                        ..
                    } => {
                        let short_sender = if sender.len() > 16 {
                            format!("{}...", &sender[..16])
                        } else {
                            sender.clone()
                        };
                        println!(
                            "[文件] {} 向你分享文件: {} ({} bytes, id: {})",
                            short_sender, strip_escape(&filename), total_size, file_id
                        );
                    }
                    IncomingMessage::DeliveryReceipt { peer_id, .. } => {
                        println!("[系统] 消息已送达 ✓ (from: {})", peer_id);
                    }
                    IncomingMessage::MessageSent { .. } => {
                        // 消息已发送通知，notui 模式不需要额外输出
                    }
                },
                MessageEvent::OnlineStatus { online_contacts } => {
                    println!("[系统] 当前在线: {} 个连接", online_contacts.len());
                    if !online_contacts.is_empty() {
                        println!("[系统] 在线联系人:");
                        for pubkey in &online_contacts {
                            let short = if pubkey.len() > 16 {
                                format!("{}...", &pubkey[..16])
                            } else {
                                pubkey.clone()
                            };
                            println!("  ● {}", short);
                        }
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
                        strip_escape(&progress.filename),
                        progress.received_bytes,
                        progress.total_size,
                        progress.status,
                    );
                }
                MessageEvent::ContactOnlineStatus {
                    mldsa_pubkey_hex,
                    online,
                } => {
                    let short = if mldsa_pubkey_hex.len() > 16 {
                        format!("{}...", &mldsa_pubkey_hex[..16])
                    } else {
                        mldsa_pubkey_hex.clone()
                    };
                    let status = if online { "在线" } else { "离线" };
                    println!("[在线状态] {} {}", short, status);
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
                println!("{}，请重新输入", crate::MLDSA_PUBKEY_INVALID);
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

        Ok::<(), CliError>(())
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
