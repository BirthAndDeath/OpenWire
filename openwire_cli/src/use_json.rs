use crate::App;
use crate::error::CliError;
use hex;
use openwire_core::storage;
use openwire_core::{IncomingMessage, MessageEvent, validate_mldsa_pubkey_hex};
use serde_json::json;
use tokio::io::AsyncBufReadExt;
use tokio::io::AsyncWriteExt;
use tokio::io::BufReader;
/// JSON 模式运行 - 面向 shell 调用，输出精简的 JSON 格式
pub async fn json_run(app: &mut App) -> Result<(), CliError> {
    // 获取对 core 的引用并启动它
    let mut core = app.core.take().ok_or(CliError::CoreNotInitialized)?;
    let rx = core
        .take_rx_message()
        .ok_or(CliError::ChannelNotInitialized)?;
    let handle = core.core_handle.clone();

    // 启动核心服务
    let core_joinhandle = core.run();

    // 创建一个任务来处理传入的消息（JSON 格式）
    let message_handler = tokio::spawn(async move {
        let mut rx = rx;
        while let Some(msg) = rx.recv().await {
            let json_output = match msg {
                MessageEvent::ReceiveMessage(incoming) => match incoming {
                    IncomingMessage::Text { text, sender } => {
                        json!({
                            "type": "message",
                            "subtype": "text",
                            "data": {
                                "text": text,
                                "sender": sender,
                            },
                            "timestamp": chrono::Utc::now().to_rfc3339()
                        })
                    }
                    IncomingMessage::FileShare {
                        filename,
                        file_hash,
                        total_size,
                        sender,
                    } => {
                        json!({
                            "type": "message",
                            "subtype": "file_share",
                            "data": {
                                "filename": filename,
                                "file_hash": file_hash,
                                "total_size": total_size,
                                "sender": sender,
                            },
                            "timestamp": chrono::Utc::now().to_rfc3339()
                        })
                    }
                    IncomingMessage::DeliveryReceipt {
                        message_hash,
                        peer_id,
                    } => {
                        json!({
                            "type": "message",
                            "subtype": "delivery_receipt",
                            "data": {
                                "message_hash": message_hash,
                                "peer_id": peer_id,
                            },
                            "timestamp": chrono::Utc::now().to_rfc3339()
                        })
                    }
                    IncomingMessage::MessageSent {
                        message_hash,
                        peer_id,
                    } => {
                        json!({
                            "type": "message",
                            "subtype": "message_sent",
                            "data": {
                                "message_hash": message_hash,
                                "peer_id": peer_id,
                            },
                            "timestamp": chrono::Utc::now().to_rfc3339()
                        })
                    }
                },
                MessageEvent::OnlineStatus { online_contacts } => {
                    json!({
                        "type": "online_status",
                        "data": {
                            "count": online_contacts.len(),
                            "online_contacts": online_contacts,
                        },
                        "timestamp": chrono::Utc::now().to_rfc3339()
                    })
                }
                MessageEvent::Log(data) => {
                    json!({
                        "type": "log",
                        "data": data,
                        "timestamp": chrono::Utc::now().to_rfc3339()
                    })
                }
                MessageEvent::Warning(data) => {
                    json!({
                        "type": "warning",
                        "data": data,
                        "timestamp": chrono::Utc::now().to_rfc3339()
                    })
                }
                MessageEvent::Error(data) => {
                    json!({
                        "type": "error",
                        "data": data,
                        "timestamp": chrono::Utc::now().to_rfc3339()
                    })
                }
                MessageEvent::FileTransferProgress(progress) => {
                    json!({
                        "type": "file_transfer_progress",
                        "data": {
                            "filename": progress.filename,
                            "chunk_index": progress.chunk_index,
                            "total_chunks": progress.total_chunks,
                            "received_bytes": progress.received_bytes,
                            "total_size": progress.total_size,
                            "status": progress.status,
                        },
                        "timestamp": chrono::Utc::now().to_rfc3339()
                    })
                }
                MessageEvent::ContactOnlineStatus {
                    mldsa_pubkey_hex,
                    online,
                } => {
                    json!({
                        "type": "contact_online_status",
                        "data": {
                            "mldsa_pubkey_hex": mldsa_pubkey_hex,
                            "online": online,
                        },
                        "timestamp": chrono::Utc::now().to_rfc3339()
                    })
                }
                MessageEvent::IdentityChanged { .. } => {
                    json!({
                        "type": "identity_changed",
                        "timestamp": chrono::Utc::now().to_rfc3339()
                    })
                }
            };
            let json_str = serde_json::to_string(&json_output)
                .unwrap_or_else(|_| r#"{"type":"error","data":"序列化失败"}"#.to_string());
            println!("{}", json_str);
            // 确保立即输出
            tokio::io::stdout().flush().await.ok();
        }
    });

    let input_handler = tokio::spawn(async move {
        let stdin = tokio::io::stdin();
        let mut reader = BufReader::new(stdin);
        let mut line = String::new();

        loop {
            line.clear();
            let n = reader.read_line(&mut line).await?;
            if n == 0 {
                break;
            }
            let input = line.trim().to_string();

            if input.is_empty() {
                continue;
            }

            // 解析 JSON 输入
            let parsed: serde_json::Value = match serde_json::from_str(&input) {
                Ok(v) => v,
                Err(e) => {
                    let error_output = json!({
                        "type": "error",
                        "data": format!("无效的 JSON 输入: {}", e),
                        "timestamp": chrono::Utc::now().to_rfc3339()
                    });
                    let err_str = serde_json::to_string(&error_output)
                        .unwrap_or_else(|_| r#"{"type":"error","data":"序列化失败"}"#.to_string());
                    eprintln!("{}", err_str);
                    continue;
                }
            };

            // 处理命令
            if let Some(cmd) = parsed.get("command").and_then(|v| v.as_str()) {
                match cmd {
                    "list_contacts" => {
                        // 每次需要时重新获取 pool 引用
                        if let Some(pool_ref) = storage::pool() {
                            let owner = storage::get_current_identity(pool_ref)
                                .await
                                .ok()
                                .flatten()
                                .unwrap_or_default();
                            match storage::list_contacts(pool_ref, &owner).await {
                                Ok(contacts) => {
                                    let contacts_json: Vec<serde_json::Value> = contacts
                                        .iter()
                                        .map(|c| {
                                            json!({
                                                "mldsa_pubkey_hex": c.mldsa_pubkey_hex,
                                                "name": c.name,
                                                "added_at": c.added_at
                                            })
                                        })
                                        .collect();
                                    let output = json!({
                                        "type": "contacts",
                                        "data": contacts_json,
                                        "timestamp": chrono::Utc::now().to_rfc3339()
                                    });
                                    println!("{}", serde_json::to_string(&output)?);
                                    tokio::io::stdout().flush().await.ok();
                                }
                                Err(e) => {
                                    let error_output = json!({
                                        "type": "error",
                                        "data": format!("加载联系人失败: {}", e),
                                        "timestamp": chrono::Utc::now().to_rfc3339()
                                    });
                                    let err_str = serde_json::to_string(&error_output)
                                        .unwrap_or_else(|_| {
                                            r#"{"type":"error","data":"序列化失败"}"#.to_string()
                                        });
                                    eprintln!("{}", err_str);
                                }
                            }
                        }
                    }
                    "send_message" => {
                        if let (Some(mldsa_pubkey_hex), Some(message)) = (
                            parsed.get("mldsa_pubkey_hex").and_then(|v| v.as_str()),
                            parsed.get("message").and_then(|v| v.as_str()),
                        ) {
                            if !validate_mldsa_pubkey_hex(mldsa_pubkey_hex) {
                                let error_output = json!({
                                    "type": "error",
                                    "data": crate::MLDSA_PUBKEY_INVALID,
                                    "timestamp": chrono::Utc::now().to_rfc3339()
                                });
                                let err_str =
                                    serde_json::to_string(&error_output).unwrap_or_else(|_| {
                                        r#"{"type":"error","data":"序列化失败"}"#.to_string()
                                    });
                                eprintln!("{}", err_str);
                                continue;
                            }

                            if !handle.send_msg(mldsa_pubkey_hex, message).await {
                                let error_output = json!({
                                    "type": "error",
                                    "data": "无法发送消息",
                                    "timestamp": chrono::Utc::now().to_rfc3339()
                                });
                                let err_str =
                                    serde_json::to_string(&error_output).unwrap_or_else(|_| {
                                        r#"{"type":"error","data":"序列化失败"}"#.to_string()
                                    });
                                eprintln!("{}", err_str);
                                continue;
                            }

                            let success_output = json!({
                                "type": "sent",
                                "data": {
                                    "mldsa_pubkey_hex": mldsa_pubkey_hex,
                                    "message": message
                                },
                                "timestamp": chrono::Utc::now().to_rfc3339()
                            });
                            println!("{}", serde_json::to_string(&success_output)?);
                            tokio::io::stdout().flush().await.ok();
                        } else {
                            let error_output = json!({
                                "type": "error",
                                "data": "缺少 mldsa_pubkey_hex 或 message 字段",
                                "timestamp": chrono::Utc::now().to_rfc3339()
                            });
                            let err_str =
                                serde_json::to_string(&error_output).unwrap_or_else(|_| {
                                    r#"{"type":"error","data":"序列化失败"}"#.to_string()
                                });
                            eprintln!("{}", err_str);
                        }
                    }
                    "send_file" => {
                        if let (Some(mldsa_pubkey_hex), Some(file_path)) = (
                            parsed.get("mldsa_pubkey_hex").and_then(|v| v.as_str()),
                            parsed.get("file_path").and_then(|v| v.as_str()),
                        ) {
                            if !validate_mldsa_pubkey_hex(mldsa_pubkey_hex) {
                                let error_output = json!({
                                    "type": "error",
                                    "data": crate::MLDSA_PUBKEY_INVALID,
                                    "timestamp": chrono::Utc::now().to_rfc3339()
                                });
                                let err_str =
                                    serde_json::to_string(&error_output).unwrap_or_else(|_| {
                                        r#"{"type":"error","data":"序列化失败"}"#.to_string()
                                    });
                                eprintln!("{}", err_str);
                                continue;
                            }
                            if !handle
                                .send_file(mldsa_pubkey_hex, std::path::Path::new(file_path))
                                .await
                            {
                                let error_output = json!({
                                    "type": "error",
                                    "data": "发送文件失败",
                                    "timestamp": chrono::Utc::now().to_rfc3339()
                                });
                                let err_str =
                                    serde_json::to_string(&error_output).unwrap_or_else(|_| {
                                        r#"{"type":"error","data":"序列化失败"}"#.to_string()
                                    });
                                eprintln!("{}", err_str);
                                continue;
                            }
                            let success_output = json!({
                                "type": "sent",
                                "data": {
                                    "mldsa_pubkey_hex": mldsa_pubkey_hex,
                                    "file_path": file_path
                                },
                                "timestamp": chrono::Utc::now().to_rfc3339()
                            });
                            println!("{}", serde_json::to_string(&success_output)?);
                            tokio::io::stdout().flush().await.ok();
                        } else {
                            let error_output = json!({
                                "type": "error",
                                "data": "缺少 mldsa_pubkey_hex 或 file_path 字段",
                                "timestamp": chrono::Utc::now().to_rfc3339()
                            });
                            let err_str =
                                serde_json::to_string(&error_output).unwrap_or_else(|_| {
                                    r#"{"type":"error","data":"序列化失败"}"#.to_string()
                                });
                            eprintln!("{}", err_str);
                        }
                    }
                    "request_file_download" => {
                        if let (
                            Some(sender_mldsa_pubkey_hex),
                            Some(file_hash_hex),
                            Some(save_path),
                        ) = (
                            parsed
                                .get("sender_mldsa_pubkey_hex")
                                .and_then(|v| v.as_str()),
                            parsed.get("file_hash_hex").and_then(|v| v.as_str()),
                            parsed.get("save_path").and_then(|v| v.as_str()),
                        ) {
                            if !validate_mldsa_pubkey_hex(sender_mldsa_pubkey_hex) {
                                let error_output = json!({
                                    "type": "error",
                                    "data": &format!("发送方 {}", crate::MLDSA_PUBKEY_INVALID),
                                    "timestamp": chrono::Utc::now().to_rfc3339()
                                });
                                let err_str =
                                    serde_json::to_string(&error_output).unwrap_or_else(|_| {
                                        r#"{"type":"error","data":"序列化失败"}"#.to_string()
                                    });
                                eprintln!("{}", err_str);
                                continue;
                            }
                            let file_hash_bytes = match hex::decode(file_hash_hex) {
                                Ok(b) if b.len() == 32 => b,
                                _ => {
                                    let error_output = json!({
                                        "type": "error",
                                        "data": "file_hash_hex 无效，应为64字符的hex编码",
                                        "timestamp": chrono::Utc::now().to_rfc3339()
                                    });
                                    let err_str = serde_json::to_string(&error_output)
                                        .unwrap_or_else(|_| {
                                            r#"{"type":"error","data":"序列化失败"}"#.to_string()
                                        });
                                    eprintln!("{}", err_str);
                                    continue;
                                }
                            };
                            let mut file_hash = [0u8; 32];
                            file_hash_bytes
                                .iter()
                                .enumerate()
                                .for_each(|(i, &b)| file_hash[i] = b);
                            if !handle
                                .request_file_download(
                                    sender_mldsa_pubkey_hex,
                                    file_hash,
                                    std::path::PathBuf::from(save_path),
                                )
                                .await
                            {
                                let error_output = json!({
                                    "type": "error",
                                    "data": "请求文件下载失败",
                                    "timestamp": chrono::Utc::now().to_rfc3339()
                                });
                                let err_str =
                                    serde_json::to_string(&error_output).unwrap_or_else(|_| {
                                        r#"{"type":"error","data":"序列化失败"}"#.to_string()
                                    });
                                eprintln!("{}", err_str);
                                continue;
                            }
                            let success_output = json!({
                                "type": "download_requested",
                                "data": {
                                    "sender_mldsa_pubkey_hex": sender_mldsa_pubkey_hex,
                                    "save_path": save_path
                                },
                                "timestamp": chrono::Utc::now().to_rfc3339()
                            });
                            println!("{}", serde_json::to_string(&success_output)?);
                            tokio::io::stdout().flush().await.ok();
                        } else {
                            let error_output = json!({
                                "type": "error",
                                "data": "缺少 sender_mldsa_pubkey_hex、file_hash_hex 或 save_path 字段",
                                "timestamp": chrono::Utc::now().to_rfc3339()
                            });
                            let err_str =
                                serde_json::to_string(&error_output).unwrap_or_else(|_| {
                                    r#"{"type":"error","data":"序列化失败"}"#.to_string()
                                });
                            eprintln!("{}", err_str);
                        }
                    }
                    "exit" | "quit" => {
                        break;
                    }
                    _ => {
                        let error_output = json!({
                            "type": "error",
                            "data": format!("未知命令: {}", cmd),
                            "timestamp": chrono::Utc::now().to_rfc3339()
                        });
                        let err_str = serde_json::to_string(&error_output).unwrap_or_else(|_| {
                            r#"{"type":"error","data":"序列化失败"}"#.to_string()
                        });
                        eprintln!("{}", err_str);
                    }
                }
            } else {
                let error_output = json!({
                    "type": "error",
                    "data": "缺少 command 字段",
                    "timestamp": chrono::Utc::now().to_rfc3339()
                });
                let err_str = serde_json::to_string(&error_output)
                    .unwrap_or_else(|_| r#"{"type":"error","data":"序列化失败"}"#.to_string());
                eprintln!("{}", err_str);
            }
        }

        Ok::<(), CliError>(())
    });

    // 等待输入处理任务完成
    let _ = tokio::join!(message_handler, input_handler);

    // 发送关闭命令
    app.core_handle.shutdown();

    // 使用 spawn_blocking 等待核心服务结束，避免阻塞 tokio 线程
    let result = tokio::task::spawn_blocking(move || core_joinhandle.join()).await;
    match result {
        Ok(join_result) => {
            let exit_output = json!({
                "type": "exit",
                "data": format!("{:?}", join_result),
                "timestamp": chrono::Utc::now().to_rfc3339()
            });
            println!("{}", serde_json::to_string(&exit_output)?);
        }
        Err(e) => {
            let error_output = json!({
                "type": "error",
                "data": format!("等待后台线程失败: {:?}", e),
                "timestamp": chrono::Utc::now().to_rfc3339()
            });
            eprintln!("{}", serde_json::to_string(&error_output)?);
        }
    }

    Ok(())
}
