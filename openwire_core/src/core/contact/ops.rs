use std::time::Instant;

use crate::actor::p2p::P2pCommand;
use crate::{ScanContactInfo, core::ChatCore, storage};

/// 发现冷却表上限（防御性，条目数=尝试发现的不重复公钥数）
const MAX_DISCOVERY_ENTRIES: usize = 10_000;

impl ChatCore {
    pub(crate) async fn add_contact(
        &mut self,
        mldsa_pubkey_hex: String,
        mlkem_public_key: Vec<u8>,
        name: Option<String>,
    ) -> bool {
        if !crate::signature::validate_mldsa_pubkey_hex(&mldsa_pubkey_hex) {
            tracing::warn!("拒绝添加联系人：ML-DSA 公钥密码学验证失败");
            self.send_warning_mpsc("ML-DSA 公钥无效，拒绝添加".to_string())
                .await;
            return false;
        }
        let owner_identity_id = self.mldsa_identity_id.as_deref().unwrap_or("");

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
                    tracing::info!("Successfully added contact: {}", &mldsa_pubkey_hex[..16.min(mldsa_pubkey_hex.len())]);
                    let msg = format!("好友 {} 添加成功", &mldsa_pubkey_hex[..16.min(mldsa_pubkey_hex.len())]);
                    self.send_log_mpsc(msg).await;
                    // 添加联系人后，立即发起发现（DHT + 中继，双路并行）
                    // 使用 SHA256 哈希隐藏原始公钥
                    let query_key = crate::actor::p2p::swarm_ops::dht_key(&mldsa_pubkey_hex);
                    // 预存 DHT 查询键 → 公钥映射，供 GetProvidersResult 反向查找
                    self.peer_cache
                        .cache_dht_key(query_key.clone(), mldsa_pubkey_hex.clone());
                    if let Err(e) = self.p2p_handle.tx.try_send(P2pCommand::GetProviders {
                        key: query_key,
                    }) {
                        tracing::warn!("Failed to send GetProviders after adding contact: {e:?}");
                    }
                    if let Err(e) = self.p2p_handle.tx.try_send(P2pCommand::DiscoverPeer {
                        mldsa_pubkey_hex: mldsa_pubkey_hex.clone(),
                    }) {
                        tracing::warn!("Failed to send DiscoverPeer after adding contact: {e:?}");
                    }
                    // 重新发布自身身份到 DHT，确保对方能通过 DHT 反向发现我方
                    let store = self.get_dht_store();
                    if let (Some(pubkey), Some(pid)) =
                        (&self.mldsa_pubkey_hex, &self.current_peer_id)
                    {
                        store.set_pubkey_peerid(pubkey, pid);
                        if let Err(e) = self.p2p_handle.tx.try_send(P2pCommand::PublishIdentity {
                            mldsa_pubkey_hex: pubkey.clone(),
                        }) {
                            tracing::warn!(
                                "Failed to send PublishIdentity after adding contact: {e:?}"
                            );
                        }
                    }
                    // 通知上层刷新联系人列表（覆盖所有 add_contact 调用路径）
                    let _ = self
                        .tx_message
                        .try_send(crate::command::MessageEvent::ContactsChanged);
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

    /// 记录每个联系人的最近发现时间，防止过频重复发现
    fn check_discovery_cooldown(&mut self, pubkey: &str) -> bool {
        let now = Instant::now();
        // 冷却与定时轮询周期（120s）对齐：避免每轮定时器触发时
        // 冷却早已失效导致 DHT 查询风暴（每联系人每 120s 至多 1 次）
        let cooldown = std::time::Duration::from_secs(120);
        if let Some(last) = self.last_discovery_time.get(pubkey)
            && now.duration_since(*last) < cooldown {
                tracing::debug!(
                    "Discover cooldown for {}.., skipping",
                    &pubkey[..16.min(pubkey.len())]
                );
                return false;
            }
        // 防御上限：条目数=尝试发现的去重公钥数，超限时淘汰最早的记录
        if !self.last_discovery_time.contains_key(pubkey)
            && self.last_discovery_time.len() >= MAX_DISCOVERY_ENTRIES
            && let Some(oldest) = self.last_discovery_time.iter().min_by_key(|(_, t)| **t).map(|(k, _)| k.clone())
        {
            self.last_discovery_time.remove(&oldest);
        }
        self.last_discovery_time.insert(pubkey.to_string(), now);
        true
    }

    /// 通过 DHT 网络发现联系人
    ///
    /// 发现策略（优先级顺序）：
    ///   1. 本地缓存 → 直接拨号
    ///   2. 已存地址 → 直接拨号（无需 DHT 网络查询）
    ///   3. DHT GetProviders → 网络查询
    ///   4. Relay DiscoverPeer → 中继查询
    ///
    /// 双路并行（DHT + 中继）确保最快发现路径。
    pub(crate) async fn discover_contact(&mut self, mldsa_pubkey_hex: &str, name: Option<String>) {
        let pubkey_short = &mldsa_pubkey_hex[..16.min(mldsa_pubkey_hex.len())];
        tracing::info!("Discovering contact {} via DHT network", pubkey_short);

        // 快速路径：本地已缓存该联系人 PeerID 且当前未连接 → 立即拨号，不占发现冷却。
        // 否则联系人掉线后重连要等 120s 冷却+定时器（最长数分钟），在线状态持续离线，
        // 只有互发消息（rr_msg 自动拨号）才能即时连上。
        // 多设备：主设备（pubkey_peerid）不可达时，对账号其余在线设备（FriendOnline
        // 已登记）一并拨号——消息 fan-out 能达但主动建连/在线恢复不能只依赖主设备。
        {
            let store = self.get_dht_store();
            let mut dialed = std::collections::HashSet::new();
            let mut try_dial = |pid: libp2p::PeerId, who: &str| {
                if self.connected_peers.contains_key(&pid) || !dialed.insert(pid) {
                    return;
                }
                tracing::info!(
                    "DHT discovery: 拨号 {} 的{}{}（不占冷却）",
                    pubkey_short,
                    who,
                    pid
                );
                if let Err(e) = self.p2p_handle.tx.try_send(P2pCommand::Dial { peer_id: pid }) {
                    tracing::warn!("Failed to send Dial for cached contact device: {e:?}");
                }
            };
            if let Some(peer_id) = store.get_peerid_by_pubkey(mldsa_pubkey_hex) {
                try_dial(peer_id, "主设备 ");
            }
            for pid in store.list_pubkey_devices(mldsa_pubkey_hex) {
                try_dial(pid, "设备 ");
            }
        }

        // 发现冷却：防止重复发现（仅约束后续 DHT/DiscoverPeer 网络查询）
        if !self.check_discovery_cooldown(mldsa_pubkey_hex) {
            return;
        }

        // 检查联系人是否已在数据库中
        let is_existing_contact = match self.mldsa_identity_id.as_ref() {
            Some(owner_id) => match storage::pool() {
                Some(pool) => storage::is_contact_exists(pool, owner_id, mldsa_pubkey_hex)
                    .await
                    .unwrap_or(false),
                None => false,
            },
            None => false,
        };

        let has_local_peerid = self
            .get_dht_store()
            .get_peerid_by_pubkey(mldsa_pubkey_hex)
            .is_some();

        // 本地已有 PeerID：拨号已在快速路径完成，这里仅处理联系人关系与 DHT 键刷新
        if has_local_peerid {
            if is_existing_contact {
                tracing::debug!(
                    "DHT discovery: contact {} already exists, refreshing DHT query",
                    pubkey_short
                );
                let query_key = crate::actor::p2p::swarm_ops::dht_key(mldsa_pubkey_hex);
                if let Err(e) = self.p2p_handle.tx.try_send(P2pCommand::GetProviders {
                    key: query_key,
                }) {
                    tracing::warn!("Failed to send GetProviders for DHT discovery: {e:?}");
                }
            } else {
                tracing::info!(
                    "DHT discovery: contact {} already cached locally, adding directly",
                    pubkey_short
                );
                self.add_contact(mldsa_pubkey_hex.to_string(), vec![], name)
                    .await;
                let msg = format!("已通过 DHT 发现并添加联系人: {}..", pubkey_short);
                self.send_log_mpsc(msg).await;
            }
            return;
        }

        // === 尝试从存储的多地址直接拨号（无需 DHT 网络查询）===
        // 如果之前成功连接过，存储中可能有对方的历史地址
        self.try_dial_from_stored_addrs(mldsa_pubkey_hex, pubkey_short).await;

        // === 双路并行发现：DHT + 中继 ===
        // 本地没有缓存，发起网络查询但不阻塞等待
        let query_key = crate::actor::p2p::swarm_ops::dht_key(mldsa_pubkey_hex);
        // 预存 DHT 查询键 → 公钥映射，供 GetProvidersResult 反向查找
        self.peer_cache.cache_dht_key(query_key.clone(), mldsa_pubkey_hex.to_string());
        if let Err(e) = self.p2p_handle.tx.try_send(P2pCommand::GetProviders {
            key: query_key,
        }) {
            tracing::warn!("Failed to send GetProviders for new contact: {e:?}");
        }
        if let Err(e) = self.p2p_handle.tx.try_send(P2pCommand::DiscoverPeer {
            mldsa_pubkey_hex: mldsa_pubkey_hex.to_string(),
        }) {
            tracing::warn!("Failed to send DiscoverPeer for new contact: {e:?}");
        }
        tracing::debug!(
            "DHT discovery: initiated GetProviders + DiscoverPeer + address dial for {} (non-blocking)",
            pubkey_short
        );

        tracing::info!(
            "DHT discovery: no cached data for contact {}, initiated network queries",
            pubkey_short
        );
    }

    /// 尝试从本地存储的 Multiaddr 直接拨号（无需 DHT 网络查询）
    async fn try_dial_from_stored_addrs(&self, mldsa_pubkey_hex: &str, pubkey_short: &str) {
        let store = self.get_dht_store();
        if let Some(peer_id) = store.get_peerid_by_pubkey(mldsa_pubkey_hex) {
            if self.connected_peers.contains_key(&peer_id) {
                return;
            }
            if let addrs = store.get_multiaddrs(&peer_id)
                && !addrs.is_empty() {
                    tracing::info!(
                        "尝试从本地存储的 {} 个地址拨号 {}..",
                        addrs.len(),
                        pubkey_short
                    );
                    for addr in addrs {
                        let _ = self.p2p_handle.tx.try_send(P2pCommand::DialAddr { addr });
                    }
                }
        }
    }

    /// 通过 SHA256 摘要添加好友（扫码路径）
    ///
    /// digest 与 DHT 查询键一致（`swarm_ops::dht_key`，即 SHA256(公钥 hex 字符串)），
    /// 不引入第二套摘要表示。本地已有匹配公钥时直接添加；否则暂存
    /// `pending_contacts` 并按二维码推荐信息拨号，等待对方 FriendOnline
    /// 携带完整公钥后由 event_loop 校验摘要匹配并补添加。
    pub(crate) async fn add_contact_by_digest(&mut self, info: &ScanContactInfo) -> bool {
        let digest_hex = &info.digest_hex;
        if digest_hex.len() != 64 || !digest_hex.chars().all(|c| c.is_ascii_hexdigit()) {
            self.send_warning_mpsc("二维码摘要格式无效（需 64 位 hex）".to_string())
                .await;
            return false;
        }
        let owner_identity_id = self.mldsa_identity_id.as_deref().unwrap_or("");
        if owner_identity_id.is_empty() {
            self.send_warning_mpsc("未选择身份，无法添加好友".to_string())
                .await;
            return false;
        }
        // 摘要与自身一致 → 拒绝（扫码到自己的二维码）
        if let Some(own) = self.mldsa_pubkey_hex.as_deref()
            && crate::actor::p2p::swarm_ops::dht_key(own) == *digest_hex
        {
            self.send_warning_mpsc("不能添加自己为好友".to_string()).await;
            return false;
        }
        let short = &digest_hex[..16];

        // 核心层复检数量上限（外部 QR 驱动，JS/Tauri 层的防线不可依赖）
        if info.addrs.len() > 16 || info.relay_addrs.len() > 8 {
            self.send_warning_mpsc("二维码推荐节点数量超限".to_string())
                .await;
            return false;
        }

        // 本地已有完整公钥且摘要匹配 → 直接添加
        if let Some(pubkey) = self.find_pubkey_by_digest(digest_hex, info.peer_id.as_deref()) {
            tracing::info!(
                "摘要 {}.. 匹配本地公钥 {}..，直接添加",
                short,
                &pubkey[..16.min(pubkey.len())]
            );
            let ok = self.add_contact(pubkey, Vec::new(), info.name.clone()).await;
            if ok {
                self.send_log_mpsc(format!("扫码添加成功：摘要 {}..", short))
                    .await;
            }
            return ok;
        }

        // 本地无完整公钥 → 暂存待匹配（跨重启保留），等待 FriendOnline 到达后补添加
        if let Some(pool) = storage::pool() {
            if let Err(e) = storage::add_pending_contact(pool, owner_identity_id, info).await {
                self.send_warning_mpsc(format!("保存扫码暂存失败: {e}")).await;
                return false;
            }
        } else {
            self.send_warning_mpsc("数据库不可用".to_string()).await;
            return false;
        }

        // 拨号：二维码推荐的自身地址与公共节点，加速发现
        // （peer_id 可能已轮换 / 直连地址 NAT 不可达，下面的按摘要查询是兜底）
        if let Some(pid_str) = info.peer_id.as_deref()
            && let Ok(pid) = pid_str.parse::<libp2p::PeerId>()
            && let Err(e) = self.p2p_handle.tx.try_send(P2pCommand::Dial { peer_id: pid })
        {
            tracing::warn!("Failed to send Dial for scanned peer: {e:?}");
        }
        for addr_str in &info.addrs {
            if let Ok(addr) = addr_str.parse::<libp2p::Multiaddr>() {
                let _ = self.p2p_handle.tx.try_send(P2pCommand::DialAddr { addr });
            }
        }
        for relay in &info.relay_addrs {
            if relay.len() != 2 {
                continue;
            }
            if let (Ok(pid), Ok(addr)) = (
                relay[0].parse::<libp2p::PeerId>(),
                relay[1].parse::<libp2p::Multiaddr>(),
            ) {
                let _ = self.p2p_handle.tx.try_send(P2pCommand::AddKademliaAddress {
                    peer_id: pid,
                    addr,
                });
            }
        }
        // 按摘要经中继反查对端当前 PeerID：仅持 SHA256 摘要即可把
        // "等对方上线" 从分钟级收敛到秒级（中继缓存来自对方 FriendOnline 注册）。
        // 无连接中继时由 P2pActor 暂存，中继接通后立即补发。
        let _ = self.p2p_handle.tx.try_send(P2pCommand::DiscoverPeer {
            mldsa_pubkey_hex: digest_hex.to_string(),
        });

        self.send_log_mpsc(format!(
            "扫码成功，已暂存 {}..，等待对方上线自动完成添加",
            short
        ))
        .await;
        true
    }

    /// 在本端缓存中按摘要查找完整公钥（均以 dht_key 重算比对，防缓存脏数据）
    fn find_pubkey_by_digest(&self, digest_hex: &str, peer_id: Option<&str>) -> Option<String> {
        // DHT 查询键反查（discover_contact 已缓存 key→pubkey）
        if let Some(pubkey) = self.peer_cache.dht_key_to_pubkey.get(digest_hex)
            && crate::actor::p2p::swarm_ops::dht_key(pubkey) == digest_hex
        {
            return Some(pubkey.clone());
        }
        // peerid_to_pubkey 内存缓存（FriendOnline/Identify 建立）
        for pubkey in self.peer_cache.peerid_to_pubkey.values() {
            if crate::actor::p2p::swarm_ops::dht_key(pubkey) == digest_hex {
                return Some(pubkey.clone());
            }
        }
        // 二维码携带 peer_id 时，反查该 peer 的已认证公钥
        if let Some(pid_str) = peer_id
            && let Ok(pid) = pid_str.parse::<libp2p::PeerId>()
            && let Some(pubkey) = self.get_dht_store().get_pubkey_by_peerid(&pid)
            && crate::actor::p2p::swarm_ops::dht_key(&pubkey) == digest_hex
        {
            return Some(pubkey);
        }
        None
    }

    /// 组装身份分享二维码载荷（v2 二进制，v1 JSON 解码兼容）。
    ///
    /// 二进制格式：字段名/hex/base58 文本化开销全部去除，载荷 ≈ 250B
    /// （v1 JSON 约 570B），二维码版本约降 2 级、码元更粗更易扫。
    /// 本机处于公网（AutoNAT Public）时直接推荐自身 peer_id 与外部地址，
    /// 加速对方直连发现；否则推荐公共 relay 节点。
    pub(crate) async fn build_identity_share_info(&mut self) -> Vec<u8> {
        let digest = self
            .mldsa_pubkey_hex
            .as_deref()
            .map(crate::actor::p2p::swarm_ops::dht_key)
            .unwrap_or_default();
        let peer_id = self
            .current_peer_id
            .map(|p| p.to_string())
            .unwrap_or_default();

        // 查询 NAT 状态与外部地址（复用 GetNetworkStatus 往返）
        let (p2p_tx, p2p_rx) = tokio::sync::oneshot::channel();
        let mut status_json = crate::command::NetworkStatusData::error_json(
            "p2p_no_response",
            "P2pActor 未响应网络状态查询",
        );
        if let Err(e) = self
            .p2p_handle
            .tx
            .try_send(P2pCommand::GetNetworkStatus { resp: p2p_tx })
        {
            tracing::warn!("build_identity_share_info: 查询网络状态失败: {e:?}");
        } else if let Ok(Ok(json)) =
            tokio::time::timeout(std::time::Duration::from_secs(5), p2p_rx).await
        {
            // 限时等待：避免 ChatCore 事件循环在生成二维码时被 P2pActor 无界阻塞
            status_json = json;
        }
        let status: crate::command::NetworkStatusData = serde_json::from_str(&status_json)
            .unwrap_or_else(|e| {
                tracing::warn!("解析网络状态 JSON 失败: {e}");
                // error_json 输出与本结构体 schema 一致（本 crate 内部不变量）
                serde_json::from_str(&crate::command::NetworkStatusData::error_json(
                    "parse_failed",
                    "网络状态解析失败",
                ))
                .expect("error_json 输出必须可反序列化为 NetworkStatusData")
            });

        let is_public = status.nat_status == "Public";
        // 推荐 addr：仅取前 4 条（公网/局域网混合，二维码载荷需克制——
        // 每条约 45 字符，16 条会让二维码版本升到 40 级、码元过细难扫；
        // 拨号其余地址由 mDNS/DHT/DiscoverPeer 兜底，不必全塞进二维码）。
        let addr: Vec<String> = status.recommend_addrs.iter().take(4).cloned().collect();
        // 推荐 relay：仅取前 2 个公共中继（二维码载荷克制；发现主要靠 DHT/mDNS）
        let relay: Vec<Vec<String>> = if is_public {
            Vec::new()
        } else {
            let config = crate::p2p::nodes::NodesConfig::load(&self.data_dir);
            config
                .relay_nodes
                .iter()
                .take(2)
                .map(|pair| pair.to_vec())
                .collect()
        };

        // === v2 二进制编码 ===
        // [0]=0x02, [1]=flags(bit0=has_peer_id)
        // [2..34]=digest(32 原始字节)
        // has_peer_id: [u16 BE len][peer_id.to_bytes()（36B，ed25519）]
        // [u8 addr_count] × [u8 len][multiaddr 文本]
        // [u8 relay_count] × [u16 BE plen][relay peer.to_bytes()][u16 BE alen][relay addr 文本]
        let mut out: Vec<u8> = Vec::with_capacity(64 + addr.len() * 48 + relay.len() * 96);
        out.push(0x02u8);
        // has_peer 必须以"peer_id 能否解析"为准置位，且与后续写入的字节一致：
        // 否则 flag 置位却不写 peer 字节，会让解码端从错误的偏移读取长度而错位。
        let peer_bytes = peer_id.parse::<libp2p::PeerId>().ok().map(|p| p.to_bytes());
        out.push(if peer_bytes.is_some() { 0x01 } else { 0x00 });
        let digest_bytes = hex::decode(&digest).unwrap_or_default();
        out.extend_from_slice(&digest_bytes[..digest_bytes.len().min(32)]);
        if let Some(b) = peer_bytes {
            out.extend_from_slice(&(b.len() as u16).to_be_bytes());
            out.extend_from_slice(&b);
        }
        out.push(addr.len() as u8);
        for a in &addr {
            let b = a.as_bytes();
            let n = b.len().min(255);
            out.push(n as u8);
            out.extend_from_slice(&b[..n]);
        }
        out.push(relay.len() as u8);
        for pair in &relay {
            if pair.len() == 2
                && let Ok(pid) = pair[0].parse::<libp2p::PeerId>()
            {
                let pb = pid.to_bytes();
                out.extend_from_slice(&(pb.len() as u16).to_be_bytes());
                out.extend_from_slice(&pb);
            } else {
                out.extend_from_slice(&0u16.to_be_bytes());
            }
            let ab = pair[1].as_bytes();
            out.extend_from_slice(&(ab.len() as u16).to_be_bytes());
            out.extend_from_slice(ab);
        }
        out
    }

    /// 解码 v2 二进制二维码载荷（与 build_identity_share_info 编码对称）。
    pub(crate) fn decode_identity_qr_v2(
        bytes: &[u8],
    ) -> Option<crate::command::ScanContactInfo> {
        // 校验：长度 ≥ 34（版本+flags+digest），版本标记为 0x02
        if bytes.len() < 34 || bytes[0] != 0x02 {
            return None;
        }
        let mut off = 2usize;
        let flags = bytes[1];
        let digest_hex = hex::encode(&bytes[off..off + 32]);
        off += 32;
        let mut peer_id = None;
        if flags & 0x01 != 0 {
            if off + 2 > bytes.len() {
                return None;
            }
            let plen = u16::from_be_bytes([bytes[off], bytes[off + 1]]) as usize;
            off += 2;
            if off + plen > bytes.len() {
                return None;
            }
            if let Ok(pid) = libp2p::PeerId::from_bytes(&bytes[off..off + plen]) {
                peer_id = Some(pid.to_string());
            }
            off += plen;
        }
        if off >= bytes.len() {
            return None;
        }
        let addr_count = bytes[off] as usize;
        off += 1;
        let mut addrs = Vec::new();
        for _ in 0..addr_count {
            if off >= bytes.len() {
                return None;
            }
            let n = bytes[off] as usize;
            off += 1;
            if off + n > bytes.len() {
                return None;
            }
            addrs.push(String::from_utf8_lossy(&bytes[off..off + n]).to_string());
            off += n;
        }
        if off >= bytes.len() {
            return None;
        }
        let relay_count = bytes[off] as usize;
        off += 1;
        let mut relays = Vec::new();
        for _ in 0..relay_count {
            if off + 2 > bytes.len() {
                return None;
            }
            let plen = u16::from_be_bytes([bytes[off], bytes[off + 1]]) as usize;
            off += 2;
            if off + plen > bytes.len() {
                return None;
            }
            let pid_str = if plen > 0 {
                libp2p::PeerId::from_bytes(&bytes[off..off + plen])
                    .map(|p| p.to_string())
                    .unwrap_or_default()
            } else {
                String::new()
            };
            off += plen;
            if off + 2 > bytes.len() {
                return None;
            }
            let alen = u16::from_be_bytes([bytes[off], bytes[off + 1]]) as usize;
            off += 2;
            if off + alen > bytes.len() {
                return None;
            }
            let addr = String::from_utf8_lossy(&bytes[off..off + alen]).to_string();
            off += alen;
            relays.push(vec![pid_str, addr]);
        }
        Some(crate::command::ScanContactInfo {
            digest_hex,
            peer_id,
            addrs,
            relay_addrs: relays,
            name: None,
            version: 2,
        })
    }
}