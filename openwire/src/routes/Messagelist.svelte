<script lang="ts">
    import { VList } from "virtua/svelte";
    import { tick } from "svelte";
    import { invoke } from "@tauri-apps/api/core";
    import { save } from "@tauri-apps/plugin-dialog";
    import { fontSizeScale } from "../lib/settings";

    let fontSize = $derived($fontSizeScale * 13 + "px");

    interface Msg {
        id: string;
        content: string;
        ts: number;
        me: boolean;
        // 消息类型: text | file_hash | file_stream
        type: "text" | "file_hash" | "file_stream";
        // FileHash 相关
        file_hash_info?: {
            filename: string;
            total_size: number;
            file_hash: string; // hex
            file_id: string; // hex
        };
        // 发送方 ML-DSA 公钥 hex（用于点击 FileHash 时发起下载请求）
        sender_mldsa_pubkey_hex?: string;
        // 消息所属联系人的 ML-DSA 公钥 hex
        mldsa_pubkey_hex: string;
        // 消息发送状态: true = 待确认（未收到送达回执）, false = 已送达
        pending?: boolean;
        // 消息哈希（用于匹配送达回执）
        message_hash?: string;
    }

    // 文件传输进度信息
    interface FileTransferProgress {
        filename: string;
        chunk_index: number;
        total_chunks: number;
        received_bytes: number;
        total_size: number;
        status: "downloading" | "completed" | "failed";
    }

    let {
        contactId = null,
    }: {
        contactId: string | null;
    } = $props();

    let msgs = $state<Msg[]>([]);
    let vlist: any;
    // 文件传输进度映射：file_id_hex -> progress
    let fileProgress = $state<Record<string, FileTransferProgress>>({});
    // 已加载的消息 ID 集合，用于去重
    let loadedMsgIds = $state<Set<string>>(new Set());

    // ========== 双向懒加载状态 ==========
    let loading = $state(false);
    let hasMoreOlder = $state(true); // 是否还有更早的历史消息可加载
    const PAGE_SIZE = 50;
    const LOAD_THRESHOLD = 200; // 滚动到距顶部多少 px 时触发加载
    // (ts, id) 游标边界，记录已加载消息的时间范围
    // ts 以秒为单位（与数据库格式一致）
    let oldestCursor = $state<{ ts: number; id: number } | null>(null);

    // 当 contactId 变化时，重新初始化
    $effect(() => {
        const cid = contactId;
        if (!cid) {
            msgs = [];
            loadedMsgIds = new Set();
            hasMoreOlder = true;
            oldestCursor = null;
            return;
        }
        loadLatest(cid);
    });

    // ---------- 消息加载 ----------

    // 将后端消息行转换为前端 Msg 格式
    function parseBackend(m: {
        id: number;
        mldsa_pubkey_hex: string;
        content: string;
        is_outgoing: boolean;
        ts: number;
        pending: number;
    }): Msg {
        let type: Msg["type"] = "text";
        let file_hash_info: Msg["file_hash_info"] = undefined;

        // 尝试解析新版 file_hash 格式: "[文件] filename [hash:hex64]"
        const FILE_SHARE_PREFIX = "[文件] ";
        const FILE_SHARE_HASH_PREFIX = " [hash:";
        if (m.content.startsWith(FILE_SHARE_PREFIX)) {
            const hashStart = m.content.indexOf(FILE_SHARE_HASH_PREFIX);
            if (hashStart !== -1) {
                const filename = m.content.substring(FILE_SHARE_PREFIX.length, hashStart);
                const hashEnd = m.content.indexOf("]", hashStart + FILE_SHARE_HASH_PREFIX.length);
                if (hashEnd !== -1) {
                    const fileHash = m.content.substring(
                        hashStart + FILE_SHARE_HASH_PREFIX.length,
                        hashEnd,
                    );
                    type = "file_hash";
                    file_hash_info = {
                        filename,
                        total_size: 0,
                        file_hash: fileHash,
                        file_id: fileHash,
                    };
                }
            }
        }

        // 尝试解析旧版 JSON 格式（兼容历史消息）
        if (type === "text") {
            try {
                const parsed = JSON.parse(m.content);
                if (parsed.file_hash && parsed.filename !== undefined) {
                    type = "file_hash";
                    file_hash_info = {
                        filename: parsed.filename,
                        total_size: parsed.total_size || 0,
                        file_hash: parsed.file_hash,
                        file_id: parsed.file_id || parsed.file_hash,
                    };
                }
            } catch {
                // 不是 JSON，保持 text 类型
            }
        }

        return {
            id: `hist-${m.id}`,
            content:
                type === "file_hash"
                    ? `[文件] ${file_hash_info!.filename} (${formatFileSize(file_hash_info!.total_size)})`
                    : m.content,
            ts: m.ts * 1000,
            me: m.is_outgoing,
            type,
            file_hash_info,
            mldsa_pubkey_hex: m.mldsa_pubkey_hex,
            pending: m.pending === 1 ? true : undefined,
        };
    }

    // 从后端提取数字 id（去掉 "hist-" 前缀）
    function extractNumericId(msgId: string): number {
        if (msgId.startsWith("hist-")) {
            return parseInt(msgId.slice(5), 10);
        }
        // 实时消息没有 "hist-" 前缀，返回 NaN
        return NaN;
    }

    // 初始加载：获取最新消息
    async function loadLatest(peerId: string) {
        loading = true;
        try {
            const raw: {
                id: number;
                mldsa_pubkey_hex: string;
                content: string;
                is_outgoing: boolean;
                ts: number;
                pending: number;
            }[] = await invoke("load_messages", {
                mldsaPubkeyHex: peerId,
                limit: PAGE_SIZE,
            });
            // 反转顺序（数据库按时间倒序，前端需要正序）
            const loaded = raw.reverse().map(parseBackend);
            const ids = new Set(loaded.map((m) => m.id));
            msgs = loaded;
            loadedMsgIds = ids;
            // 如果返回数量 < limit，说明没有更早的消息了
            hasMoreOlder = raw.length >= PAGE_SIZE;
            // 更新游标
            if (loaded.length > 0) {
                const id0 = extractNumericId(loaded[0].id);
                oldestCursor = {
                    ts: Math.floor(loaded[0].ts / 1000),
                    id: isNaN(id0) ? 0 : id0,
                };
            } else {
                oldestCursor = null;
                hasMoreOlder = false;
            }
            // 滚动到底部
            await tick();
            vlist?.scrollToIndex(msgs.length - 1, { smooth: false });
        } catch (e) {
            console.error("加载消息失败:", e);
        } finally {
            loading = false;
        }
    }

    // 加载更早的消息（上向翻页 —— 用户向上滚动触顶时调用）
    async function loadOlder() {
        if (
            loading ||
            !hasMoreOlder ||
            msgs.length === 0 ||
            !contactId ||
            !oldestCursor
        )
            return;
        loading = true;
        // 记录当前滚动偏移，用于 prepend 后恢复
        const scrollPos = vlist?.scrollTop ?? 0;
        try {
            const raw: {
                id: number;
                mldsa_pubkey_hex: string;
                content: string;
                is_outgoing: boolean;
                ts: number;
                pending: number;
            }[] = await invoke("load_messages", {
                mldsaPubkeyHex: contactId,
                before: oldestCursor.ts,
                beforeId: oldestCursor.id,
                limit: PAGE_SIZE,
            });
            if (raw.length < PAGE_SIZE) hasMoreOlder = false;
            if (raw.length === 0) return;
            // 解析、反转（DESC → ASC）、去重
            const parsed = raw.reverse().map(parseBackend);
            const newOnes = parsed.filter((m) => !loadedMsgIds.has(m.id));
            if (newOnes.length === 0) {
                hasMoreOlder = false; // 剩余的都是重复内容，说明已到尽头
                return;
            }
            // 更新游标（新加载消息中最旧的那条）
            const firstNew = newOnes[0];
            const firstNewId = extractNumericId(firstNew.id);
            oldestCursor = {
                ts: Math.floor(firstNew.ts / 1000),
                id: isNaN(firstNewId) ? 0 : firstNewId,
            };
            // Prepending
            const count = newOnes.length;
            msgs = [...newOnes, ...msgs];
            newOnes.forEach((m) => loadedMsgIds.add(m.id));
            await tick();
            // 滚动位置补偿：prepend 的内容将旧内容推下去了，
            // 需要向下滚动补偿新内容的总高度
            vlist?.scrollTo(scrollPos + count * 80);
        } catch (e) {
            console.error("加载更早消息失败:", e);
        } finally {
            loading = false;
        }
    }

    // ---------- Scroll 事件处理 ----------

    function handleScroll(offset: number) {
        if (!vlist || loading) return;
        // 这里的 offset 是 VList 传入的滚动位置，使用它判断是否触底/触顶。
        if (offset < LOAD_THRESHOLD && hasMoreOlder) {
            loadOlder();
        }
    }

    // ---------- 以下函数与改造前完全兼容 ----------

    export function add(
        text: string,
        me = false,
        type: "text" | "file_hash" | "file_stream" = "text",
        file_hash_info?: Msg["file_hash_info"],
        sender_mldsa_pubkey_hex?: string,
        mldsa_pubkey_hex?: string,
        pending?: boolean,
        message_hash?: string,
    ) {
        // 如果指定了 mldsa_pubkey_hex 且与当前选中的联系人不同，则忽略此消息
        if (mldsa_pubkey_hex && contactId && mldsa_pubkey_hex !== contactId) {
            return;
        }
        if (!text.trim() && type === "text") return;
        const newMsg: Msg = {
            id: `${Date.now()}-${Math.random().toString(36).slice(2, 7)}`,
            content: text.trim(),
            ts: Date.now(),
            me,
            type,
            file_hash_info,
            sender_mldsa_pubkey_hex,
            mldsa_pubkey_hex: mldsa_pubkey_hex || contactId || "",
            pending,
            message_hash,
        };
        // 去重：如果消息 ID 已存在则跳过
        if (loadedMsgIds.has(newMsg.id)) return;
        msgs.push(newMsg);
        loadedMsgIds.add(newMsg.id);
        tick().then(() =>
            vlist?.scrollToIndex(msgs.length - 1, { smooth: true }),
        );
    }

    // 更新消息发送状态（从 pending 变为 sent）
    export function markSent(messageHash: string) {
        // 通过消息哈希精确匹配对应的 pending 消息并标记为已送达
        // 优先按 message_hash 精确匹配
        for (let i = msgs.length - 1; i >= 0; i--) {
            const msg = msgs[i];
            if (msg.me && msg.message_hash === messageHash) {
                msg.pending = false;
                return;
            }
        }
        // 如果未找到精确匹配（例如历史消息没有存储 hash），
        // 则标记最近一条 pending 消息
        for (let i = msgs.length - 1; i >= 0; i--) {
            const msg = msgs[i];
            if (msg.me && msg.pending === true) {
                msg.pending = false;
                return;
            }
        }
    }

    // 更新最近一条 pending 消息的 message_hash 字段
    // 由后端发送 message-sent 事件时调用，用于后续送达回执精确匹配
    export function updateMessageHash(messageHash: string) {
        // 从后往前找最近一条没有 message_hash 的 pending 消息
        for (let i = msgs.length - 1; i >= 0; i--) {
            const msg = msgs[i];
            if (msg.me && msg.pending === true && !msg.message_hash) {
                msg.message_hash = messageHash;
                return;
            }
        }
    }

    export async function del(id: string) {
        // 如果是历史消息（id 格式为 "hist-{数字}"），调用后端删除
        if (id.startsWith("hist-")) {
            const msgId = parseInt(id.slice(5), 10);
            if (!isNaN(msgId)) {
                try {
                    await invoke("delete_message", { messageId: msgId });
                } catch (e) {
                    console.error("删除消息失败:", e);
                    return; // 后端删除失败，不更新 UI
                }
            }
        }
        // 前端 UI 移除
        msgs = msgs.filter((m) => m.id !== id);
        loadedMsgIds.delete(id);
    }

    // 更新文件传输进度
    export function updateFileProgress(progress: FileTransferProgress) {
        // 用 filename 作为 key 来跟踪进度
        const key = progress.filename;
        fileProgress = { ...fileProgress, [key]: progress };

        // 如果传输完成，3秒后移除进度信息
        if (progress.status === "completed") {
            setTimeout(() => {
                const newProgress = { ...fileProgress };
                delete newProgress[key];
                fileProgress = newProgress;
            }, 3000);
        }
    }

    // 格式化文件大小
    function formatFileSize(bytes: number): string {
        if (bytes === 0) return "0 B";
        const units = ["B", "KB", "MB", "GB"];
        const i = Math.floor(Math.log(bytes) / Math.log(1024));
        return (bytes / Math.pow(1024, i)).toFixed(1) + " " + units[i];
    }

    // 计算进度百分比
    function calcProgress(progress: FileTransferProgress): number {
        if (progress.total_size === 0) return 0;
        return Math.round(
            (progress.received_bytes / progress.total_size) * 100,
        );
    }

// 点击 FileHash 消息发起下载
    async function handleFileHashClick(msg: Msg) {
        if (!msg.file_hash_info || !msg.sender_mldsa_pubkey_hex) return;
        const info = msg.file_hash_info;

        // 检查是否已经在下载中
        if (fileProgress[info.filename]?.status === "downloading") {
            return;
        }

        // 弹出保存对话框让用户选择保存位置
        let savePath: string | null = null;
        try {
            savePath = await save({
                defaultPath: info.filename,
                title: "保存文件",
            });
        } catch (e) {
            console.error("打开保存对话框失败:", e);
            return;
        }
        if (!savePath) return; // 用户取消

        try {
            await invoke("request_file_download", {
                senderMldsaPubkeyHex: msg.sender_mldsa_pubkey_hex,
                fileHashHex: info.file_hash,
                savePath: savePath,
            });
        } catch (e) {
            console.error("请求文件下载失败:", e);
        }
    }

    // 获取文件传输进度（如果有）
    function getFileProgress(msg: Msg): FileTransferProgress | undefined {
        if (msg.file_hash_info) {
            return fileProgress[msg.file_hash_info.filename];
        }
        return undefined;
    }
</script>

<VList
    bind:this={vlist}
    data={msgs}
    getKey={(m: Msg) => m.id}
    class="list"
    onscroll={handleScroll}
>
    {#snippet children(m: Msg)}
        <div class="msg" class:me={m.me}>
            <div class="bubble" class:file-hash={m.type === "file_hash"} style="font-size: {fontSize}">
                {#if m.type === "file_hash" && m.file_hash_info}
                    <!-- FileHash 消息：可点击下载 -->
                    <div
                        class="file-hash-content"
                        onclick={() => handleFileHashClick(m)}
                        onkeydown={(e) =>
                            e.key === "Enter" && handleFileHashClick(m)}
                        role="button"
                        tabindex="0"
                        aria-label="点击下载文件"
                    >
                        <div class="file-icon">
                            <svg
                                width="24"
                                height="24"
                                viewBox="0 0 24 24"
                                fill="none"
                                stroke="currentColor"
                                stroke-width="2"
                            >
                                <path
                                    d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z"
                                />
                                <polyline points="14 2 14 8 20 8" />
                                <line x1="12" y1="18" x2="12" y2="12" />
                                <line x1="9" y1="15" x2="15" y2="15" />
                            </svg>
                        </div>
                        <div class="file-info">
                            <span class="file-name"
                                >{m.file_hash_info.filename}</span
                            >
                            <span class="file-size"
                                >{formatFileSize(
                                    m.file_hash_info.total_size,
                                )}</span
                            >
                            <span class="file-hash-label">点击下载文件</span>

                            {#if getFileProgress(m)}
                                {@const p = getFileProgress(m)!}
                                <div class="download-progress">
                                    <div class="progress-bar">
                                        <div
                                            class="progress-fill"
                                            class:completed={p.status ===
                                                "completed"}
                                            style="width: {calcProgress(p)}%"
                                        ></div>
                                    </div>
                                    <span class="progress-text">
                                        {#if p.status === "downloading"}
                                            下载中... {calcProgress(p)}%
                                        {:else if p.status === "completed"}
                                            下载完成
                                        {:else if p.status === "failed"}
                                            下载失败
                                        {/if}
                                    </span>
                                </div>
                            {/if}
                        </div>
                    </div>
                {:else if m.type === "file_stream"}
                    <!-- file_stream 类型消息（接收文件传输中） -->
                    <div class="file-stream-content">
                        <div class="file-icon">
                            <svg
                                width="24"
                                height="24"
                                viewBox="0 0 24 24"
                                fill="none"
                                stroke="currentColor"
                                stroke-width="2"
                            >
                                <path
                                    d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z"
                                />
                                <polyline points="14 2 14 8 20 8" />
                            </svg>
                        </div>
                        <span>{m.content}</span>
                    </div>
                {:else if m.type === "text"}
                    <p>{m.content}</p>
                {/if}
            </div>
            <div class="msg-meta">
                <time
                    >{new Date(m.ts).toLocaleTimeString([], {
                        hour: "2-digit",
                        minute: "2-digit",
                    })}</time
                >
                {#if m.pending === true}
                    <span class="pending-indicator">● 发送中</span>
                {:else if m.pending === false}
                    <span class="sent-indicator">✓</span>
                {/if}
            </div>
            <button class="x" onclick={() => del(m.id)} aria-label="删除消息"
                >×</button
            >
        </div>
    {/snippet}
</VList>

{#if loading}
    <div class="loading-hint">加载中...</div>
{/if}

<style>
    :global(.virtua-scroll-view) {
        padding: 16px;
        padding-top: 48px;
        background: transparent;
    }
    @container (max-width: 480px) {
        :global(.virtua-scroll-view) {
            padding: 8px;
            padding-top: 48px;
        }
        .msg {
            padding-right: 8px;
        }
        .msg.me {
            padding-left: 8px;
        }
        .bubble {
            max-width: 90%;
        }
        .msg-meta time {
            font-size: 10px;
        }
    }
    :global(.list) {
        height: 100%;
        display: flex;
        flex-direction: column;
        container-type: inline-size;
    }
    .msg {
        display: flex;
        flex-direction: column;
        align-items: flex-start;
        margin-bottom: var(--spacing-xs, 8px);
        position: relative;
        padding-right: 36px;
        gap: 2px;
    }
    .msg.me {
        align-items: flex-end;
        padding-right: 0;
        padding-left: 36px;
    }
    .bubble {
        max-width: var(--bubble-max, 70%);
        padding: var(--bubble-padding, 8px 12px);
        border-radius: var(--bubble-radius, 12px);
        background: var(--bg-secondary, #2a2a2a);
        word-break: break-word;
        white-space: pre-wrap;
    }
    @container (max-width: 400px) {
        .bubble {
            max-width: 85%;
        }
        .msg {
            margin-bottom: 4px;
        }
    }
    .msg.me .bubble {
        background: #3b82f6;
    }

    /* FileHash 气泡样式 */
    .bubble.file-hash {
        background: var(--bg-tertiary, #1a1a1a);
        border: 1px solid var(--border-color, #3b82f6);
        cursor: pointer;
        padding: 8px;
    }
    .msg.me .bubble.file-hash {
        background: #2563eb;
        border-color: #60a5fa;
    }

    .file-hash-content {
        display: flex;
        flex-direction: column;
        gap: 8px;
        user-select: none;
    }

    .file-hash-content:hover .file-name {
        color: #60a5fa;
    }

    .file-stream-content {
        display: flex;
        gap: 12px;
        align-items: center;
    }

    .file-icon {
        flex-shrink: 0;
        opacity: 0.8;
    }

    .file-info {
        display: flex;
        flex-direction: column;
        gap: 2px;
        min-width: 0;
    }

    .file-name {
        font-weight: 600;
        font-size: 14px;
        word-break: break-all;
        transition: color 0.2s;
    }

    .file-size {
        font-size: 12px;
        opacity: 0.7;
    }

    .file-hash-label {
        font-size: 11px;
        opacity: 0.6;
        font-style: italic;
    }

    /* 下载进度条 */
    .download-progress {
        display: flex;
        flex-direction: column;
        gap: 4px;
        margin-top: 4px;
    }

    .progress-bar {
        width: 100%;
        height: 6px;
        background: rgba(255, 255, 255, 0.15);
        border-radius: 3px;
        overflow: hidden;
    }

    .progress-fill {
        height: 100%;
        background: #3b82f6;
        border-radius: 3px;
        transition: width 0.3s ease;
    }

    .progress-fill.completed {
        background: #22c55e;
    }

    .progress-text {
        font-size: 11px;
        opacity: 0.7;
    }

    .bubble p {
        margin: 0;
        line-height: 1.5;
        word-break: break-word;
    }
    .msg-meta {
        display: flex;
        align-items: center;
        gap: 4px;
        margin-top: 4px;
    }
    .msg-meta time {
        font-size: 11px;
        opacity: 0.6;
    }
    .pending-indicator {
        font-size: 11px;
        opacity: 0.7;
        animation: pulse 1.5s ease-in-out infinite;
    }
    .sent-indicator {
        font-size: 11px;
        color: #22c55e;
        opacity: 0.8;
    }
    @keyframes pulse {
        0%,
        50% {
            opacity: 0.7;
        }
        50% {
            opacity: 0.3;
        }
    }

    .x {
        width: 20px;
        height: 20px;
        border-radius: 50%;
        border: none;
        background: transparent;
        color: #666;
        cursor: pointer;
        opacity: 0;
        position: absolute;
        right: 8px;
        top: 50%;
        transform: translateY(-50%);
    }
    .msg:hover .x {
        opacity: 1;
    }
    .x:hover {
        background: rgba(239, 68, 68, 0.2);
        color: #ef4444;
    }
    .msg.me .x {
        right: auto;
        left: 8px;
    }

    .loading-hint {
        text-align: center;
        font-size: 12px;
        color: #888;
        padding: 4px;
        position: absolute;
        top: 0;
        left: 50%;
        transform: translateX(-50%);
        pointer-events: none;
    }
</style>
