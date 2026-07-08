use crate::actor::p2p::P2pCommand;
use sha2::{Digest, Sha256};

use crate::{core::ChatCore, crypto, error::CoreError, message::ChatMessageType, storage};

impl ChatCore {
    /// send_text 的公开入口
    ///
    /// 返回发送的消息的 hash（hex 编码），供调用方（如 command_handler）通知前端
    pub(crate) async fn send_text(
        &mut self,
        mldsa_pubkey_hex: &str,
        msgtype: ChatMessageType,
        data: Vec<u8>,
    ) -> Result<String, CoreError> {
        self.send_text_impl(mldsa_pubkey_hex, msgtype, data, false)
            .await
    }

    /// send_text 内部实现，is_retry=true 时跳过 save_pending_message 避免重复
    ///
    /// 返回发送的消息的 hash（hex 编码），供调用方（如 retry_pending_messages）更新数据库记录
    pub(crate) async fn send_text_impl(
        &mut self,
        mldsa_pubkey_hex: &str,
        msgtype: ChatMessageType,
        data: Vec<u8>,
        is_retry: bool,
    ) -> Result<String, CoreError> {
        let pubkey_short = &mldsa_pubkey_hex[..16];
        tracing::debug!(
            "send_text_impl: is_retry={}, msgtype={:?}, data_len={}",
            is_retry,
            msgtype,
            data.len(),
        );

        // 通过 DHT 查找接收方当前的 PeerID（临时传输层标识）
        let recipient_peer_id = {
            let store = self.get_dht_store();
            match store.get_peerid_by_pubkey(mldsa_pubkey_hex) {
                Ok(Some(peer_id)) => {
                    tracing::debug!(
                        "Found PeerID {} for {} in local DHT database",
                        peer_id,
                        pubkey_short
                    );
                    peer_id
                }
                Ok(None) => {
                    // 本地 DHT 未找到 PeerID 时，先通过已建立的连接或本地数据库查找
                    tracing::info!(
                        "PeerID not found locally for {}, performing DHT lookup...",
                        pubkey_short
                    );

                    // dht_lookup_peerid 是纯本地查询（检查 connected_peers + 本地数据库），
                    // 不阻塞事件循环。如果本地未找到，会发起非阻塞 GetProviders 网络查询，
                    // 查询结果通过 events.rs 自动缓存到本地数据库并触发重试。
                    match self.dht_lookup_peerid(mldsa_pubkey_hex).await {
                        Some(peer_id) => {
                            tracing::info!(
                                "通过已建立连接找到 {} 的 PeerID: {}",
                                pubkey_short,
                                peer_id
                            );
                            // 将找到的 PeerID 缓存到本地 DHT 数据库，供后续使用
                            let store = self.get_dht_store();
                            let _ = store.set_pubkey_peerid(mldsa_pubkey_hex, &peer_id);
                            peer_id
                        }
                        None => {
                            tracing::info!("未找到 {} 的 PeerID，保存到离线队列", pubkey_short);
                            if !is_retry {
                                self.save_pending_message(mldsa_pubkey_hex, msgtype, &data)
                                    .await;
                            }
                            // 发起非阻塞 DHT 发现（通过命令队列，不阻塞事件循环）
                            let _ = self.core_handle.cmd_tx.try_send(
                                crate::command::ChatCommand::DiscoverContact {
                                    mldsa_pubkey_hex: mldsa_pubkey_hex.to_string(),
                                    name: None,
                                },
                            );
                            return Err(CoreError::ContactOffline(format!(
                                "联系人 {} 当前不在线（本地未缓存 PeerID），消息已保存到离线队列，后台正在尝试 DHT 发现",
                                pubkey_short
                            )));
                        }
                    }
                }
                Err(e) => return Err(CoreError::DhtError(e)),
            }
        };

        // 获取接收方的 ML-KEM 公钥
        let recipient_mlkem_public_key = self
            .lookup_mlkem_pubkey(mldsa_pubkey_hex, pubkey_short, msgtype, &data, is_retry)
            .await?;

        // 加密消息数据
        let encrypted_data = crypto::encrypt_message(&data, &recipient_mlkem_public_key)?;
        tracing::debug!(
            "send_text: data_len={}, mlkem_pubkey_len={}, encrypted_len={}, encrypted_preview={}",
            data.len(),
            recipient_mlkem_public_key.len(),
            encrypted_data.len(),
            hex::encode(&encrypted_data[..std::cmp::min(16, encrypted_data.len())]),
        );

        // 构建签名的消息
        let message = self.build_signed_message(msgtype, encrypted_data).await?;
        tracing::debug!("send_text: message.data.len()={}", message.data.len(),);

        // 使用 ChatMessage 自身的 hash 字段作为消息哈希
        let message_hash = hex::encode(&message.hash);
        tracing::debug!(
            "send_text_impl: message_hash={}.., msgtype={:?}, is_retry={}",
            &message_hash[..16],
            msgtype,
            is_retry,
        );

        // 保存到数据库作为 pending（重试时消息已在 pending 队列中，跳过保存）
        if !is_retry {
            tracing::debug!(
                "send_text_impl: 调用 save_pending_message_with_hash, msgtype={:?}, hash={}..",
                msgtype,
                &message_hash[..16]
            );
            self.save_pending_message_with_hash(mldsa_pubkey_hex, msgtype, &data, &message_hash)
                .await;
        } else {
            tracing::debug!("send_text_impl: is_retry=true, 跳过 save_pending_message_with_hash");
        }

        // 发送消息到网络
        self.send_message(recipient_peer_id, message).await;

        // 首次发送成功后立即标记为已发送，防止重制定时器重复发送消息
        // 重试路径由 retry_single_pending_message 中的 mark_sent 按 ID 标记
        if !is_retry {
            if let Some(pool) = storage::pool() {
                let _ = storage::mark_sent_by_hash(pool, &message_hash).await;
            }
        }

        Ok(message_hash)
    }

    /// 查找接收方的 ML-KEM 公钥
    ///
    /// 查询链：DHT 本地数据库 → contacts 表 → 保存到离线队列并发起 DHT 网络查询（非阻塞）
    ///
    /// # 设计说明
    /// DHT 本地数据库的优先级高于 contacts 表，因为：
    /// - 接收方每次 select_identity（切换身份）都会生成新的 ML-KEM 密钥对，
    ///   并立即更新 DHT 本地数据库中的 ML-KEM 公钥。
    /// - contacts 表中的 ML-KEM 公钥仅在添加联系人时写入一次，不会自动更新。
    /// - 如果优先查询 contacts 表，发送方会一直使用旧的 ML-KEM 公钥加密，
    ///   导致接收方解密失败（AES-GCM 解密失败: aead::Error）。
    ///
    /// 当 is_retry=true 时，如果 ML-KEM 公钥未找到，不会调用 save_pending_message
    /// 创建新的 pending 记录（因为消息已经在 pending 队列中），避免数据库重复记录。
    async fn lookup_mlkem_pubkey(
        &mut self,
        mldsa_pubkey_hex: &str,
        pubkey_short: &str,
        msgtype: ChatMessageType,
        data: &[u8],
        is_retry: bool,
    ) -> Result<Vec<u8>, CoreError> {
        let owner_identity_id = self.mldsa_identity_id.as_deref().unwrap_or("");
        tracing::debug!(
            "lookup_mlkem_pubkey: 开始查询, is_retry={}, msgtype={:?}, data_len={}",
            is_retry,
            msgtype,
            data.len()
        );

        // 步骤 1：从 DHT 本地数据库查询（优先级最高）
        // DHT 本地数据库中的 ML-KEM 公钥在 select_identity 时实时更新，
        // 而 contacts 表中的公钥仅在添加联系人时写入一次，不会自动更新。
        let store = self.get_dht_store();
        match store.get_mlkem_pubkey(mldsa_pubkey_hex) {
            Ok(Some(mlkem_hex)) if !mlkem_hex.is_empty() => {
                tracing::info!("Found ML-KEM pubkey for {} via DHT local DB", pubkey_short);
                // 后台异步发起 DHT 网络查询以验证公钥是否最新
                let mlkem_key = format!("mlkem:{}", mldsa_pubkey_hex);
                let _ = self
                    .p2p_handle
                    .tx
                    .try_send(crate::actor::ActorCommand::Custom(P2pCommand::GetRecord {
                        key: mlkem_key,
                    }));
                return hex::decode(&mlkem_hex).map_err(CoreError::InvalidMlKemFormat);
            }
            _ => {
                tracing::debug!(
                    "ML-KEM pubkey not found in DHT local DB for {}, trying contacts DB",
                    pubkey_short
                );
            }
        }

        // 步骤 2：从 contacts 表查询（兜底）
        if let Some(pool) = storage::pool() {
            match storage::get_contact_mlkem_pubkey(pool, owner_identity_id, mldsa_pubkey_hex).await
            {
                Ok(Some(pubkey)) if !pubkey.is_empty() => {
                    tracing::debug!(
                        "Found ML-KEM pubkey for {} in contacts DB ({} bytes)",
                        pubkey_short,
                        pubkey.len()
                    );
                    return Ok(pubkey);
                }
                Ok(Some(_)) => {
                    tracing::debug!("ML-KEM pubkey in contacts DB is empty for {}", pubkey_short);
                }
                Ok(None) => {
                    tracing::info!(
                        "ML-KEM pubkey not found in contacts DB for {}",
                        pubkey_short
                    );
                }
                Err(e) => {
                    return Err(CoreError::MlKemQueryFailed(format!(
                        "查询联系人 ML-KEM 公钥失败: {}",
                        e
                    )));
                }
            }
        } else {
            return Err(CoreError::DatabaseNotAvailable);
        }

        // 步骤 3：本地没有，保存到离线队列并发起 DHT 网络查询（非阻塞）
        tracing::debug!(
            "lookup_mlkem_pubkey: 步骤3 - 本地无 ML-KEM 公钥, is_retry={}, 将调用 save_pending_message",
            is_retry
        );
        if !is_retry {
            self.save_pending_message(mldsa_pubkey_hex, msgtype, data)
                .await;
        } else {
            tracing::debug!("lookup_mlkem_pubkey: is_retry=true, 跳过 save_pending_message");
        }
        let mlkem_key = format!("mlkem:{}", mldsa_pubkey_hex);
        let _ = self
            .p2p_handle
            .send(crate::actor::ActorCommand::Custom(P2pCommand::GetRecord {
                key: mlkem_key,
            }))
            .await;
        Err(CoreError::MlKemKeyNotCached(format!(
            "联系人 {} 的 ML-KEM 公钥未缓存，消息已保存到离线队列，后台正在通过 DHT 网络查询",
            pubkey_short
        )))
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

            let content = match msgtype {
                ChatMessageType::Text => String::from_utf8_lossy(data).to_string(),
                _ => format!("[{}] {}", msgtype as u8, hex::encode(data)),
            };

            tracing::debug!(
                "save_pending_message: msgtype={:?}, hash={}.., data_len={}",
                msgtype,
                &message_hash[..16],
                data.len(),
            );

            match storage::add_message_with_hash(
                pool,
                owner_identity_id,
                mldsa_pubkey_hex,
                &content,
                true,
                true,
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

    /// 使用指定的消息哈希保存待发送消息到数据库
    async fn save_pending_message_with_hash(
        &mut self,
        mldsa_pubkey_hex: &str,
        msgtype: ChatMessageType,
        data: &[u8],
        message_hash: &str,
    ) {
        let owner_identity_id = self.mldsa_identity_id.as_deref().unwrap_or("");
        if let Some(pool) = storage::pool() {
            let content = match msgtype {
                ChatMessageType::Text => String::from_utf8_lossy(data).to_string(),
                _ => format!("[{}] {}", msgtype as u8, hex::encode(data)),
            };

            tracing::debug!(
                "save_pending_message_with_hash: msgtype={:?}, hash={}.., data_len={}",
                msgtype,
                &message_hash[..16],
                data.len(),
            );

            match storage::add_message_with_hash(
                pool,
                owner_identity_id,
                mldsa_pubkey_hex,
                &content,
                true,
                true,
                message_hash,
            )
            .await
            {
                Ok(Some(id)) => {
                    tracing::info!("消息已保存到数据库（pending），id={}", id);
                }
                Ok(None) => {
                    tracing::debug!("数据库中已存在相同哈希的消息，跳过");
                }
                Err(e) => {
                    tracing::warn!("保存消息到数据库失败: {}", e);
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
            if let Err(e) = self.retry_single_pending_message(pool, msg).await {
                tracing::warn!("重试消息 {} 失败: {}", msg.id, e);
            }
        }

        tracing::info!("离线消息重试完成");
    }

    async fn retry_single_pending_message(
        &mut self,
        pool: &sqlx::Pool<sqlx::sqlite::Sqlite>,
        msg: &storage::Message,
    ) -> Result<(), CoreError> {
        let msgtype = detect_msgtype(&msg.content);

        let original_data = if msgtype == ChatMessageType::Text {
            msg.content.as_bytes().to_vec()
        } else {
            match msg.content.find(']') {
                Some(pos) => {
                    let hex_part = msg.content[pos + 1..].trim();
                    match hex::decode(hex_part) {
                        Ok(decoded) => decoded,
                        Err(_) => msg.content.as_bytes().to_vec(),
                    }
                }
                None => msg.content.as_bytes().to_vec(),
            }
        };

        let has_local_peerid = self
            .get_dht_store()
            .get_peerid_by_pubkey(&msg.peer_pubkey_hex)
            .ok()
            .flatten()
            .is_some();

        if !has_local_peerid {
            return Err(CoreError::ContactOffline(format!(
                "联系人 {} 当前不在线（本地未缓存 PeerID），不重试",
                &msg.peer_pubkey_hex[..16]
            )));
        }

        let message_hash = self
            .send_text_impl(&msg.peer_pubkey_hex, msgtype, original_data, true)
            .await?;

        tracing::info!(
            "离线消息 {} 发送成功, hash={}..",
            msg.id,
            &message_hash[..16]
        );
        storage::update_message_hash(pool, msg.id, &message_hash).await?;
        storage::mark_sent(pool, msg.id).await?;

        Ok(())
    }

    /// 重试指定联系人的待发送消息
    /// 用于 FriendOnline 事件，只重试该联系人的消息，避免重复发送
    pub(crate) async fn retry_pending_for_peer(&mut self, peer_pubkey_hex: &str) {
        let pool = match storage::pool() {
            Some(p) => p,
            None => {
                tracing::warn!("数据库不可用，无法重试待发送消息");
                return;
            }
        };

        let pending_msgs = match storage::list_pending_by_peer(pool, peer_pubkey_hex).await {
            Ok(msgs) => msgs,
            Err(e) => {
                tracing::warn!("查询待发送消息列表失败: {}", e);
                return;
            }
        };

        if pending_msgs.is_empty() {
            tracing::debug!("联系人 {}.. 没有待发送的消息", &peer_pubkey_hex[..16]);
            return;
        }

        tracing::info!(
            "开始重试联系人 {}.. 的 {} 条待发送消息",
            &peer_pubkey_hex[..16],
            pending_msgs.len()
        );

        for msg in &pending_msgs {
            if let Err(e) = self.retry_single_pending_message(pool, msg).await {
                tracing::warn!("重试给 {}.. 的消息 {} 失败: {}", &peer_pubkey_hex[..16], msg.id, e);
            }
        }
    }

    /// 仅向当前在线的好友重试待发送消息
    ///
    /// 与 retry_pending_messages() 的区别：
    /// - 只处理接收方 PeerID 在 connected_peers 中（当前在线）的消息
    /// - 对不在线的联系人跳过，不发起 DHT 查询（避免无效网络开销）
    /// - 由主循环中 1 秒间隔的定时器触发，替代 ConnectionEstablished 过早的重试
    ///
    /// # 设计说明
    /// 当对方上线后，FriendOnline 通知会缓存 (公钥→PeerID) 映射到 DHT 本地数据库，
    /// 但 FriendOnline 可能延迟或丢失。此定时器确保：
    ///   1. FriendOnline 到达后最多 1 秒内重试成功
    ///   2. FriendOnline 丢失时不会永久等待，下一次定时器触发即可重试
    ///   3. 只触达在线节点，不会对离线联系人做无效的 DHT 网络查询
    pub(crate) async fn retry_pending_for_online_peers(&mut self) {
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
            return;
        }

        let online_pending: Vec<_> = pending_msgs
            .into_iter()
            .filter(|msg| {
                let store = self.get_dht_store();
                match store.get_peerid_by_pubkey(&msg.peer_pubkey_hex) {
                    Ok(Some(peer_id)) => {
                        let online = self.connected_peers.contains_key(&peer_id);
                        if !online {
                            tracing::debug!(
                                "待发消息 {} 的接收方 {}.. 不在线，跳过",
                                msg.id,
                                &msg.peer_pubkey_hex[..16]
                            );
                        }
                        online
                    }
                    _ => {
                        tracing::debug!(
                            "待发消息 {} 的接收方 {}.. PeerID 未缓存，跳过",
                            msg.id,
                            &msg.peer_pubkey_hex[..16]
                        );
                        false
                    }
                }
            })
            .collect();

        if online_pending.is_empty() {
            tracing::debug!("没有面向在线好友的待发送消息");
            return;
        }

        tracing::info!(
            "定时重试: 向 {} 位在线好友重试 {} 条待发送消息",
            online_pending.len(),
            online_pending.len()
        );

        for msg in &online_pending {
            if let Err(e) = self.retry_single_pending_message(pool, msg).await {
                tracing::warn!("定时重试: 消息 {} 失败: {}", msg.id, e);
            }
        }
    }

    /// 通过 DHT 网络查询或已建立的连接查找对方的 PeerID
    ///
    /// 查找顺序：
    ///   1. 先检查已建立的连接（connected_peers），通过 DHT 本地数据库反向查找
    ///      每个已连接 PeerID 对应的 ML-DSA 公钥，看是否匹配目标公钥
    ///   2. 如果未找到，发起 DHT 网络查询获取对方的 PeerID 绑定记录
    ///
    /// 当本地 DHT 数据库中没有对方的 PeerID 缓存时使用此方法。
    ///
    /// 注意：此方法在事件循环的 cmd 分支中调用，因此需要主动处理 swarm 事件
    /// 来驱动 DHT 查询完成。使用 tokio::select! 同时处理 swarm 事件和超时。
    /// 查找 ML-DSA 公钥对应的 PeerID（仅本地查询，不阻塞事件循环）
    ///
    /// 查询顺序：
    /// 1. 遍历 connected_peers，反向查找已连接 PeerID 对应的 ML-DSA 公钥
    /// 2. 查询本地 DHT 数据库
    ///
    /// 如果本地未找到，发起 Kademlia GetProviders 网络查询（非阻塞），
    /// 查询结果会通过 events.rs 中的事件处理自动缓存到本地数据库，
    /// 并触发 retry_pending_messages 重试。
    ///
    /// 此函数不会 await 网络查询结果，确保不阻塞事件循环。
    pub(crate) async fn dht_lookup_peerid(&mut self, mldsa_pubkey_hex: &str) -> Option<libp2p::PeerId> {
        // === 步骤 1：检查内存缓存 peerid_to_pubkey ===
        // gossipsub 在线状态通知和 identify 协议会更新此缓存，比 DHT 数据库更快
        for (peer_id, pubkey_hex) in &self.peerid_to_pubkey {
            if pubkey_hex == mldsa_pubkey_hex {
                tracing::info!(
                    "dht_lookup_peerid: 通过内存缓存找到 {}.. -> PeerID={}",
                    &mldsa_pubkey_hex[..16],
                    peer_id
                );
                return Some(*peer_id);
            }
        }

        // === 步骤 2：检查已建立的连接 ===
        // 遍历 connected_peers，通过 DHT 本地数据库反向查找每个已连接 PeerID
        // 对应的 ML-DSA 公钥，看是否匹配目标公钥
        let store = self.get_dht_store();
        let connected: Vec<libp2p::PeerId> = self.connected_peers.keys().copied().collect();
        for peer_id in &connected {
            // 反向查找：检查这个已连接的 PeerID 是否对应目标 ML-DSA 公钥
            match store.get_pubkey_by_peerid(peer_id) {
                Ok(Some(pubkey_hex)) if pubkey_hex == mldsa_pubkey_hex => {
                    tracing::info!(
                        "dht_lookup_peerid: 通过 connected_peers 找到 {}.. -> PeerID={}",
                        &mldsa_pubkey_hex[..16],
                        peer_id
                    );
                    return Some(*peer_id);
                }
                _ => continue,
            }
        }

        // === 步骤 3：查询本地 DHT 数据库 ===
        match store.get_peerid_by_pubkey(mldsa_pubkey_hex) {
            Ok(Some(peer_id)) => {
                tracing::info!(
                    "dht_lookup_peerid: 本地数据库找到 {}.. -> PeerID={}",
                    &mldsa_pubkey_hex[..16],
                    peer_id
                );
                return Some(peer_id);
            }
            _ => {}
        }

        // === 步骤 4：本地未找到，发起 Kademlia GetProviders 网络查询（非阻塞） ===
        // 查询结果会通过 P2pEvent::GetProvidersResult 事件处理
        // 自动缓存到本地数据库，并触发 retry_pending_messages 重试待发送消息
        let _ = self
            .p2p_handle
            .send(crate::actor::ActorCommand::Custom(
                P2pCommand::GetProviders {
                    key: mldsa_pubkey_hex.to_string(),
                },
            ))
            .await;

        tracing::debug!(
            "dht_lookup_peerid: 本地未找到 {}..，已发起非阻塞 GetProviders 查询",
            &mldsa_pubkey_hex[..16]
        );

        None
    }
}

/// 根据消息内容检测消息类型
fn detect_msgtype(content: &str) -> ChatMessageType {
    if let Some(stripped) = content.strip_prefix('[') {
        if let Some(rest) = stripped.split(']').next() {
            match rest.parse::<u8>() {
                Ok(n) if n == ChatMessageType::FileHash as u8 => ChatMessageType::FileHash,
                Ok(n) if n == ChatMessageType::FileDownloadRequest as u8 => {
                    ChatMessageType::FileDownloadRequest
                }
                Ok(n) if n == ChatMessageType::FileStream as u8 => ChatMessageType::FileStream,
                _ => ChatMessageType::Text,
            }
        } else {
            ChatMessageType::Text
        }
    } else {
        ChatMessageType::Text
    }
}
