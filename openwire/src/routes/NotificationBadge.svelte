<script lang="ts">
  interface Notification {
    id: number;
    message: string;
    ts: number;
  }

  let { onNotification }: { onNotification: (cb: (msg: string) => void) => void } = $props();

  let notifications = $state<Notification[]>([]);
  let unreadCount = $state(0);
  let expanded = $state(false);
  let nextId = $state(0);
  let popover: HTMLDivElement | undefined = $state();

  function add(msg: string) {
    const id = nextId++;
    notifications = [...notifications, { id, message: msg, ts: Date.now() }];
    unreadCount++;
    if (notifications.length > 50) {
      notifications = notifications.slice(-50);
    }
  }

  $effect(() => {
    onNotification((msg) => add(msg));
  });

function toggle(e: MouseEvent) {
    e.stopPropagation();
    expanded = !expanded;
    if (expanded) {
      unreadCount = 0;
    }
  }

  function dismiss(id: number) {
    notifications = notifications.filter((n) => n.id !== id);
  }

  function clearAll() {
    notifications = [];
    unreadCount = 0;
    expanded = false;
  }

  function formatTime(ts: number): string {
    const d = new Date(ts);
    return `${d.getHours().toString().padStart(2, "0")}:${d.getMinutes().toString().padStart(2, "0")}:${d.getSeconds().toString().padStart(2, "0")}`;
  }

  function handleClickOutside(e: MouseEvent) {
    if (expanded && popover && !popover.contains(e.target as Node)) {
      expanded = false;
    }
  }

  $effect(() => {
    if (expanded) {
      document.addEventListener("click", handleClickOutside);
    }
    return () => document.removeEventListener("click", handleClickOutside);
  });
</script>

{#if notifications.length > 0 || expanded}
<div class="notif-badge-container">
  <button class="notif-badge" onclick={toggle} aria-label="notifications">
    <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
      <path d="M18 8A6 6 0 0 0 6 8c0 7-3 9-3 9h18s-3-2-3-9" />
      <path d="M13.73 21a2 2 0 0 1-3.46 0" />
    </svg>
    {#if unreadCount > 0}
      <span class="badge-dot">{unreadCount > 99 ? "99+" : unreadCount}</span>
    {/if}
  </button>

  {#if expanded}
    <div class="notif-popover" bind:this={popover}>
      <div class="notif-header">
        <span class="notif-title">通知</span>
        {#if notifications.length > 0}
          <button class="clear-btn" onclick={clearAll}>清空</button>
        {/if}
      </div>
      <div class="notif-list">
        {#if notifications.length === 0}
          <div class="notif-empty">暂无通知</div>
        {:else}
          {#each notifications as n (n.id)}
            <div class="notif-item">
              <span class="notif-time">{formatTime(n.ts)}</span>
              <span class="notif-msg">{n.message}</span>
              <button class="notif-dismiss" onclick={() => dismiss(n.id)}>✕</button>
            </div>
          {/each}
        {/if}
      </div>
    </div>
  {/if}
</div>
{/if}

<style>
  .notif-badge-container {
    position: fixed;
    top: 12px;
    right: 12px;
    z-index: 999;
  }

  .notif-badge {
    position: relative;
    width: 40px;
    height: 40px;
    border-radius: 50%;
    background: var(--bg-secondary, #2a2a2a);
    border: 1px solid var(--border-color, #333);
    color: var(--text-secondary, #999);
    cursor: pointer;
    display: flex;
    align-items: center;
    justify-content: center;
    box-shadow: 0 2px 8px rgba(0, 0, 0, 0.3);
    transition: background 0.2s;
  }

  .notif-badge:hover {
    background: var(--bg-tertiary, #333);
    color: var(--text-primary, #eee);
  }

  .badge-dot {
    position: absolute;
    top: -4px;
    right: -4px;
    min-width: 18px;
    height: 18px;
    border-radius: 9px;
    background: #ef4444;
    color: white;
    font-size: 11px;
    font-weight: 700;
    display: flex;
    align-items: center;
    justify-content: center;
    padding: 0 4px;
    box-shadow: 0 0 4px rgba(239, 68, 68, 0.6);
  }

  .notif-popover {
    position: absolute;
    top: 48px;
    right: 0;
    width: 360px;
    max-height: 400px;
    background: var(--bg-secondary, #1a1a1a);
    border: 1px solid var(--border-color, #333);
    border-radius: 12px;
    box-shadow: 0 8px 32px rgba(0, 0, 0, 0.4);
    overflow: hidden;
    display: flex;
    flex-direction: column;
  }

  .notif-header {
    display: flex;
    justify-content: space-between;
    align-items: center;
    padding: 12px 16px;
    border-bottom: 1px solid var(--border-color, #333);
  }

  .notif-title {
    font-weight: 600;
    font-size: 14px;
    color: var(--text-primary, #eee);
  }

  .clear-btn {
    background: none;
    border: none;
    color: var(--text-secondary, #999);
    font-size: 12px;
    cursor: pointer;
    padding: 4px 8px;
    border-radius: 4px;
  }

  .clear-btn:hover {
    color: var(--text-primary, #eee);
    background: var(--bg-tertiary, #333);
  }

  .notif-list {
    overflow-y: auto;
    max-height: 340px;
  }

  .notif-empty {
    padding: 32px 16px;
    text-align: center;
    color: var(--text-secondary, #666);
    font-size: 13px;
  }

  .notif-item {
    display: flex;
    align-items: flex-start;
    gap: 8px;
    padding: 10px 16px;
    border-bottom: 1px solid var(--border-color, #222);
    font-size: 13px;
    line-height: 1.4;
  }

  .notif-item:last-child {
    border-bottom: none;
  }

  .notif-time {
    color: var(--text-secondary, #666);
    font-size: 11px;
    white-space: nowrap;
    margin-top: 2px;
    min-width: 56px;
  }

  .notif-msg {
    flex: 1;
    color: var(--text-primary, #ddd);
    word-break: break-word;
  }

  .notif-dismiss {
    background: none;
    border: none;
    color: var(--text-secondary, #555);
    cursor: pointer;
    font-size: 12px;
    padding: 2px 4px;
    border-radius: 4px;
    flex-shrink: 0;
  }

  .notif-dismiss:hover {
    color: var(--text-primary, #eee);
    background: var(--bg-tertiary, #333);
  }
</style>