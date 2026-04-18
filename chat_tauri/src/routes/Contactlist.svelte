<script lang="ts">
    import { VList } from "virtua/svelte";

    interface Contact {
        order: number;
        peerid: string;
        name: string;
        avatar?: string;
        lastMsg?: string;
        lastTime?: number;
        unread: number;
        online?: boolean;
    }

    let {
        contacts = [],
        selectedId = null,
        onselect,
        onctx,
    }: {
        contacts: Contact[];
        selectedId: string | null;
        onselect?: (id: string) => void;
        onctx?: (e: MouseEvent, id: string) => void;
    } = $props();

    let q = $state("");
    let list = $state<any>(undefined);

    let filtered = $derived(
        contacts
            .filter(
                (c) =>
                    c.name.toLowerCase().includes(q.toLowerCase()) ||
                    c.lastMsg?.toLowerCase().includes(q.toLowerCase()),
            )
            .sort((a, b) => a.order - b.order),
    );

    const select = (id: string) => onselect?.(id);

    const fmtTime = (ts?: number) => {
        if (!ts) return "";
        const d = new Date(ts),
            now = new Date();
        return d.toDateString() === now.toDateString()
            ? d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })
            : d.toLocaleDateString([], { month: "short", day: "numeric" });
    };
</script>

<div class="list">
    <div class="search">
        <svg
            width="16"
            height="16"
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            stroke-width="2"
        >
            <circle cx="11" cy="11" r="8" /><path d="m21 21-4.35-4.35" />
        </svg>
        <input placeholder="搜索..." bind:value={q} />
        {#if q}<button class="clear" onclick={() => (q = "")}>×</button>{/if}
    </div>

    <div class="items">
        {#if filtered.length === 0}
            <div class="empty">
                <svg
                    width="48"
                    height="48"
                    viewBox="0 0 24 24"
                    fill="none"
                    stroke="currentColor"
                    stroke-width="1.5"
                >
                    <path
                        d="M17 21v-2a4 4 0 0 0-4-4H5a4 4 0 0 0-4 4v2"
                    /><circle cx="9" cy="7" r="4" />
                    <path d="M23 21v-2a4 4 0 0 0-3-3.87" /><path
                        d="M16 3.13a4 4 0 0 1 0 7.75"
                    />
                </svg>
                <p>{q ? "无结果" : "暂无联系人"}</p>
            </div>
        {:else}
            <VList bind:this={list} data={filtered} getKey={(c) => c.peerid}>
                {#snippet children(c)}
                    <button
                        type="button"
                        class="item"
                        class:sel={selectedId === c.peerid}
                        class:online={c.online}
                        onclick={() => select(c.peerid)}
                        oncontextmenu={(e) => (
                            e.preventDefault(), onctx?.(e, c.peerid)
                        )}
                        aria-label={`选择联系人 ${c.name}`}
                    >
                        <div class="avatar">
                            {#if c.avatar}<img
                                    src={c.avatar}
                                    alt={c.name}
                                />{:else}
                                <div class="fallback">
                                    {c.name.slice(0, 2).toUpperCase()}
                                </div>
                            {/if}
                            {#if c.online}<span class="dot"></span>{/if}
                        </div>

                        <div class="info">
                            <div class="row">
                                <span class="name">{c.name}</span>
                                {#if c.lastTime}<span class="time"
                                        >{fmtTime(c.lastTime)}</span
                                    >{/if}
                            </div>
                            {#if c.lastMsg}<p
                                    class="msg"
                                    class:unread={c.unread > 0}
                                >
                                    {c.lastMsg}
                                </p>{/if}
                        </div>

                        {#if c.unread > 0}<span class="badge"
                                >{c.unread > 99 ? "99+" : c.unread}</span
                            >{/if}
                    </button>
                {/snippet}
            </VList>
        {/if}
    </div>

    <div class="foot">{filtered.length} 位联系人</div>
</div>

<style>
    .list {
        display: flex;
        flex-direction: column;
        height: 100%;
        background: var(--bg-primary, #0f0f0f);
    }

    .search {
        display: flex;
        align-items: center;
        gap: 10px;
        padding: 12px 16px;
        border-bottom: 1px solid var(--border-color, #2a2a2a);
    }
    .search svg {
        color: var(--text-secondary, #525252);
    }
    .search input {
        flex: 1;
        background: transparent;
        border: none;
        color: var(--text-primary, #fafafa);
        font-size: 14px;
        outline: none;
    }
    .search input::placeholder {
        color: var(--text-secondary, #525252);
    }
    .clear {
        width: 18px;
        height: 18px;
        display: grid;
        place-items: center;
        background: #333;
        border: none;
        border-radius: 50%;
        color: #888;
        font-size: 12px;
        cursor: pointer;
    }
    .clear:hover {
        background: #444;
    }

    .items {
        flex: 1;
        overflow: hidden;
    }
    :global(.virtua-scroll-view) {
        padding: 8px;
    }

    .empty {
        display: grid;
        place-items: center;
        height: 100%;
        color: #525252;
        gap: 12px;
    }
    .empty p {
        font-size: 14px;
    }

    .item {
        display: flex;
        align-items: center;
        gap: 12px;
        padding: 12px;
        border-radius: 10px;
        cursor: pointer;
        transition: all 0.15s;
        background: transparent;
        border: none;
        width: 100%;
        text-align: left;
    }
    .item:hover {
        background: var(--bg-tertiary, #1a1a1a);
    }
    .item.sel {
        background: #1e3a5f;
    }
    .item.sel .name {
        color: #3b82f6;
    }

    .avatar {
        position: relative;
        flex-shrink: 0;
    }
    .avatar img,
    .fallback {
        width: 44px;
        height: 44px;
        border-radius: 50%;
        object-fit: cover;
    }
    .fallback {
        display: grid;
        place-items: center;
        background: linear-gradient(135deg, #3b82f6, #2563eb);
        color: white;
        font-size: 14px;
        font-weight: 600;
    }
    .dot {
        position: absolute;
        bottom: 2px;
        right: 2px;
        width: 12px;
        height: 12px;
        background: #22c55e;
        border: 2px solid #0f0f0f;
        border-radius: 50%;
    }

    .info {
        flex: 1;
        min-width: 0;
    }
    .row {
        display: flex;
        justify-content: space-between;
        align-items: center;
        margin-bottom: 4px;
    }
    .name {
        font-size: 15px;
        font-weight: 500;
        color: var(--text-primary, #fafafa);
        white-space: nowrap;
        overflow: hidden;
        text-overflow: ellipsis;
    }
    .time {
        font-size: 11px;
        color: var(--text-secondary, #525252);
    }
    .msg {
        font-size: 13px;
        color: var(--text-secondary, #737373);
        white-space: nowrap;
        overflow: hidden;
        text-overflow: ellipsis;
        margin: 0;
    }
    .msg.unread {
        color: var(--text-primary, #fafafa);
        font-weight: 500;
    }

    .badge {
        min-width: 20px;
        height: 20px;
        padding: 0 6px;
        display: grid;
        place-items: center;
        background: #ef4444;
        color: white;
        font-size: 12px;
        font-weight: 600;
        border-radius: 10px;
    }

    .foot {
        padding: 12px 16px;
        border-top: 1px solid var(--border-color, #2a2a2a);
        font-size: 12px;
        color: var(--text-secondary, #525252);
    }
</style>
