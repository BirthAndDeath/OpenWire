<script lang="ts">
    import { VList, type VListHandle } from "virtua/svelte";
    import { invoke } from "@tauri-apps/api/core";
    import { _ } from "svelte-i18n";
    import Portal from "#lib/components/Portal.svelte";
    import { tt } from "#lib/i18n";

    interface Contact {
        order: number;
        pubkey_hex: string;
        name: string;
        avatar?: string;
        lastMsg?: string;
        lastTime?: number;
        unread: number;
        online?: boolean;
        notes?: string;
        group?: string;
        blocked?: boolean;
        nickname?: string | null;
    }

    // 列表条目：分组头或联系人
    interface ListEntry {
        kind: "header" | "item";
        key: string;
        label?: string;
        contact?: Contact;
    }

    let {
        contacts = [],
        selectedId = null,
        onselect,
        ondelete,
        onmeta,
        onmsgjump,
    }: {
        contacts: Contact[];
        selectedId: string | null;
        onselect?: (id: string) => void;
        ondelete?: (id: string) => void;
        onmeta?: () => void;
        /** 消息搜索结果跳转：打开会话并定位到该消息行 */
        onmsgjump?: (peerId: string, msgId: number) => void;
    } = $props();

    let q = $state("");
    let list: VListHandle | undefined = $state(undefined);
    // 搜索模式开关仅在搜索框聚焦/有关键词时显示
    let searchModesVisible = $state(false);
    // 搜索模式：按消息（后端全文匹配）/ 本地联系人（name/notes 本地过滤），可同时启用=交集
    let searchByMsg = $state(false);
    let searchByLocal = $state(true);
    let msgSearching = $state(false);
    // 后端返回的消息匹配行（含 id/联系人/预览/时间）
    let msgHits = $state<
        {
            id: number;
            peer_pubkey_hex: string;
            name: string;
            content: string;
            ts: number;
        }[]
    >([]);

    // 右键菜单状态
    let contextMenu = $state<{
        x: number;
        y: number;
        contactId: string;
    } | null>(null);

    // 复制 MLDSA-SHA256 成功/失败提示（轻量 toast，不依赖父组件）
    let copyToast = $state("");
    let copyToastTimer: ReturnType<typeof setTimeout> | null = null;
    function showCopyToast(msg: string) {
        copyToast = msg;
        if (copyToastTimer) clearTimeout(copyToastTimer);
        copyToastTimer = setTimeout(() => {
            copyToast = "";
            copyToastTimer = null;
        }, 2000);
    }

    // SHA256(公钥 hex 字符串) 摘要：与后端 dht_key / 扫码 digest 一致（小写 hex）
    const sha256Hex = async (text: string): Promise<string> => {
        const buf = await crypto.subtle.digest(
            "SHA-256",
            new TextEncoder().encode(text),
        );
        return [...new Uint8Array(buf)]
            .map((b) => b.toString(16).padStart(2, "0"))
            .join("");
    };

    // 复制好友的 MLDSA-SHA256 字符串到剪贴板
    const handleCopyMldsa = async (id: string) => {
        closeContextMenu();
        try {
            const digest = await sha256Hex(id);
            await navigator.clipboard.writeText(digest);
            showCopyToast(tt("copied_to_clipboard", { values: { label: tt("menu_copy_mldsa") } }));
        } catch (e) {
            showCopyToast(tt("copy_failed", { values: { error: e } }));
        }
    };

    // 按消息搜索：关键词变化时（防抖 250ms）请求后端全文匹配
    // 请求序号：丢弃过期响应，避免慢的旧查询覆盖新查询结果
    let searchSeq = 0;
    $effect(() => {
        const kw = q.trim();
        if (!searchByMsg || !kw) {
            msgHits = [];
            return;
        }
        const seq = ++searchSeq;
        const t = setTimeout(async () => {
            msgSearching = true;
            try {
                const raw = (await invoke(
                    "search_messages",
                    { query: kw },
                )) as string;
                if (seq === searchSeq) msgHits = JSON.parse(raw);
            } catch (e) {
                if (seq === searchSeq) {
                    console.warn("消息搜索失败:", e);
                    msgHits = [];
                }
            } finally {
                if (seq === searchSeq) msgSearching = false;
            }
        }, 250);
        return () => clearTimeout(t);
    });

    // 消息命中相关：peer 集合（用于交集/仅消息）与预览
    let msgPeers = $derived(new Set(msgHits.map((h) => h.peer_pubkey_hex)));
    function hitFor(peer: string) {
        return msgHits.find((x) => x.peer_pubkey_hex === peer);
    }
    function previewFor(peer: string): string | undefined {
        const h = hitFor(peer);
        return h ? h.content.slice(0, 60) : undefined;
    }

    // 点击联系人：消息搜索命中时跳转定位到该消息；否则普通打开会话
    function onItemClick(c: Contact) {
        const h = hitFor(c.pubkey_hex);
        if (searchByMsg && h && h.id) {
            onmsgjump?.(c.pubkey_hex, h.id);
        } else {
            select(c.pubkey_hex);
        }
    }

    // 模式感知的可显示联系人：
    // - 仅消息：后端命中的联系人
    // - 仅本地或全关：本地 name/notes 过滤（q 为空 = 全部）
    // - 同时启用：本地过滤 ∩ 消息命中（交集）
    let filtered = $derived.by((): Contact[] => {
        const kw = q.trim().toLowerCase();
        const nameMatch = (c: Contact) =>
            !kw ||
            c.name.toLowerCase().includes(kw) ||
            (c.notes ?? "").toLowerCase().includes(kw);

        let arr: Contact[];
        if (searchByMsg && !searchByLocal) {
            arr = contacts.filter((c) => msgPeers.has(c.pubkey_hex));
        } else if (searchByMsg) {
            arr = contacts.filter(
                (c) => nameMatch(c) && msgPeers.has(c.pubkey_hex),
            );
        } else {
            arr = contacts.filter(nameMatch);
        }
        return arr.sort((a, b) => a.order - b.order);
    });

    // 现有分组列表（供分组输入框下拉提示；输入新名称即为新建分组）
    let existingGroups = $derived(
        [...new Set(contacts.map((c) => c.group).filter((g) => g && g.trim()))].sort(),
    );

    // 有分组时按分组头插入列表；无任何分组则保持平铺（不改变现有观感）
    let listEntries = $derived.by((): ListEntry[] => {
        const grouped = filtered.some((c) => c.group);
        if (!grouped)
            return filtered.map((c) => ({
                kind: "item" as const,
                key: c.pubkey_hex,
                contact: c,
            }));
        const map = new Map<string, ListEntry[]>();
        for (const c of filtered) {
            const g = c.group || "";
            if (!map.has(g))
                map.set(g, [
                    { kind: "header", key: `h:${g}`, label: g },
                ]);
            map.get(g)!.push({ kind: "item", key: c.pubkey_hex, contact: c });
        }
        return [...map.keys()].sort().flatMap((g) => map.get(g)!);
    });

    const select = (id: string) => onselect?.(id);

    const fmtTime = (ts?: number) => {
        if (!ts) return "";
        const d = new Date(ts),
            now = new Date();
        return d.toDateString() === now.toDateString()
            ? d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })
            : d.toLocaleDateString([], { month: "short", day: "numeric" });
    };

    // 右键点击处理（钳制到视口内，避免贴边时菜单溢出）
    const handleContextMenu = (e: MouseEvent, id: string) => {
        e.preventDefault();
        const MENU_W = 200;
        const MENU_H = 220; // 5 项菜单（编辑/分组/屏蔽/复制/删除）的估算高度
        const x = Math.min(e.clientX, window.innerWidth - MENU_W - 8);
        const y = Math.min(e.clientY, window.innerHeight - MENU_H - 8);
        contextMenu = { x: Math.max(8, x), y: Math.max(8, y), contactId: id };
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

    // 编辑弹窗状态（昵称/备注/分组）
    let editTarget = $state<{
        pubkey_hex: string;
        name: string;
        notes: string;
        group: string;
    } | null>(null);
    let editName = $state("");
    let editNotes = $state("");
    let editGroup = $state("");

    const openEdit = (id: string) => {
        const c = contacts.find((x) => x.pubkey_hex === id);
        if (!c) return;
        editName = c.nickname ?? "";
        editNotes = c.notes ?? "";
        editGroup = c.group ?? "";
        editTarget = {
            pubkey_hex: id,
            name: c.nickname ?? "",
            notes: c.notes ?? "",
            group: c.group ?? "",
        };
        closeContextMenu();
    };

    const saveMeta = async (field?: "name" | "notes" | "group") => {
        if (!editTarget) return;
        const pub = editTarget.pubkey_hex;
        const patch = {
            mldsaPubkeyHex: pub,
            name: field === undefined || field === "name" ? editName : undefined,
            notes: field === undefined || field === "notes" ? editNotes : undefined,
            groupName: field === undefined || field === "group" ? editGroup : undefined,
        };
        try {
            await invoke("update_contact_meta", patch);
            onmeta?.();
        } catch (e) {
            console.warn("更新联系人失败:", e);
        }
        if (field === undefined) editTarget = null;
    };

    const handleBlock = async (id: string, blocked: boolean) => {
        closeContextMenu();
        try {
            await invoke("update_contact_meta", {
                mldsaPubkeyHex: id,
                blocked,
            });
            onmeta?.();
        } catch (e) {
            console.warn("屏蔽操作失败:", e);
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
        <input
            placeholder={$_("search_placeholder")}
            name="contact_search"
            bind:value={q}
            onfocus={() => (searchModesVisible = true)}
            onblur={() =>
                setTimeout(() => (searchModesVisible = false), 150)}
        />
        {#if q}<button class="clear" onclick={() => (q = "")}>×</button>{/if}
    </div>
    {#if searchModesVisible || q !== ""}
        <div class="search-modes">
            <label class="mode-chip">
                <input type="checkbox" bind:checked={searchByLocal} />
                <span>{$_("search_local_contacts")}</span>
            </label>
            <label class="mode-chip">
                <input type="checkbox" bind:checked={searchByMsg} />
                <span>{$_("search_messages_mode")}</span>
            </label>
        </div>
    {/if}

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
                <p>
                        {msgSearching
                            ? $_("searching")
                            : q || searchByMsg || searchByLocal
                              ? $_("no_results")
                              : $_("no_contacts")}
                    </p>
            </div>
        {:else}
            <VList
                bind:this={list}
                data={listEntries}
                getKey={(c) => c.key}
            >
                {#snippet children(entry)}
                    {#if entry.kind === "header"}
                        <div class="group-header">{entry.label || $_("group_default")}</div>
                    {:else if entry.contact}
                        {@const c = entry.contact}
                        <div
                            class="item-wrap"
                            class:sel={selectedId === c.pubkey_hex}
                        >
                        <button
                            type="button"
                            class="item"
                            class:sel={selectedId === c.pubkey_hex}
                            class:online={c.online}
                            onclick={() => onItemClick(c)}
                            oncontextmenu={(e) =>
                                handleContextMenu(e, c.pubkey_hex)}
                            aria-label={$_("select_contact", { values: { name: c.name } })}
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
                                {#if c.notes}<p class="notes">{c.notes}</p>{/if}
                                {#if previewFor(c.pubkey_hex) || c.lastMsg}<p
                                        class="msg"
                                        class:unread={c.unread > 0}
                                    >
                                        {previewFor(c.pubkey_hex) ?? c.lastMsg}
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
                            aria-label={$_("delete_contact_aria", { values: { name: c.name } })}
                            title={$_("delete_contact")}
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
                {/if}
                {/snippet}
            </VList>
        {/if}
    </div>

    <div class="foot">{$_("contact_count", { values: { n: filtered.length } })}</div>
</div>

<!-- 右键菜单（Portal 到 body，确保 fixed 按视口定位） -->
<Portal>
{#if contextMenu}
    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <div
        class="context-menu"
        style="left: {contextMenu.x}px; top: {contextMenu.y}px;"
        onclick={() => {}}
        onkeydown={() => {}}
    >
        <button
            class="context-menu-item"
            onclick={() =>
                contextMenu && openEdit(contextMenu.contactId)}
        >
            <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
                <path d="M17 3a2.85 2.83 0 1 1 4 4L7.5 20.5 2 22l1.5-5.5Z" />
            </svg>
            {$_("menu_edit_profile")}
        </button>
        <button
            class="context-menu-item"
            onclick={() =>
                contextMenu && openEdit(contextMenu.contactId)}
        >
            <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
                <rect x="3" y="3" width="7" height="7" rx="1" />
                <rect x="14" y="3" width="7" height="7" rx="1" />
                <rect x="3" y="14" width="7" height="7" rx="1" />
                <rect x="14" y="14" width="7" height="7" rx="1" />
            </svg>
            {$_("menu_move_group")}
        </button>
        <button
            class="context-menu-item"
            onclick={() => {
                const id = contextMenu!.contactId;
                handleBlock(
                    id,
                    !contacts.find((c) => c.pubkey_hex === id)?.blocked,
                );
            }}
        >
            <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
                <path d="M12 2a9 9 0 1 0 9 9" />
                <path d="M21 2v5h-5" />
            </svg>
            {contacts.find((c) => c.pubkey_hex === contextMenu?.contactId)
                ?.blocked
                ? $_("menu_unblock")
                : $_("menu_block")}
        </button>
        <button
            class="context-menu-item"
            onclick={() => contextMenu && handleCopyMldsa(contextMenu.contactId)}
        >
            <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
                <rect x="9" y="9" width="13" height="13" rx="2" />
                <path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1" />
            </svg>
            {$_("menu_copy_mldsa")}
        </button>
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
            {$_("delete_contact")}
        </button>
    </div>
{/if}
{#if copyToast}
    <div class="copy-toast" role="status">{copyToast}</div>
{/if}
</Portal>

<!-- 编辑联系人弹窗（昵称/备注/分组），Portal 到 body -->
<Portal>
{#if editTarget}
    <!-- svelte-ignore a11y_click_events_have_key_events -->
    <div class="modal-overlay" role="presentation" aria-hidden="true" onclick={() => (editTarget = null)}>
        <div class="edit-modal" role="dialog" aria-label={$_("contact_edit_title")} tabindex="-1" onclick={(e) => e.stopPropagation()}>
            <h3>{$_("contact_edit_title")}</h3>
            <label>
                {$_("contact_nickname")}
                <input type="text" bind:value={editName} maxlength="256" />
            </label>
            <label>
                {$_("contact_notes")}
                <textarea bind:value={editNotes} maxlength="256" rows="3"></textarea>
            </label>
            <label>
                {$_("contact_group")}
                <input
                    type="text"
                    bind:value={editGroup}
                    maxlength="256"
                    placeholder={$_("group_empty_hint")}
                    list="group-list"
                />
            </label>
            <datalist id="group-list">
                {#each existingGroups as g}
                    <option value={g}></option>
                {/each}
            </datalist>
            <div class="modal-actions">
                <button class="btn-ghost" onclick={() => (editTarget = null)}>{$_("action_cancel")}</button>
                <button class="btn-primary" onclick={() => saveMeta()}>{$_("action_save")}</button>
            </div>
        </div>
    </div>
{/if}
</Portal>

<style>
    .list {
        display: flex;
        flex-direction: column;
        height: 100%;
        background: transparent;
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
    .search-modes {
        display: flex;
        gap: 10px;
        padding: 0 16px 8px;
        flex-wrap: wrap;
        border-bottom: 1px solid var(--border-color, #2a2a2a);
    }
    .mode-chip {
        display: inline-flex;
        align-items: center;
        gap: 4px;
        font-size: 12px;
        color: var(--text-secondary, #737373);
        cursor: pointer;
        user-select: none;
    }
    .mode-chip input {
        accent-color: var(--color-primary);
        margin: 0;
    }
    .clear {
        width: 18px;
        height: 18px;
        display: grid;
        place-items: center;
        background: var(--bg-secondary);
        border: none;
        border-radius: 50%;
        color: var(--text-muted, #888);
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
        background: var(--color-selected);
    }
    .item.sel .name {
        color: var(--color-primary);
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
        color: var(--color-danger);
        background: var(--color-danger-soft);
        border-color: var(--color-danger-soft);
    }

    .avatar {
        position: relative;
        flex-shrink: 0;
    }
    .avatar img,
    .fallback {
        width: var(--avatar-size, 44px);
        height: var(--avatar-size, 44px);
        border-radius: 50%;
        object-fit: cover;
    }
    .fallback {
        display: grid;
        place-items: center;
        background: linear-gradient(135deg, var(--color-primary), var(--color-primary-strong));
        color: white;
        font-size: var(--avatar-font, 14px);
        font-weight: 600;
    }
    .dot {
        position: absolute;
        bottom: 2px;
        right: 2px;
        width: 12px;
        height: 12px;
        background: var(--color-success);
        border: 2px solid var(--bg-primary, #0f0f0f);
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
    .notes {
        font-size: 11px;
        line-height: 1.4;
        color: var(--text-secondary, #737373);
        white-space: nowrap;
        overflow: hidden;
        text-overflow: ellipsis;
        margin: 0;
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
        color: var(--color-danger);
    }
    .context-menu-item.danger:hover {
        background: var(--color-danger-soft);
    }
    .context-menu-item svg {
        flex-shrink: 0;
    }

    /* 复制成功/失败提示 */
    .copy-toast {
        position: fixed;
        top: 16px;
        left: 50%;
        transform: translateX(-50%);
        z-index: 1200;
        max-width: 80vw;
        padding: 8px 16px;
        border-radius: 20px;
        background: rgba(0, 0, 0, 0.85);
        color: #fafafa;
        font-size: 13px;
        box-shadow: 0 4px 16px rgba(0, 0, 0, 0.3);
        pointer-events: none;
        animation: copyToastIn 0.15s ease-out;
    }
    @keyframes copyToastIn {
        from {
            opacity: 0;
            transform: translateX(-50%) translateY(-6px);
        }
        to {
            opacity: 1;
            transform: translateX(-50%) translateY(0);
        }
    }

    /* 分组头 */
    .group-header {
        padding: 10px 16px 4px;
        font-size: 12px;
        font-weight: 600;
        color: var(--text-secondary, #737373);
        letter-spacing: 0.02em;
    }

    /* 编辑弹窗 */
    .modal-overlay {
        position: fixed;
        inset: 0;
        z-index: 1100;
        display: grid;
        place-items: center;
        background: rgba(0, 0, 0, 0.45);
    }
    .edit-modal {
        width: min(320px, 86vw);
        box-sizing: border-box;
        max-height: 85dvh;
        overflow-y: auto;
        padding: 20px;
        border-radius: var(--radius-md, 12px);
        background: var(--bg-secondary, #1a1a2e);
        border: 1px solid var(--border-color, #2a2a2a);
        display: flex;
        flex-direction: column;
        gap: 12px;
    }
    .edit-modal h3 {
        margin: 0;
        font-size: 15px;
    }
    .edit-modal label {
        display: flex;
        flex-direction: column;
        gap: 4px;
        font-size: 12px;
        color: var(--text-secondary, #737373);
    }
    .edit-modal input,
    .edit-modal textarea {
        background: var(--bg-primary, #0f0f0f);
        border: 1px solid var(--border-color, #2a2a2a);
        border-radius: 8px;
        padding: 8px 10px;
        color: var(--text-primary, #fafafa);
        font-size: 13px;
        font-family: inherit;
        outline: none;
        resize: none;
    }
    .edit-modal input:focus,
    .edit-modal textarea:focus {
        border-color: var(--color-primary);
    }
    .modal-actions {
        display: flex;
        justify-content: flex-end;
        gap: 10px;
        margin-top: 4px;
    }
    .btn-ghost,
    .btn-primary {
        padding: 7px 16px;
        border-radius: 8px;
        font-size: 13px;
        cursor: pointer;
        transition: all 0.15s;
    }
    .btn-ghost {
        background: transparent;
        border: 1px solid var(--border-color, #2a2a2a);
        color: var(--text-secondary, #737373);
    }
    .btn-primary {
        background: var(--color-primary);
        border: 1px solid var(--color-primary);
        color: white;
    }
    .btn-primary:hover {
        background: var(--color-primary-strong);
    }
</style>
