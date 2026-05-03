<script lang="ts">
    import { VList } from "virtua/svelte";
    import { tick } from "svelte";
    import { invoke } from "@tauri-apps/api/core";

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

    // 当 contactId 变化时，加载历史消息
    $effect(() => {
        const cid = contactId;
        if (!cid) {
            msgs = [];
            loadedMsgIds = new Set();
            return;
        }
        loadHistory(cid);
    });

    // 从数据库加载历史消息
    async function loadHistory(peerId: string) {
        try {
            const history: {
                id: number;
                mldsa_pubkey_hex: string;
                content: string;
                is_outgoing: boolean;
                ts: number;
            }[] = await invoke("load_messages", {
                mldsaPubkeyHex: peerId,
                before: null,
                limit: 50,
            });
            // 反转顺序（数据库按时间倒序，前端需要正序）
            const loaded: Msg[] = history.reverse().map((m) => {
                // 尝试解析 file_hash 消息
                let type: Msg["type"] = "text";
                let file_hash_info: Msg["file_hash_info"] = undefined;
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
                };
            });
            const ids = new Set(loaded.map((m) => m.id));
            msgs = loaded;
            loadedMsgIds = ids;
            tick().then(() =>
                vlist?.scrollToIndex(msgs.length - 1, { smooth: false }),
            );
        } catch (e) {
            console.error("加载历史消息失败:", e);
        }
    }

    export function add(
        text: string,
        me = false,
        type: "text" | "file_hash" | "file_stream" = "text",
        file_hash_info?: Msg["file_hash_info"],
        sender_mldsa_pubkey_hex?: string,
        mldsa_pubkey_hex?: string,
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
        };
        // 去重：如果消息 ID 已存在则跳过
        if (loadedMsgIds.has(newMsg.id)) return;
        msgs.push(newMsg);
        loadedMsgIds.add(newMsg.id);
        tick().then(() =>
            vlist?.scrollToIndex(msgs.length - 1, { smooth: true }),
        );
    }

    export function del(id: string) {
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
            return; // 已经在下载中，忽略重复点击
        }

        try {
            await invoke("request_file_download", {
                senderMldsaPubkeyHex: msg.sender_mldsa_pubkey_hex,
                fileIdHex: info.file_id,
                downloadDir: null, // 使用默认下载目录
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

<VList bind:this={vlist} data={msgs} getKey={(m) => m.id} class="list">
    {#snippet children(m)}
        <div class="msg" class:me={m.me}>
            <div class="bubble" class:file-hash={m.type === "file_hash"}>
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
                                width="32"
                                height="32"
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
                                <line x1="9" y1="15" x2="12" y2="12" />
                                <line x1="15" y1="15" x2="12" y2="12" />
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
                            <span class="file-hash-label"
                                >文件分享 - 点击下载</span
                            >
                        </div>
                        <!-- 下载进度条 -->
                        {#if getFileProgress(m)}
                            {@const progress = getFileProgress(m)!}
                            <div class="download-progress">
                                <div class="progress-bar">
                                    <div
                                        class="progress-fill"
                                        class:completed={progress.status ===
                                            "completed"}
                                        style="width: {calcProgress(progress)}%"
                                    ></div>
                                </div>
                                <span class="progress-text">
                                    {#if progress.status === "downloading"}
                                        下载中 {calcProgress(progress)}% ({formatFileSize(
                                            progress.received_bytes,
                                        )}/{formatFileSize(
                                            progress.total_size,
                                        )})
                                    {:else if progress.status === "completed"}
                                        下载完成 ✓
                                    {:else}
                                        下载失败 ✗
                                    {/if}
                                </span>
                            </div>
                        {/if}
                    </div>
                {:else if m.type === "file_stream"}
                    <!-- FileStream 消息：显示文件传输中 -->
                    <div class="file-stream-content">
                        <div class="file-icon">
                            <svg
                                width="32"
                                height="32"
                                viewBox="0 0 24 24"
                                fill="none"
                                stroke="currentColor"
                                stroke-width="2"
                            >
                                <path
                                    d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z"
                                />
                                <polyline points="14 2 14 8 20 8" />
                                <line x1="16" y1="13" x2="8" y2="13" />
                                <line x1="16" y1="17" x2="8" y2="17" />
                            </svg>
                        </div>
                        <div class="file-info">
                            <span class="file-name">文件传输中...</span>
                            <span class="file-hash-label">正在接收文件分片</span
                            >
                        </div>
                    </div>
                {:else}
                    <!-- 普通文本消息 -->
                    <p>{m.content}</p>
                {/if}
                <time>{new Date(m.ts).toLocaleTimeString()}</time>
            </div>
            <button class="x" onclick={() => del(m.id)}>×</button>
        </div>
    {/snippet}
</VList>

<style>
    :global(.virtua-scroll-view) {
        padding: 16px;
        background: transparent;
        backdrop-filter: blur(10px);
        height: 100%;
    }

    .msg {
        display: flex;
        gap: 8px;
        margin-bottom: 12px;
        align-items: flex-start;
    }
    .msg.me {
        flex-direction: row-reverse;
    }

    .bubble {
        max-width: 70%;
        padding: 12px 16px;
        border-radius: 16px;
        background: var(--bg-tertiary, #1a1a1a);
        color: var(--text-primary, #fafafa);
    }
    .me .bubble {
        background: #3b82f6;
    }

    /* FileHash 气泡样式 */
    .bubble.file-hash {
        background: var(--bg-tertiary, #1a1a1a);
        border: 1px solid var(--border-color, #3b82f6);
        cursor: pointer;
        padding: 8px;
    }
    .me .bubble.file-hash {
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
        font-size: 14px;
        line-height: 1.5;
        word-break: break-word;
    }
    .bubble time {
        display: block;
        font-size: 11px;
        opacity: 0.6;
        margin-top: 4px;
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
    }
    .msg:hover .x {
        opacity: 1;
    }
    .x:hover {
        background: rgba(239, 68, 68, 0.2);
        color: #ef4444;
    }
</style>
