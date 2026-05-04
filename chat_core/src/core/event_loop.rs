use std::sync::Arc;

use futures::StreamExt;
use redb::Database;
use tokio::sync::mpsc;

use crate::{command::ChatCommand, core::ChatCore, p2p};

impl ChatCore {
    /// 启动核心事件循环（使用独立的 Tokio 运行时）
    pub fn run(mut self) -> std::thread::JoinHandle<()> {
        let worker_threads = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1);

        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(worker_threads)
            .enable_all()
            .build()
            .expect("Failed to build tokio runtime");

        let handle = rt.handle().clone();
        std::thread::spawn(move || {
            rt.block_on(async move {
                self.run_inner(&handle).await;
            });
        })
    }

    /// 启动核心事件循环（使用调用方提供的 Tokio 运行时句柄）
    pub fn run_on_runtime(
        mut self,
        rt_handle: tokio::runtime::Handle,
    ) -> std::thread::JoinHandle<()> {
        let handle = rt_handle.clone();
        let handle_for_block = handle.clone();
        let handle_for_inner = handle.clone();
        std::thread::spawn(move || {
            let _guard = handle.enter();
            handle_for_block.block_on(async move {
                self.run_inner(&handle_for_inner).await;
            });
        })
    }

    /// 内部事件循环：DHT 注册 + 主循环（run / run_on_runtime 共享）
    async fn run_inner(&mut self, rt_handle: &tokio::runtime::Handle) {
        // 启动 DHT 定期注册任务
        let dht_reg_cmd_tx = self.core_handle.cmd_tx.clone();
        self.spawn_dht_registration(rt_handle);

        // 主事件循环：处理网络事件和控制命令
        loop {
            tokio::select! {
                event = self.swarm.select_next_some() => {
                    p2p::swarm_event(event, self).await;
                }
                Some(cmd) = self.rx_cmd.recv() => {
                    if matches!(cmd, ChatCommand::Shutdown) {
                        tracing::info!("P2P core shutting down...");
                        break;
                    }
                    // 处理身份切换：更新 DHT 注册循环的身份信息
                    if matches!(cmd, ChatCommand::SelectIdentity { .. }) {
                        // 先执行身份切换
                        self.handle_command(cmd).await;
                        // 发送内部命令通知 DHT 注册循环更新身份
                        if let (Some(pubkey), Some(pid)) = (self.mldsa_pubkey_hex.clone(), self.current_peer_id) {
                            let _ = dht_reg_cmd_tx.try_send(ChatCommand::DhtPublishIdentity {
                                mldsa_pubkey_hex: pubkey.clone(),
                                peer_id: pid.to_string(),
                                mlkem_pubkey_hex: self.mlkem_pubkey_hex.clone().unwrap_or_default(),
                            });
                            // 同时更新本地 DHT 数据库（使用缓存的连接）
                            if let Ok(store) = self.get_dht_store() {
                                let _ = store.set_pubkey_peerid(&pubkey, &pid);
                                if let Some(ref mlkem) = self.mlkem_pubkey_hex {
                                    let _ = store.set_mlkem_pubkey(&pubkey, mlkem);
                                }
                            }
                        }
                    } else {
                        self.handle_command(cmd).await;
                    }
                }
                else => break, // swarm 或 rx_cmd 关闭时退出
            }
        }
    }

    /// 启动 DHT 定期注册后台任务
    fn spawn_dht_registration(&self, rt_handle: &tokio::runtime::Handle) {
        let cmd_tx = self.core_handle.cmd_tx.clone();
        // 使用 ChatCore 中已打开的 DHT 数据库连接，避免重复打开导致文件锁冲突
        let db = match self.dht_db.clone() {
            Some(db) => db,
            None => {
                tracing::error!("DHT database not initialized, DHT registration disabled");
                return;
            }
        };

        let handle = rt_handle.clone();
        tokio::task::spawn_blocking(move || {
            handle.block_on(async move {
                Self::dht_registration_loop(cmd_tx, db).await;
            });
        });
    }

    /// DHT 定期注册循环（每 5 分钟执行一次）
    ///
    /// 每次 tick 从 DHT 数据库读取最新的身份信息，确保身份切换后能立即使用新身份。
    /// 不再通过闭包捕获身份字段，避免 select_identity 后使用旧值。
    /// 使用传入的 `Arc<Database>` 而非重新打开文件，避免 `redb` 文件锁冲突。
    pub(crate) async fn dht_registration_loop(
        cmd_tx: mpsc::Sender<ChatCommand>,
        db: Arc<Database>,
    ) {
        use crate::core::DHT_REGISTRATION_INTERVAL_SECS;

        const CACHE_CLEANUP_INTERVAL: u64 = 6;
        let mut tick_count = 0u64;
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(
            DHT_REGISTRATION_INTERVAL_SECS,
        ));

        loop {
            interval.tick().await;
            tick_count += 1;

            let store = p2p::dht::RedbRecordStore::new(db.clone());

            // 从 DHT 数据库读取最新的身份信息（select_identity 会更新这些记录）
            // 这样身份切换后，注册循环自动使用新身份
            let pubkeys = match store.get_all_pubkeys() {
                Ok(keys) => keys,
                Err(e) => {
                    tracing::warn!("Failed to read pubkeys from DHT database: {}", e);
                    continue;
                }
            };

            for pubkey in &pubkeys {
                // 1. 读取 PeerID 映射（get_peerid_by_pubkey 直接返回 PeerId）
                let pid = match store.get_peerid_by_pubkey(pubkey) {
                    Ok(Some(pid)) => pid,
                    _ => continue,
                };

                // 2. 读取 ML-KEM 公钥
                let mlkem_hex = match store.get_mlkem_pubkey(pubkey) {
                    Ok(Some(mlkem)) => Some(mlkem),
                    _ => None,
                };

                // 3. 注册 ML-DSA pubkey -> PeerID 映射
                if let Err(e) = store.set_pubkey_peerid(pubkey, &pid) {
                    tracing::warn!("Failed to refresh DHT registration: {}", e);
                }

                // 4. 发布 ML-KEM 公钥
                if let Some(ref mlkem) = mlkem_hex
                    && let Err(e) = store.set_mlkem_pubkey(pubkey, mlkem)
                {
                    tracing::warn!("Failed to publish ML-KEM pubkey: {}", e);
                }

                // 5. 通过命令通道请求发布到 Kademlia 网络
                if let Err(e) = cmd_tx.try_send(ChatCommand::DhtPublishIdentity {
                    mldsa_pubkey_hex: pubkey.clone(),
                    peer_id: pid.to_string(),
                    mlkem_pubkey_hex: mlkem_hex.unwrap_or_default(),
                }) {
                    tracing::warn!("Failed to send DHT publish command: {:?}", e);
                }

                // 6. 定期清理缓存
                if tick_count.is_multiple_of(CACHE_CLEANUP_INTERVAL) {
                    tracing::info!("Clearing stale DHT local cache (tick={})", tick_count);
                    let _ = store.clear_expired_pubkey_peerid_cache(pubkey);
                    let _ = store.clear_expired_mlkem_pubkey_cache(pubkey);
                }
            }
        }
    }
}
