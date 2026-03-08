<script lang="ts">
    import { VList } from "virtua/svelte";
    import { tick } from "svelte";

    interface Msg {
        id: string;
        content: string;
        ts: number;
        me: boolean;
    }

    let msgs = $state<Msg[]>([]);
    let vlist: any;

    export function add(text: string, me = false) {
        if (!text.trim()) return;
        msgs = [
            ...msgs,
            {
                id: `${Date.now()}-${Math.random().toString(36).slice(2, 7)}`,
                content: text.trim(),
                ts: Date.now(),
                me,
            },
        ];
        tick().then(() =>
            vlist?.scrollToIndex(msgs.length - 1, { smooth: true }),
        );
    }

    add("欢迎来到聊天室", true);

    export function del(id: string) {
        msgs = msgs.filter((m) => m.id !== id);
    }
</script>

<VList bind:this={vlist} data={msgs} getKey={(m) => m.id} class="list">
    {#snippet children(m)}
        <div class="msg" class:me={m.me}>
            <div class="bubble">
                <p>{m.content}</p>
                <time>{new Date(m.ts).toLocaleTimeString()}</time>
            </div>
            <button class="x" onclick={() => del(m.id)}>×</button>
        </div>
    {/snippet}
</VList>

<style>
    :global(.virtua-scroll-view) {
        padding: 16px;
        background: #0f0f0f;
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
        background: #1a1a1a;
        color: #fafafa;
    }
    .me .bubble {
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
