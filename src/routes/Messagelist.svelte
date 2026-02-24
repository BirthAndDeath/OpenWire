<script lang="ts">
    import { VList } from "virtua/svelte";
    import { listen } from "@tauri-apps/api/event";
    import { onMount, tick } from "svelte";

    // === 类型定义 ===
    interface Message {
        id: string;
        content: string;
        timestamp: number;
        isLocal: boolean;
    }

    // === 状态管理 ===
    let messages = $state<Message[]>([]);
    let vlistRef: any = $state(undefined);
    let isLoadingHistory = $state(false);
    let hasMoreHistory = $state(true);
    let oldestTs = $state<number | null>(null);

    // === 消息操作 ===
    export function add(content: string, isLocal = false) {
        if (!content.trim()) return;
        messages = [
            ...messages,
            {
                id: `${Date.now()}-${Math.random().toString(36).slice(2, 9)}`,
                content: content.trim(),
                timestamp: Date.now(),
                isLocal,
            },
        ];
        tick().then(() => scrollToBottom());
    }
    add("欢迎来到聊天室", true);
    export function remove(id: string) {
        messages = messages.filter((m) => m.id !== id);
    }

    function scrollToBottom() {
        vlistRef?.scrollToIndex(messages.length - 1, {
            smooth: true,
            align: "end",
        });
    }

    // === 历史加载 ===
    async function loadHistory() {
        if (isLoadingHistory || !hasMoreHistory) return;
        isLoadingHistory = true;
        // await invoke("load_history", { before: oldestTs, limit: 30 });
        isLoadingHistory = false;
    }

    // === 事件监听 ===
    onMount(() => {
        let unlisten: (() => void) | undefined;
        (async () => {
            unlisten = await listen<string>("chat-message", (e) =>
                add(e.payload, false),
            );
        })();
        return () => unlisten?.();
    });
</script>

<div class="chat">
    {#if isLoadingHistory}
        <div class="loading">加载中...</div>
    {/if}

    <VList
        bind:this={vlistRef}
        data={messages}
        getKey={(m: Message) => m.id}
        class="list"
    >
        {#snippet children(msg)}
            <div class="msg" class:me={msg.isLocal}>
                <div class="bubble">
                    <p>{msg.content}</p>
                    <time>{new Date(msg.timestamp).toLocaleTimeString()}</time>
                </div>
                <button class="del" onclick={() => remove(msg.id)}>×</button>
            </div>
        {/snippet}
    </VList>
</div>

<style>
    :global(.chat) {
        display: flex;
        flex-direction: column;
        width: 100%;
        height: 100%;
        background: #0f0f0f;
        position: relative;
    }

    :global(.virtua-scroll-view) {
        flex: 1;
        padding: 16px;
    }

    .loading {
        position: absolute;
        top: 0;
        left: 0;
        right: 0;
        text-align: center;
        padding: 8px;
        background: rgba(0, 0, 0, 0.8);
        color: #666;
        font-size: 12px;
        z-index: 10;
    }

    .msg {
        display: flex;
        margin-bottom: 12px;
        align-items: flex-start;
        gap: 8px;
    }
    .msg.me {
        flex-direction: row-reverse;
    }

    .bubble {
        max-width: 70%;
        padding: 12px 16px;
        border-radius: 16px;
        background: #1a1a1a;
        color: #fafafa;
    }
    .msg.me .bubble {
        background: #3b82f6;
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

    .del {
        width: 20px;
        height: 20px;
        border-radius: 50%;
        border: none;
        background: transparent;
        color: #666;
        cursor: pointer;
        opacity: 0;
        transition: all 0.2s;
        display: flex;
        align-items: center;
        justify-content: center;
    }
    .msg:hover .del {
        opacity: 1;
    }
    .del:hover {
        background: rgba(239, 68, 68, 0.2);
        color: #ef4444;
    }
</style>
