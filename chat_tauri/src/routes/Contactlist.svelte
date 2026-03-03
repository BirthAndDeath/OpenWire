<script lang="ts">
    import { VList } from "virtua/svelte";

    // === 类型定义 ===
    interface Contact {
        order: number;
        peerid: string;
        name: string;
        avatar?: string;
        lastMessage?: string;
        lastTime?: number;
        unread: number;
        isOnline?: boolean;
    }

    // === Props ===
    let {
        contacts = [],
        selectedId = null,
        onselect,
        oncontextmenu,
    }: {
        contacts: Contact[];
        selectedId: string | null;
        onselect?: (id: string) => void;
        oncontextmenu?: (e: MouseEvent, id: string) => void;
    } = $props();

    // === 状态 ===
    let searchQuery = $state("");
    let vlistRef: any = $state(undefined);

    // === 派生状态：过滤后的联系人 ===
    let filteredContacts = $derived(
        contacts
            .filter(
                (c) =>
                    c.name.toLowerCase().includes(searchQuery.toLowerCase()) ||
                    c.lastMessage
                        ?.toLowerCase()
                        .includes(searchQuery.toLowerCase()),
            )
            .sort((a, b) => a.order - b.order),
    );

    // === 操作 ===
    function select(id: string) {
        onselect?.(id);
    }

    function formatTime(ts?: number): string {
        if (!ts) return "";
        const date = new Date(ts);
        const now = new Date();
        const isToday = date.toDateString() === now.toDateString();

        if (isToday) {
            return date.toLocaleTimeString([], {
                hour: "2-digit",
                minute: "2-digit",
            });
        }
        return date.toLocaleDateString([], { month: "short", day: "numeric" });
    }
</script>

<div class="contact-list">
    <!-- 搜索栏 -->
    <div class="search-bar">
        <svg
            class="search-icon"
            width="16"
            height="16"
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            stroke-width="2"
        >
            <circle cx="11" cy="11" r="8" />
            <path d="m21 21-4.35-4.35" />
        </svg>
        <input
            type="text"
            placeholder="搜索联系人..."
            bind:value={searchQuery}
        />
        {#if searchQuery}
            <button class="clear-btn" onclick={() => (searchQuery = "")}
                >×</button
            >
        {/if}
    </div>

    <!-- 联系人列表 -->
    <div class="list-container">
        {#if filteredContacts.length === 0}
            <div class="empty-state">
                <svg
                    width="48"
                    height="48"
                    viewBox="0 0 24 24"
                    fill="none"
                    stroke="currentColor"
                    stroke-width="1.5"
                >
                    <path d="M17 21v-2a4 4 0 0 0-4-4H5a4 4 0 0 0-4 4v2" />
                    <circle cx="9" cy="7" r="4" />
                    <path d="M23 21v-2a4 4 0 0 0-3-3.87" />
                    <path d="M16 3.13a4 4 0 0 1 0 7.75" />
                </svg>
                <p>{searchQuery ? "无搜索结果" : "暂无联系人"}</p>
            </div>
        {:else}
            <VList
                bind:this={vlistRef}
                data={filteredContacts}
                getKey={(c: Contact) => c.peerid}
            >
                {#snippet children(contact)}
                    <div
                        class="contact-item"
                        class:selected={selectedId === contact.peerid}
                        class:online={contact.isOnline}
                        role="button"
                        tabindex="0"
                        onclick={() => select(contact.peerid)}
                        onkeydown={(e) => {
                            if (e.key === "Enter" || e.key === " ") {
                                e.preventDefault();
                                select(contact.peerid);
                            }
                        }}
                        oncontextmenu={(e) => {
                            e.preventDefault();
                            oncontextmenu?.(e, contact.peerid);
                        }}
                    >
                        <!-- 头像 -->
                        <div class="avatar">
                            {#if contact.avatar}
                                <img src={contact.avatar} alt={contact.name} />
                            {:else}
                                <div class="avatar-fallback">
                                    {contact.name.slice(0, 2).toUpperCase()}
                                </div>
                            {/if}
                            {#if contact.isOnline}
                                <span class="online-indicator"></span>
                                <!-- 修复自闭合标签 -->
                            {/if}
                        </div>

                        <!-- 信息 -->
                        <div class="info">
                            <div class="name-row">
                                <span class="name">{contact.name}</span>
                                {#if contact.lastTime}
                                    <span class="time"
                                        >{formatTime(contact.lastTime)}</span
                                    >
                                {/if}
                            </div>
                            {#if contact.lastMessage}
                                <p
                                    class="last-msg"
                                    class:unread={contact.unread > 0}
                                >
                                    {contact.lastMessage}
                                </p>
                            {/if}
                        </div>

                        <!-- 未读数 -->
                        {#if contact.unread > 0}
                            <span class="badge">
                                {contact.unread > 99 ? "99+" : contact.unread}
                            </span>
                        {/if}
                    </div>
                {/snippet}
            </VList>
        {/if}
    </div>

    <!-- 底部统计 -->
    <div class="footer">
        <span>{filteredContacts.length} 位联系人</span>
    </div>
</div>

<style>
    .contact-list {
        display: flex;
        flex-direction: column;
        height: 100%;
        background: #0f0f0f;
        border-right: 1px solid #2a2a2a;
    }

    /* 搜索栏 */
    .search-bar {
        display: flex;
        align-items: center;
        gap: 10px;
        padding: 12px 16px;
        border-bottom: 1px solid #2a2a2a;
    }

    .search-icon {
        color: #525252;
        flex-shrink: 0;
    }

    .search-bar input {
        flex: 1;
        background: transparent;
        border: none;
        color: #fafafa;
        font-size: 14px;
        outline: none;
    }

    .search-bar input::placeholder {
        color: #525252;
    }

    .clear-btn {
        width: 18px;
        height: 18px;
        display: flex;
        align-items: center;
        justify-content: center;
        background: #333;
        border: none;
        border-radius: 50%;
        color: #888;
        font-size: 12px;
        cursor: pointer;
    }

    .clear-btn:hover {
        background: #444;
    }

    /* 列表容器 */
    .list-container {
        flex: 1;
        overflow: hidden;
        position: relative;
    }

    :global(.virtua-scroll-view) {
        padding: 8px;
    }

    /* 空状态 */
    .empty-state {
        display: flex;
        flex-direction: column;
        align-items: center;
        justify-content: center;
        height: 100%;
        color: #525252;
        gap: 12px;
    }

    .empty-state p {
        font-size: 14px;
    }

    /* 联系人项 */
    .contact-item {
        display: flex;
        align-items: center;
        gap: 12px;
        padding: 12px;
        border-radius: 10px;
        cursor: pointer;
        transition: all 0.15s;
        position: relative;
    }

    .contact-item:hover {
        background: #1a1a1a;
    }

    .contact-item.selected {
        background: #1e3a5f;
    }

    .contact-item.selected .name {
        color: #3b82f6;
    }

    /* 头像 */
    .avatar {
        position: relative;
        flex-shrink: 0;
    }

    .avatar img,
    .avatar-fallback {
        width: 44px;
        height: 44px;
        border-radius: 50%;
        object-fit: cover;
    }

    .avatar-fallback {
        display: flex;
        align-items: center;
        justify-content: center;
        background: linear-gradient(135deg, #3b82f6, #2563eb);
        color: white;
        font-size: 14px;
        font-weight: 600;
    }

    .online-indicator {
        position: absolute;
        bottom: 2px;
        right: 2px;
        width: 12px;
        height: 12px;
        background: #22c55e;
        border: 2px solid #0f0f0f;
        border-radius: 50%;
    }

    /* 信息区域 */
    .info {
        flex: 1;
        min-width: 0;
    }

    .name-row {
        display: flex;
        justify-content: space-between;
        align-items: center;
        margin-bottom: 4px;
    }

    .name {
        font-size: 15px;
        font-weight: 500;
        color: #fafafa;
        white-space: nowrap;
        overflow: hidden;
        text-overflow: ellipsis;
    }

    .time {
        font-size: 11px;
        color: #525252;
        flex-shrink: 0;
    }

    .last-msg {
        font-size: 13px;
        color: #737373;
        white-space: nowrap;
        overflow: hidden;
        text-overflow: ellipsis;
        margin: 0;
    }

    .last-msg.unread {
        color: #fafafa;
        font-weight: 500;
    }

    /* 未读徽章 */
    .badge {
        min-width: 20px;
        height: 20px;
        padding: 0 6px;
        display: flex;
        align-items: center;
        justify-content: center;
        background: #ef4444;
        color: white;
        font-size: 12px;
        font-weight: 600;
        border-radius: 10px;
        flex-shrink: 0;
    }

    /* 底部 */
    .footer {
        padding: 12px 16px;
        border-top: 1px solid #2a2a2a;
        font-size: 12px;
        color: #525252;
    }
</style>
