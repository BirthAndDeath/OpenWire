<script lang="ts">
  import "../lib/i18n";
  import { _, locale } from "svelte-i18n";
  import { listen } from "@tauri-apps/api/event";
  import { invoke } from "@tauri-apps/api/core";
  import { onMount } from "svelte";
  import Input from "./Input.svelte";
  import Messagelist from "./Messagelist.svelte";
  import Contactlist from "./Contactlist.svelte";
  import { fly } from "svelte/transition";
  let warning = $state<string | null>(null);
  let timeout: ReturnType<typeof setTimeout> | null = null;

  // 显示 warning 的统一函数
  const showWarning = (message: string, duration: number = 5000) => {
    warning = message;
    if (timeout) {
      clearTimeout(timeout);
    }
    timeout = setTimeout(() => {
      warning = null;
      timeout = null;
    }, duration);
  };

  // 语言选项
  const languages = [
    { code: "en", name: "English" },
    { code: "zh", name: "中文" },
    { code: "fr", name: "Français" },
    { code: "es", name: "Español" },
    { code: "de", name: "Deutsch" },
    { code: "ja", name: "日本語" },
  ];

  interface ContactDto {
    peer_id: string;
    name: string;
    added_at: number;
  }

  interface IdentityDto {
    id: number;
    peer_id: string;
    is_current: boolean;
  }

  // 切换语言
  function changeLanguage(lang: string) {
    locale.set(lang);
  }

  let identities = $state<IdentityDto[]>([]);
  let currentIdentity = $state("");
  let loadingIdentities = $state(false);
  let loadingContacts = $state(false);

  const loadContacts = async () => {
    loadingContacts = true;
    try {
      const list: ContactDto[] = await invoke("list_contacts");
      contacts = list.map((c, index) => ({
        order: index,
        peerid: c.peer_id,
        name: c.name,
        lastMsg: "最近聊天",
        lastTime: c.added_at * 1000,
        unread: 0,
        online: false,
      }));
      if (!selectedId && contacts.length) selectedId = contacts[0].peerid;
    } catch (e) {
      showWarning(`加载联系人失败：${e}`);
    } finally {
      loadingContacts = false;
    }
  };

  const loadIdentities = async () => {
    loadingIdentities = true;
    try {
      identities = await invoke<IdentityDto[]>("list_identities");
      currentIdentity = identities.find((id) => id.is_current)?.peer_id ?? "";
    } catch (e) {
      showWarning(`加载身份失败：${e}`);
    } finally {
      loadingIdentities = false;
    }
  };

  const selectIdentity = async (peerid: string) => {
    if (!peerid) return;
    try {
      const ok = await invoke<boolean>("select_identity", { peerId: peerid });
      if (ok) {
        currentIdentity = peerid;
        await loadIdentities();
      }
    } catch (e) {
      showWarning(`切换身份失败：${e}`);
    }
  };

  const deleteIdentity = async (peerid: string) => {
    if (!peerid) return;
    try {
      const ok = await invoke<boolean>("delete_identity", { peerId: peerid });
      if (ok) {
        await loadIdentities();
        currentIdentity = identities.find((id) => id.is_current)?.peer_id ?? "";
      }
    } catch (e) {
      showWarning(`删除身份失败：${e}`);
    }
  };

  const createIdentity = async () => {
    try {
      const ok = await invoke<boolean>("generate_identity");
      if (ok) {
        await loadIdentities();
        showWarning("已生成新身份", 3000);
      }
    } catch (e) {
      showWarning(`生成身份失败：${e}`);
    }
  };

  const copyPeerId = async () => {
    if (!currentIdentity) return;
    try {
      await navigator.clipboard.writeText(currentIdentity);
      showWarning("PeerID 已复制到剪贴板", 3000);
    } catch (e) {
      showWarning(`复制失败：${e}`);
    }
  };

  onMount(() => {
    let unlisten: (() => void) | undefined;

    (async () => {
      unlisten = await listen<string>("warning", (e) => {
        showWarning(e.payload, 5000);
        console.warn("Received warning from backend:", e.payload);
      });
    })();

    loadContacts();
    loadIdentities();

    return () => {
      unlisten?.();
      if (timeout) {
        clearTimeout(timeout);
        timeout = null;
      }
    };
  });

  // === 状态 ===
  let msgListRef = $state<ReturnType<typeof Messagelist>>();
  let selectedId = $state<string | null>(null);
  let sidebarW = $state(300);
  let inputH = $state(150);
  let friendInput = $state("");

  let contacts = $state([
    {
      order: 0,
      peerid: "0",
      name: "topic",
      lastMsg: "欢迎来到聊天室",
      lastTime: Date.now(),
      unread: 0,
      online: true,
    },
    {
      order: 1,
      peerid: "1",
      name: "Alice",
      lastMsg: "好的，明天见",
      lastTime: Date.now(),
      unread: 2,
      online: true,
    },
    {
      order: 2,
      peerid: "2",
      name: "Bob",
      lastMsg: "收到",
      lastTime: Date.now() - 864e5,
      unread: 0,
      online: false,
    },
    {
      order: 3,
      peerid: "3",
      name: "Charlie",
      lastMsg: "在吗？",
      lastTime: Date.now() - 1728e5,
      unread: 5,
      online: true,
    },
  ]);

  // === 监听消息 ===
  onMount(() => {
    let unlisten: (() => void) | undefined;
    (async () => {
      unlisten = await listen<string>("chat-message", (e) => {
        msgListRef?.add(e.payload, false);
        contacts = contacts.map((c) =>
          c.peerid === selectedId
            ? { ...c, lastMsg: e.payload, lastTime: Date.now() }
            : c,
        );
      });
    })();
    return () => unlisten?.();
  });

  // === 拖动调整 ===
  function drag(start: number, min: number, max: number, vertical: boolean) {
    return (e: MouseEvent) => {
      const val = vertical
        ? Math.max(min, Math.min(max, e.clientX))
        : Math.max(min, Math.min(max, window.innerHeight - e.clientY - 16));
      vertical ? (sidebarW = val) : (inputH = val);
    };
  }

  function startDrag(isSidebar: boolean) {
    const handler = drag(
      isSidebar ? 0 : 0,
      isSidebar ? 200 : 100,
      isSidebar ? 500 : 300,
      isSidebar,
    );
    const up = () => {
      document.removeEventListener("mousemove", handler);
      document.removeEventListener("mouseup", up);
    };
    document.addEventListener("mousemove", handler);
    document.addEventListener("mouseup", up);
  }

  // === 操作 ===
  const select = (id: string) => (selectedId = id);

  const send = (text: string) => {
    if (!selectedId) return;
    msgListRef?.add(text, true);
    contacts = contacts.map((c) =>
      c.peerid === selectedId
        ? { ...c, lastMsg: text, lastTime: Date.now() }
        : c,
    );
  };

  const addFriend = () => {
    const key = friendInput.trim();
    if (!key) return;
    contacts = [
      ...contacts,
      {
        order: contacts.length,
        peerid: crypto.randomUUID(),
        name: `User ${key.slice(0, 8)}...`,
        lastMsg: "新朋友",
        lastTime: Date.now(),
        unread: 0,
        online: false,
      },
    ];
    friendInput = "";
  };

  const onKey = (e: KeyboardEvent) =>
    e.key === "Enter" && (e.preventDefault(), addFriend());
</script>

<main class="container">
  <aside class="sidebar" style="width: {sidebarW}px">
    <!-- 语言选择器 -->
    <div class="identity-panel">
      {#if currentIdentity}
        <button 
          type="button"
          class="current-identity-display" 
          onclick={copyPeerId} 
          title="点击复制 PeerID"
          aria-label="复制当前身份 PeerID"
        >
          <span class="current-label">当前身份:</span>
          <code class="peerid-badge">{currentIdentity}</code>
        </button>
      {/if}
      <div class="identity-control">
        <label for="identity-select">切换身份：</label>
        <select
          id="identity-select"
          bind:value={currentIdentity}
          onchange={(e) =>
            selectIdentity((e.target as HTMLSelectElement).value)}
          disabled={loadingIdentities}
        >
          {#if identities.length === 0}
            <option value="">暂无身份</option>
          {:else}
            {#each identities as idt}
              <option value={idt.peer_id}>
                {idt.peer_id}{idt.is_current ? " (当前)" : ""}
              </option>
            {/each}
          {/if}
        </select>
      </div>
      <div class="identity-actions">
        <button
          type="button"
          class="identity-btn"
          onclick={createIdentity}
          disabled={loadingIdentities}
        >
          生成身份
        </button>
        <button
          type="button"
          class="identity-btn"
          onclick={copyPeerId}
          disabled={loadingIdentities || !currentIdentity}
          title="复制当前 PeerID"
        >
          复制身份
        </button>
        <button
          type="button"
          class="identity-btn delete"
          onclick={() => deleteIdentity(currentIdentity)}
          disabled={loadingIdentities || !currentIdentity}
        >
          删除身份
        </button>
      </div>
    </div>

    <div class="language-selector">
      <label for="lang-select">{$_("language")}:</label>
      <select
        id="lang-select"
        bind:value={$locale}
        onchange={(e) => changeLanguage((e.target as HTMLSelectElement).value)}
      >
        {#each languages as lang}
          <option value={lang.code}>{lang.name}</option>
        {/each}
      </select>
    </div>

    <Contactlist {contacts} {selectedId} onselect={select} />

    <div class="add-friend">
      <input
        bind:value={friendInput}
        onkeydown={onKey}
        placeholder={$_("add_friend")}
      />
      <button onclick={addFriend} disabled={!friendInput.trim()}>+</button>
    </div>
  </aside>

  <div
    class="resizer-v"
    style="left: {sidebarW}px"
    onmousedown={() => startDrag(true)}
    role="button"
    aria-label="调整侧边栏宽度"
    tabindex="0"
  ></div>

  <div class="main">
    <a class="about" href="./about">{$_("about")}</a>

    <div class="chat" style="height: calc(100% - {inputH}px)">
      {#if selectedId}
        <Messagelist bind:this={msgListRef} />
      {:else}
        <div class="empty">选择联系人开始聊天</div>
      {/if}
    </div>
    {#if warning}
      <div
        class="toast"
        transition:fly={{
          y: -20,
          duration: 300,
          easing: (t) => Math.sin((t * Math.PI) / 2),
        }}
      >
        {warning}
      </div>
    {/if}
    <div
      class="resizer-h"
      style="top: calc(100% - {inputH}px)"
      onmousedown={() => startDrag(false)}
      role="button"
      aria-label="调整输入框高度"
      tabindex="0"
    ></div>

    <div class="input-box" style="height: {inputH}px">
      <Input
        onsend={send}
        disabled={!selectedId || !currentIdentity}
        peerId={selectedId ?? ""}
        fill
      />
    </div>
  </div>
</main>

<style>
  :global(:root) {
    font-family: system-ui;
    background: #0f0f0f;
    color: #f6f6f6;
  }
  .container {
    display: flex;
    height: 100vh;
    overflow: hidden;
  }

  .sidebar {
    display: flex;
    flex-direction: column;
    background: #0a0a0a;
    border-right: 1px solid #2a2a2a;
  }
  .language-selector {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 12px;
    border-bottom: 1px solid #2a2a2a;
  }
  .language-selector label {
    font-size: 14px;
    color: #fafafa;
  }
  .language-selector select {
    background: #1a1a1a;
    border: 1px solid #2a2a2a;
    border-radius: 6px;
    padding: 4px 8px;
    color: inherit;
    font-size: 14px;
  }
  .add-friend {
    display: flex;
    gap: 8px;
    padding: 12px;
    border-top: 1px solid #2a2a2a;
  }
  .add-friend input {
    flex: 1;
    background: #1a1a1a;
    border: 1px solid #2a2a2a;
    border-radius: 6px;
    padding: 8px;
    color: inherit;
  }
  .add-friend button {
    width: 32px;
    background: #3b82f6;
    border: none;
    border-radius: 6px;
    color: white;
    cursor: pointer;
  }
  .add-friend button:disabled {
    background: #2a2a2a;
    cursor: not-allowed;
  }
  .toast {
    position: fixed;
    top: 50%;
    left: 50%;
    transform: translate(-50%, -50%);
    background: rgba(255, 68, 68, 0.95);
    color: white;
    padding: 12px 20px;
    border-radius: 40px;
    box-shadow: 0 4px 12px rgba(0, 0, 0, 0.15);
    z-index: 1000;
    font-weight: 500;
    font-size: 14px;
    text-align: center;
    max-width: 80vw;
    word-break: break-word;
    backdrop-filter: blur(4px);
    animation: fadeOut 4s ease forwards;
  }

  @keyframes fadeOut {
    0% {
      opacity: 0;
      transform: translate(-50%, -50%) scale(0.9);
    }
    15% {
      opacity: 1;
      transform: translate(-50%, -50%) scale(1);
    }
    85% {
      opacity: 1;
      transform: translate(-50%, -50%) scale(1);
    }
    100% {
      opacity: 0;
      transform: translate(-50%, -50%) scale(0.9);
      visibility: hidden;
    }
  }
  .resizer-v,
  .resizer-h {
    position: absolute;
    background: #2a2a2a;
    z-index: 10;
  }
  .resizer-v {
    width: 6px;
    height: 100%;
    cursor: col-resize;
  }
  .resizer-h {
    height: 6px;
    width: 100%;
    cursor: row-resize;
  }
  .resizer-v:hover,
  .resizer-h:hover {
    background: #3b82f6;
  }

  .main {
    flex: 1;
    display: flex;
    flex-direction: column;
    position: relative;
  }
  .about {
    position: absolute;
    top: 16px;
    right: 16px;
    color: #737373;
    text-decoration: none;
    font-size: 14px;
  }
  .about:hover {
    color: #3b82f6;
  }

  .chat {
    flex: 1;
    overflow: hidden;
  }
  .empty {
    display: grid;
    place-items: center;
    height: 100%;
    color: #525252;
  }
  .input-box {
    border-top: 1px solid #2a2a2a;
    background: #1a1a1a;
  }
  .identity-panel {
    padding: 12px;
    border-bottom: 1px solid #2a2a2a;
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .current-identity-display {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 8px 12px;
    background: #000000;
    border-radius: 6px;
    cursor: pointer;
    transition: background-color 0.2s;
    /* Reset button default styles */
    border: none;
    outline: none;
    font: inherit;
    color: inherit;
    width: 100%;
    text-align: left;
  }
  .current-identity-display:hover {
    background: #1a1a1a;
  }
  .current-identity-display:focus-visible {
    outline: 2px solid #3b82f6;
    outline-offset: 2px;
  }
  .current-label {
    font-size: 13px;
    color: #fafafa;
    white-space: nowrap;
    font-weight: 500;
  }
  .peerid-badge {
    flex: 1;
    font-family: 'Courier New', monospace;
    font-size: 11px;
    color: #ffffff;
    background: #000000;
    padding: 4px 8px;
    border-radius: 4px;
    word-break: break-all;
    text-align: right;
    border: 1px solid #333333;
  }
  .identity-control {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .identity-control label {
    font-size: 14px;
    color: #fafafa;
    white-space: nowrap;
  }
  .identity-control select {
    flex: 1;
    background: #1a1a1a;
    border: 1px solid #2a2a2a;
    border-radius: 6px;
    padding: 4px 8px;
    color: inherit;
    font-size: 14px;
    min-width: 0;
  }
  .identity-actions {
    display: flex;
    gap: 8px;
  }
  .identity-btn {
    flex: 1;
    padding: 6px 12px;
    background: #1a1a1a;
    border: 1px solid #2a2a2a;
    border-radius: 6px;
    color: #fafafa;
    cursor: pointer;
    font-size: 12px;
    transition: background-color 0.2s;
  }
  .identity-btn:hover:not(:disabled) {
    background: #2a2a2a;
  }
  .identity-btn:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }
  .identity-btn.delete {
    border-color: #ef4444;
    color: #ef4444;
  }
  .identity-btn.delete:hover:not(:disabled) {
    background: rgba(239, 68, 68, 0.1);
  }
</style>
