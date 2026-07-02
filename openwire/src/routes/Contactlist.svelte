<script lang="ts">
    import { VList } from "virtua/svelte";
    import { invoke } from "@tauri-apps/api/core";

    interface Contact {
        order: number;
        pubkey_hex: string;
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
        ondelete,
    }: {
        contacts: Contact[];
        selectedId: string | null;
        onselect?: (id: string) => void;
        onctx?: (e: MouseEvent, id: string) => void;
        ondelete?: (id: string) => void;
    } = $props();

    let q = $state("");
    let list = $state<any>(undefined);

    // 右键菜单状态
    let contextMenu = $state<{
        x: number;
        y: number;
        contactId: string;
    } | null>(null);

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

    // 右键点击处理
    const handleContextMenu = (e: MouseEvent, id: string) => {
        e.preventDefault();
        contextMenu = { x: e.clientX, y: e.clientY, contactId: id };
        onctx?.(e, id);
    };

    // 关闭右键菜单
    const closeContextMenu = () => {
        contextMenu = null;
    };

    // 删除联系人
    const handleDeleteContact = async (id: string) => {
        closeContextMenu();
        try {
            await invoke("delete_contact", { mldsaPubkeyHex: id });
            ondelete?.(id);
        } catch (e) {
            console.warn("删除联系人失败:", e);
        }
    };

    // 删除按钮点击（阻止事件冒泡，避免触发联系人选择）
    const handleDeleteBtnClick = (e: MouseEvent, id: string) => {
        e.stopPropagation();
        handleDeleteContact(id);
    };

    // 点击其他地方关闭右键菜单
    const handleWindowClick = () => {
        if (contextMenu) closeContextMenu();
    };
</script>

<svelte:window onclick={handleWindowClick} />

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
        <input placeholder="搜索..." name="contact_search" bind:value={q} />
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
            <VList
                bind:this={list}
                data={filtered}
                getKey={(c) => c.pubkey_hex}
            >
                {#snippet children(c)}
                    <div
                        class="item-wrap"
                        class:sel={selectedId === c.pubkey_hex}
                    >
                        <button
                            type="button"
                            class="item"
                            class:sel={selectedId === c.pubkey_hex}
                            class:online={c.online}
                            onclick={() => select(c.pubkey_hex)}
                            oncontextmenu={(e) =>
                                handleContextMenu(e, c.pubkey_hex)}
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
                        <button
                            type="button"
                            class="delete-btn"
                            onclick={(e) =>
                                handleDeleteBtnClick(e, c.pubkey_hex)}
                            aria-label={`删除联系人 ${c.name}`}
                            title="删除联系人"
                        >
                            <svg
                                width="16"
                                height="16"
                                viewBox="0 0 24 24"
                                fill="none"
                                stroke="currentColor"
                                stroke-width="2"
                            >
                                <polyline points="3 6 5 6 21 6" />
                                <path
                                    d="M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6m3 0V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2"
                                />
                                <line x1="10" y1="11" x2="10" y2="17" />
                                <line x1="14" y1="11" x2="14" y2="17" />
                            </svg>
                        </button>
                    </div>
                {/snippet}
            </VList>
        {/if}
    </div>

    <div class="foot">{filtered.length} 位联系人</div>
</div>

<!-- 右键菜单 -->
{#if contextMenu}
    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <div
        class="context-menu"
        style="left: {contextMenu.x}px; top: {contextMenu.y}px;"
        onclick={() => {}}
        onkeydown={() => {}}
    >
        <button
            class="context-menu-item danger"
            onclick={() =>
                contextMenu && handleDeleteContact(contextMenu.contactId)}
        >
            <svg
                width="16"
                height="16"
                viewBox="0 0 24 24"
                fill="none"
                stroke="currentColor"
                stroke-width="2"
            >
                <polyline points="3 6 5 6 21 6" />
                <path
                    d="M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6m3 0V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2"
                />
                <line x1="10" y1="11" x2="10" y2="17" />
                <line x1="14" y1="11" x2="14" y2="17" />
            </svg>
            删除联系人
        </button>
    </div>
{/if}

<style>
    .list {
        display: flex;
        flex-direction: column;
        height: 100%;
        background: color-mix(in srgb, var(--bg-primary) 70%, transparent);
        backdrop-filter: blur(10px);
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

    .item-wrap {
        position: relative;
        display: flex;
        align-items: center;
        border-radius: 10px;
        transition: background 0.15s;
    }
    .item-wrap:hover {
        background: var(--bg-tertiary, #1a1a1a);
    }
    .item-wrap:hover .delete-btn {
        opacity: 1;
        visibility: visible;
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
        flex: 1;
        min-width: 0;
    }
    .item:hover {
        background: transparent;
    }
    .item-wrap.sel,
    .item.sel {
        background: #1e3a5f;
    }
    .item.sel .name {
        color: #3b82f6;
    }

    .delete-btn {
        opacity: 0;
        visibility: hidden;
        position: absolute;
        right: 8px;
        top: 50%;
        transform: translateY(-50%);
        width: 32px;
        height: 32px;
        display: grid;
        place-items: center;
        background: var(--bg-secondary, #1a1a2e);
        border: 1px solid var(--border-color, #2a2a2a);
        border-radius: 8px;
        color: var(--text-secondary, #737373);
        cursor: pointer;
        transition: all 0.15s;
        z-index: 2;
        padding: 0;
    }
    .delete-btn:hover {
        color: #ef4444;
        background: rgba(239, 68, 68, 0.1);
        border-color: rgba(239, 68, 68, 0.3);
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

    /* 右键菜单样式 */
    .context-menu {
        position: fixed;
        z-index: 1000;
        background: var(--bg-secondary, #1a1a2e);
        border: 1px solid var(--border-color, #2a2a2a);
        border-radius: 8px;
        padding: 4px;
        min-width: 160px;
        box-shadow: 0 4px 16px rgba(0, 0, 0, 0.3);
        backdrop-filter: blur(10px);
    }
    .context-menu-item {
        display: flex;
        align-items: center;
        gap: 8px;
        width: 100%;
        padding: 8px 12px;
        border: none;
        background: transparent;
        color: var(--text-primary, #fafafa);
        font-size: 13px;
        cursor: pointer;
        border-radius: 6px;
        text-align: left;
        transition: background 0.15s;
    }
    .context-menu-item:hover {
        background: var(--bg-tertiary, #2a2a2a);
    }
    .context-menu-item.danger {
        color: #ef4444;
    }
    .context-menu-item.danger:hover {
        background: rgba(239, 68, 68, 0.1);
    }
    .context-menu-item svg {
        flex-shrink: 0;
    }
</style>
