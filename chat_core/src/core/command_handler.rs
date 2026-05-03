use crate::{command::ChatCommand, core::ChatCore, crypto, message::ChatMessageType, p2p, storage};
use sha2::{Digest, Sha256};

impl ChatCore {
    /// 处理单个控制命令
    pub(crate) async fn handle_command(&mut self, cmd: ChatCommand) {
        match cmd {
            ChatCommand::SendMessage {
                mldsa_pubkey_hex,
                msgtype,
                data,
            } => match self.send_text(&mldsa_pubkey_hex, msgtype, data).await {
                Ok(_) => tracing::info!("{:?} message sent to {}", msgtype, mldsa_pubkey_hex),
                Err(e) => {
                    tracing::error!("Failed to send {:?} message: {e}", msgtype);
                    // 发送失败时通知 UI
                    let err_msg = format!("发送消息失败: {}", e);
                    self.send_warning_mpsc(err_msg).await;
                }
            },
            ChatCommand::RetryPendingMessages => {
                self.retry_pending_messages().await;
            }
            ChatCommand::AddContact {
                mldsa_pubkey_hex,
                mlkem_public_key,
                name,
                resp,
            } => {
                let result = self
                    .add_contact(mldsa_pubkey_hex, mlkem_public_key, name)
                    .await;
                let _ = resp.send(result);
            }
            ChatCommand::GenerateIdentity => self.generate_identity().await,
            ChatCommand::SelectIdentity { identity_id } => self.select_identity(identity_id).await,
            ChatCommand::DeleteIdentity { identity_id } => self.delete_identity(identity_id).await,
            ChatCommand::RequestFileDownload {
                sender_mldsa_pubkey_hex,
                file_id,
            } => {
                // 安全说明：download_dir 由 SetDownloadDir 命令统一管理
                // 此处不传递 download_dir 参数，防止路径遍历攻击
                self.handle_file_download_request(&sender_mldsa_pubkey_hex, file_id, None)
                    .await;
            }
            ChatCommand::SetDownloadDir { path } => {
                // 安全校验：确保下载路径在 data_dir 内，防止任意路径写入
                let canonical_data = match self.data_dir.canonicalize() {
                    Ok(d) => d,
                    Err(e) => {
                        tracing::error!("无法规范化 data_dir {:?}: {}", self.data_dir, e);
                        return;
                    }
                };
                let canonical_path = match path.canonicalize() {
                    Ok(p) => p,
                    Err(_) => {
                        // 如果路径不存在，尝试创建后再规范化
                        if let Err(e) = std::fs::create_dir_all(&path) {
                            tracing::error!("无法创建下载目录 {:?}: {}", path, e);
                            return;
                        }
                        match path.canonicalize() {
                            Ok(p) => p,
                            Err(e) => {
                                tracing::error!("无法规范化下载路径 {:?}: {}", path, e);
                                return;
                            }
                        }
                    }
                };
                if !canonical_path.starts_with(&canonical_data) {
                    tracing::error!(
                        "拒绝设置下载目录: {:?} 不在 data_dir {:?} 内",
                        canonical_path,
                        canonical_data
                    );
                    return;
                }
                self.download_dir = canonical_path;
                tracing::info!("Download directory set to {:?}", self.download_dir);
            }
            ChatCommand::RegisterFileForDownload { file_id, file_path } => {
                let file_id_hex = hex::encode(file_id);
                tracing::info!(
                    "Registering file for download: file_id={}.., path={:?}",
                    &file_id_hex[..16],
                    file_path
                );
                self.file_path_map.insert(file_id, file_path);
            }
            ChatCommand::DhtPublishIdentity {
                mldsa_pubkey_hex,
                peer_id,
                mlkem_pubkey_hex,
            } => {
                self.publish_identity_to_dht(&mldsa_pubkey_hex, &peer_id, &mlkem_pubkey_hex);
            }
            ChatCommand::DiscoverContact {
                mldsa_pubkey_hex,
                name,
            } => {
                self.discover_contact(&mldsa_pubkey_hex, name).await;
            }
            ChatCommand::Shutdown => {
                // Shutdown 在 run_inner 中处理，此处不应到达
                tracing::warn!(
                    "Shutdown command reached handle_command (should be handled in run_inner)"
                );
            }
        }
    }

    pub(crate) async fn send_text(
        &mut self,
        mldsa_pubkey_hex: &str,
        msgtype: ChatMessageType,
        data: Vec<u8>,
    ) -> anyhow::Result<()> {
        // 通过 DHT 查找接收方当前的 PeerID（临时传输层标识）
        // 先查本地数据库，如果未找到则发起网络 DHT 查询
        let recipient_peer_id = {
            // 使用缓存的 DHT 数据库连接，避免每次发送消息都打开/关闭数据库
            let store = self.get_dht_store()?;
            match store.get_peerid_by_pubkey(mldsa_pubkey_hex) {
                Ok(Some(peer_id)) => {
                    tracing::debug!(
                        "Found PeerID {} for {} in local DHT database",
                        peer_id,
                        &mldsa_pubkey_hex[..16]
                    );
                    peer_id
                }
                Ok(None) => {
                    // 本地未找到，尝试网络 DHT 查询
                    tracing::info!(
                        "PeerID not found locally for {}, trying network DHT lookup",
                        &mldsa_pubkey_hex[..16]
                    );
                    match p2p::lookup_peerid_by_pubkey_network(
                        &mut self.swarm,
                        &self.data_dir,
                        mldsa_pubkey_hex,
                    )
                    .await
                    {
                        Ok(Some(peer_id)) => {
                            // 网络查询成功，将结果缓存到本地数据库（使用缓存的连接）
                            if let Some(ref db) = self.dht_db {
                                let store = p2p::dht::RedbRecordStore::new(db.clone());
                                let _ = store.set_pubkey_peerid(mldsa_pubkey_hex, &peer_id);
                            }
                            peer_id
                        }
                        Ok(None) => {
                            // 接收方离线，保存为待发送消息（离线消息队列）
                            tracing::info!(
                                "联系人 {} 当前不在线，消息将保存到离线队列",
                                &mldsa_pubkey_hex[..16]
                            );
                            self.save_pending_message(mldsa_pubkey_hex, msgtype, &data)
                                .await;
                            return Err(anyhow::anyhow!(
                                "联系人 {} 当前不在线，消息已保存到离线队列",
                                &mldsa_pubkey_hex[..16]
                            ));
                        }
                        Err(e) => {
                            // 网络查询失败，也保存为待发送
                            tracing::warn!(
                                "网络 DHT 查询联系人 {} 的 PeerID 失败: {}，消息将保存到离线队列",
                                &mldsa_pubkey_hex[..16],
                                e
                            );
                            self.save_pending_message(mldsa_pubkey_hex, msgtype, &data)
                                .await;
                            return Err(anyhow::anyhow!(
                                "网络 DHT 查询失败: {}，消息已保存到离线队列",
                                e
                            ));
                        }
                    }
                }
                Err(e) => {
                    return Err(anyhow::anyhow!("DHT 查询失败: {}", e));
                }
            }
        };

        // 获取当前身份的 identity_id
        let owner_identity_id = self.mldsa_identity_id.as_deref().unwrap_or("");

        // 获取接收方的 ML-KEM 公钥（临时密钥交换密钥）
        // 优先从 contacts 表获取，如果为空则从 DHT 查找
        let recipient_mlkem_public_key = if let Some(pool) = storage::pool() {
            match storage::get_contact_mlkem_pubkey(pool, owner_identity_id, mldsa_pubkey_hex).await
            {
                Ok(Some(pubkey)) => {
                    tracing::debug!(
                        "Found ML-KEM pubkey for {} in contacts DB",
                        &mldsa_pubkey_hex[..16]
                    );
                    pubkey
                }
                Ok(None) => {
                    // 本地没有 ML-KEM 公钥，尝试从 DHT 查找（使用缓存的连接）
                    tracing::info!(
                        "ML-KEM pubkey not found locally for {}, trying DHT lookup",
                        &mldsa_pubkey_hex[..16]
                    );
                    let store = self.get_dht_store()?;
                    match store.get_mlkem_pubkey(mldsa_pubkey_hex) {
                        Ok(Some(mlkem_hex)) => {
                            tracing::info!(
                                "Found ML-KEM pubkey for {} via DHT lookup",
                                &mldsa_pubkey_hex[..16]
                            );
                            // 解码 hex 为 bytes
                            hex::decode(&mlkem_hex)
                                .map_err(|e| anyhow::anyhow!("DHT 中 ML-KEM 公钥格式无效: {}", e))?
                        }
                        Ok(None) => {
                            return Err(anyhow::anyhow!(
                                "未找到联系人 {} 的 ML-KEM 公钥，请先交换临时密钥（让对方添加你为好友后重试）",
                                mldsa_pubkey_hex
                            ));
                        }
                        Err(e) => {
                            return Err(anyhow::anyhow!("DHT 查询 ML-KEM 公钥失败: {}", e));
                        }
                    }
                }
                Err(e) => {
                    return Err(anyhow::anyhow!("查询联系人 ML-KEM 公钥失败: {}", e));
                }
            }
        } else {
            return Err(anyhow::anyhow!("数据库连接不可用"));
        };

        // 加密消息数据（使用接收方 ML-KEM 公钥）
        let encrypted_data = crypto::encrypt_message(&data, &recipient_mlkem_public_key)?;

        // 构建签名的消息（包含加密后的数据）
        let message = self.build_signed_message(msgtype, encrypted_data).await?;
        self.send_message(recipient_peer_id, message);
        Ok(())
    }

    /// 保存待发送消息到离线队列
    async fn save_pending_message(
        &mut self,
        mldsa_pubkey_hex: &str,
        msgtype: ChatMessageType,
        data: &[u8],
    ) {
        let owner_identity_id = self.mldsa_identity_id.as_deref().unwrap_or("");
        if let Some(pool) = storage::pool() {
            // 生成消息哈希用于去重
            let hash_input = format!(
                "{}:{}:{}",
                mldsa_pubkey_hex,
                msgtype as u8,
                hex::encode(data)
            );
            let message_hash = {
                let mut hasher = Sha256::new();
                hasher.update(hash_input.as_bytes());
                hex::encode(hasher.finalize())
            };

            // 将消息内容转为字符串（文本消息直接存，其他类型存 hex）
            let content = match msgtype {
                ChatMessageType::Text => String::from_utf8_lossy(data).to_string(),
                _ => format!("[{}] {}", msgtype as u8, hex::encode(data)),
            };

            match storage::add_message_with_hash(
                pool,
                owner_identity_id,
                mldsa_pubkey_hex,
                &content,
                true, // is_outgoing
                true, // pending
                &message_hash,
            )
            .await
            {
                Ok(Some(id)) => {
                    tracing::info!("消息已保存到离线队列，id={}", id);
                    let msg = format!(
                        "消息已保存到离线队列（联系人 {} 当前不在线）",
                        &mldsa_pubkey_hex[..16]
                    );
                    self.send_log_mpsc(msg).await;
                }
                Ok(None) => {
                    tracing::debug!("离线队列中已存在相同消息，跳过");
                }
                Err(e) => {
                    tracing::warn!("保存离线消息失败: {}", e);
                }
            }
        }
    }

    /// 重试发送所有待发送消息
    pub(crate) async fn retry_pending_messages(&mut self) {
        let pool = match storage::pool() {
            Some(p) => p,
            None => {
                tracing::warn!("数据库不可用，无法重试待发送消息");
                return;
            }
        };

        let pending_msgs = match storage::list_pending(pool).await {
            Ok(msgs) => msgs,
            Err(e) => {
                tracing::warn!("查询待发送消息列表失败: {}", e);
                return;
            }
        };

        if pending_msgs.is_empty() {
            tracing::debug!("没有待发送的消息");
            return;
        }

        tracing::info!("开始重试 {} 条待发送消息", pending_msgs.len());

        for msg in &pending_msgs {
            // 根据消息内容检测类型
            // 注意：chat_core 不依赖 serde_json，使用简单字符串匹配判断
            // file_hash 消息格式为 {"type":"file_hash","file_hash":"...","filename":"...",...}
            let msgtype = if msg.content.contains(r#""type":"file_hash""#)
                || msg.content.contains(r#""file_hash":"#)
            {
                ChatMessageType::FileHash
            } else {
                ChatMessageType::Text
            };

            match self
                .send_text(
                    &msg.peer_pubkey_hex,
                    msgtype,
                    msg.content.as_bytes().to_vec(),
                )
                .await
            {
                Ok(_) => {
                    tracing::info!("离线消息 {} 发送成功", msg.id);
                    if let Err(e) = storage::mark_sent(pool, msg.id).await {
                        tracing::warn!("标记消息 {} 为已发送失败: {}", msg.id, e);
                    }
                }
                Err(e) => {
                    tracing::warn!("离线消息 {} 发送失败: {}", msg.id, e);
                }
            }
        }

        tracing::info!("离线消息重试完成");
    }

    async fn add_contact(
        &mut self,
        mldsa_pubkey_hex: String,
        mlkem_public_key: Vec<u8>,
        name: Option<String>,
    ) -> bool {
        // 注意：mldsa_pubkey_hex 是联系人的 ML-DSA 公钥 hex，作为联系人唯一标识
        // mlkem_public_key 是联系人的临时 ML-KEM 公钥（用于端到端加密，每次会话可能变化）

        // 验证身份绑定：检查 DHT 中是否存在该 ML-DSA 身份的绑定记录
        if !mlkem_public_key.is_empty() {
            let mlkem_pubkey_hex = hex::encode(&mlkem_public_key);
            match p2p::verify_identity_binding(
                &self.data_dir,
                &mldsa_pubkey_hex,
                None, // PeerID 可选，暂不验证
                Some(&mlkem_pubkey_hex),
            ) {
                Ok(true) => {
                    tracing::info!(
                        "Identity binding verified for contact {} (ML-KEM matches DHT)",
                        &mldsa_pubkey_hex[..16]
                    );
                }
                Ok(false) => {
                    tracing::warn!(
                        "Identity binding verification failed for contact {}: ML-KEM mismatch or not found in DHT",
                        &mldsa_pubkey_hex[..16]
                    );
                    // 仍然允许添加联系人，但记录警告
                    // 用户可能通过带外交互（如二维码）交换了公钥
                    let msg = format!(
                        "警告：无法验证联系人 {} 的身份绑定（DHT 中未找到对应的 ML-KEM 公钥记录），请确认公钥来源可靠",
                        &mldsa_pubkey_hex[..16]
                    );
                    self.send_warning_mpsc(msg).await;
                }
                Err(e) => {
                    tracing::warn!(
                        "Identity binding check error for contact {}: {}",
                        &mldsa_pubkey_hex[..16],
                        e
                    );
                }
            }
        }

        // 获取当前身份的 identity_id
        let owner_identity_id = self.mldsa_identity_id.as_deref().unwrap_or("");

        // 保存到数据库
        if let Some(pool) = storage::pool() {
            match storage::upsert_contact(
                pool,
                owner_identity_id,
                &mldsa_pubkey_hex,
                name.as_deref(),
                Some(&mlkem_public_key),
            )
            .await
            {
                Ok(_) => {
                    tracing::info!("Successfully added contact: {}", mldsa_pubkey_hex);
                    let msg = format!("好友 {} 添加成功", &mldsa_pubkey_hex[..16]);
                    self.send_log_mpsc(msg).await;
                    true
                }
                Err(e) => {
                    tracing::error!("Failed to save contact: {e}");
                    let msg = format!("保存好友信息失败: {}", e);
                    self.send_warning_mpsc(msg).await;
                    false
                }
            }
        } else {
            tracing::error!("Database pool not available");
            self.send_warning_mpsc("数据库不可用".to_string()).await;
            false
        }
    }

    /// 通过 DHT 网络发现联系人
    ///
    /// 通过 Kademlia get_record 查询联系人的 PeerID 和 ML-KEM 公钥，
    /// 如果找到则自动添加到联系人列表。
    async fn discover_contact(&mut self, mldsa_pubkey_hex: &str, name: Option<String>) {
        tracing::info!(
            "Discovering contact {} via DHT network",
            &mldsa_pubkey_hex[..16]
        );

        // 1. 先尝试查询 PeerID
        let peer_id_result = match p2p::lookup_peerid_by_pubkey_network(
            &mut self.swarm,
            &self.data_dir,
            mldsa_pubkey_hex,
        )
        .await
        {
            Ok(Some(peer_id)) => {
                tracing::info!(
                    "DHT discovery: found PeerID {} for contact {}",
                    peer_id,
                    &mldsa_pubkey_hex[..16]
                );
                Some(peer_id)
            }
            Ok(None) => {
                tracing::warn!(
                    "DHT discovery: no PeerID found for contact {}",
                    &mldsa_pubkey_hex[..16]
                );
                None
            }
            Err(e) => {
                tracing::warn!(
                    "DHT discovery: error querying PeerID for contact {}: {}",
                    &mldsa_pubkey_hex[..16],
                    e
                );
                None
            }
        };

        // 获取当前身份的 identity_id
        let owner_identity_id = self.mldsa_identity_id.as_deref().unwrap_or("");

        // 2. 查询 ML-KEM 公钥（通过 Kademlia get_record 查询 "mlkem:{pubkey}" 记录）
        let mlkem_pubkey_result = if let Some(pool) = storage::pool() {
            // 先查本地 contacts 表
            match storage::get_contact_mlkem_pubkey(pool, owner_identity_id, mldsa_pubkey_hex).await
            {
                Ok(Some(pk)) => {
                    tracing::debug!(
                        "DHT discovery: found ML-KEM pubkey for {} in local contacts DB",
                        &mldsa_pubkey_hex[..16]
                    );
                    Some(pk)
                }
                _ => {
                    // 本地没有，尝试从 DHT 网络查询
                    self.query_mlkem_from_dht_network(mldsa_pubkey_hex).await
                }
            }
        } else {
            self.query_mlkem_from_dht_network(mldsa_pubkey_hex).await
        };

        // 3. 如果找到了 PeerID 或 ML-KEM 公钥，添加联系人
        if peer_id_result.is_some() || mlkem_pubkey_result.is_some() {
            let mlkem_key = mlkem_pubkey_result.unwrap_or_default();
            self.add_contact(mldsa_pubkey_hex.to_string(), mlkem_key, name)
                .await;

            let msg = format!("已通过 DHT 发现并添加联系人: {}..", &mldsa_pubkey_hex[..16]);
            self.send_log_mpsc(msg).await;
        } else {
            let msg = format!(
                "DHT 发现失败：未找到联系人 {}.. 的网络记录，请确认对方已在线并发布了身份",
                &mldsa_pubkey_hex[..16]
            );
            self.send_warning_mpsc(msg).await;
        }
    }
}
