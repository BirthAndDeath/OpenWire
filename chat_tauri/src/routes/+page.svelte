<script lang="ts">
  import "../lib/i18n";
  import { _, locale } from "svelte-i18n";
  import { listen } from "@tauri-apps/api/event";
  import { invoke } from "@tauri-apps/api/core";
  import { onMount } from "svelte";
  import { goto } from "$app/navigation";
  import Input from "./Input.svelte";
  import Messagelist from "./Messagelist.svelte";
  import Contactlist from "./Contactlist.svelte";
  import Toast from "./Toast.svelte";

  // 主题和语言在layout.svelte 中统一初始化

  let warning = $state<string>("");
  // 显示 warning 的统一函数
  const showWarning = (message: string, duration: number = 5000) => {
    warning = message;
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

  onMount(() => {
    let unlisten: (() => void) | undefined;

    (async () => {
      unlisten = await listen<string>("warning", (e) => {
        showWarning(e.payload, 5000);
        console.warn("Received warning from backend:", e.payload);
      });
    })();

    loadContacts();

    return () => {
      unlisten?.();
    };
  });

  // === 状态 ===
  let msgListRef = $state<ReturnType<typeof Messagelist>>();
  let selectedId = $state<string | null>(null);
  let sidebarW = $state(300);
  let inputH = $state(150);

  // 添加好友相关状态
  let showAddFriendModal = $state(false);
  let newFriendPubkeyIdentityId = $state(""); // ML-KEM公钥的hex编码
  let newFriendName = $state("");

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

  const addFriend = async () => {
    if (!newFriendPubkeyIdentityId.trim()) {
      showWarning("请输入 Pubkey 身份 ID (ML-KEM公钥的hex编码)");
      return;
    }

    try {
      // 验证hex格式
      const hex = newFriendPubkeyIdentityId.replace(/\s/g, "");
      if (hex.length % 2 !== 0) {
        showWarning("Pubkey身份ID格式无效：长度必须是偶数");
        return;
      }

      // 验证hex字符
      if (!/^[0-9a-fA-F]+$/.test(hex)) {
        showWarning("Pubkey身份ID格式无效：只能包含0-9, a-f, A-F字符");
        return;
      }

      const success: boolean = await invoke("add_contact", {
        pubkey_identity_id: hex,
        name: newFriendName.trim() || undefined,
      });

      if (success) {
        showWarning("好友添加成功！");
        showAddFriendModal = false;
        newFriendPubkeyIdentityId = "";
        newFriendName = "";
        await loadContacts();
      }
    } catch (e) {
      showWarning(`添加好友失败：${e}`);
    }
  };

  // 跳转到设置页面
  function goToSettings() {
    goto("/settings");
  }

  // 跳转到身份管理页面
  function goToIdentity() {
    goto("/identity");
  }
</script>

<main class="container">
  <aside class="sidebar" style="width: {sidebarW}px">
    <!-- 设置按钮 -->
    <div class="icon-buttons-container">
      <button
        class="icon-btn"
        onclick={goToSettings}
        title={$_("settings")}
        aria-label="打开设置"
      >
        <svg
          xmlns="http://www.w3.org/2000/svg"
          width="24"
          height="24"
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          stroke-width="2"
          stroke-linecap="round"
          stroke-linejoin="round"
        >
          <circle cx="12" cy="12" r="3"></circle>
          <path
            d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 0 1 0 2.83 2 2 0 0 1-2.83 0l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 0 1-2 2 2 2 0 0 1-2-2v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 0 1 0-2.83 2 2 0 0 1 2.83 0l.06.06a1.65 1.65 0 0 0 1.82.33H9a1.65 1.65 0 0 0 1-1.51V3a2 2 0 0 1 2-2 2 2 0 0 1 2 2v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 0 1 2.83 0 2 2 0 0 1 0 2.83l-.06.06a1.65 1.65 0 0 0-.33 1.82V9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 0 1 2 2 2 2 0 0 1-2 2h-.09a1.65 1.65 0 0 0-1.51 1z"
          ></path>
        </svg>
      </button>

      <button
        class="icon-btn"
        onclick={goToIdentity}
        title={$_("identity_management")}
        aria-label="身份管理"
      >
        <svg
          xmlns="http://www.w3.org/2000/svg"
          width="24"
          height="24"
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          stroke-width="2"
          stroke-linecap="round"
          stroke-linejoin="round"
        >
          <path d="M20 21v-2a4 4 0 0 0-4-4H6a4 4 0 0 0-4 4v2"></path>
          <circle cx="12" cy="7" r="4"></circle>
        </svg>
      </button>
    </div>

    <Contactlist {contacts} {selectedId} onselect={select} />

    <!-- 添加好友按钮 -->
    <div class="add-friend-wrapper">
      <button
        class="add-friend-btn"
        onclick={() => (showAddFriendModal = true)}
        aria-label={$_("add_friend")}
      >
        <svg
          width="20"
          height="20"
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          stroke-width="2"
        >
          <path d="M16 21v-2a4 4 0 0 0-4-4H6a4 4 0 0 0-4 4v2" />
          <circle cx="9" cy="7" r="4" />
          <line x1="19" y1="8" x2="19" y2="14" />
          <line x1="22" y1="11" x2="16" y2="11" />
        </svg>
        <span>{$_("add_friend")}</span>
      </button>
    </div>
  </aside>

  <!-- 添加好友模态框 -->
  {#if showAddFriendModal}
    <div
      class="modal-overlay"
      onclick={(e) =>
        e.target === e.currentTarget && (showAddFriendModal = false)}
      onkeydown={(e) => e.key === "Escape" && (showAddFriendModal = false)}
      role="dialog"
      aria-label="添加好友对话框"
      tabindex="0"
    >
      <div class="modal-content" role="document" aria-label="添加好友表单">
        <h3>{$_("add_friend")}</h3>
        <div class="form-group">
          <label for="pubkey-identity-id">Pubkey 身份 ID *</label>
          <input
            id="pubkey-identity-id"
            type="text"
            placeholder="ML-KEM公钥的hex编码 (例如: 1234abcd...)"
            bind:value={newFriendPubkeyIdentityId}
          />
          <small>ML-KEM公钥的十六进制编码，作为唯一身份标识</small>
        </div>
        <div class="form-group">
          <label for="friend-name">{$_("name")} (可选)</label>
          <input
            id="friend-name"
            type="text"
            placeholder="好友姓名"
            bind:value={newFriendName}
          />
        </div>
        <div class="modal-actions">
          <button
            type="button"
            class="btn-cancel"
            onclick={() => (showAddFriendModal = false)}
          >
            取消
          </button>
          <button type="button" class="btn-confirm" onclick={addFriend}>
            添加好友
          </button>
        </div>
      </div>
    </div>
  {/if}

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
        <div class="empty">{$_("select_contact_to_chat")}</div>
      {/if}
    </div>

    <Toast message={warning || ""} />

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
        disabled={!selectedId}
        peerId={selectedId ?? ""}
        fill
      />
    </div>
  </div>
</main>

<style>
  :global(:root) {
    font-family: system-ui;
  }

  /* 暗色主题（默认） */
  :global([data-theme="dark"]) {
    --bg-primary: #0f0f0f;
    --bg-secondary: #0a0a0a;
    --bg-tertiary: #1a1a1a;
    --text-primary: #f6f6f6;
    --text-secondary: #737373;
    --border-color: #2a2a2a;
  }

  /* 亮色主题 */
  :global([data-theme="light"]) {
    --bg-primary: #ffffff;
    --bg-secondary: #f5f5f5;
    --bg-tertiary: #fafafa;
    --text-primary: #1a1a1a;
    --text-secondary: #666666;
    --border-color: #e0e0e0;
  }

  .container {
    display: flex;
    height: 100vh;
    overflow: hidden;
    background: var(--bg-primary);
    color: var(--text-primary);
  }

  .sidebar {
    display: flex;
    flex-direction: column;
    background: var(--bg-secondary);
    border-right: 1px solid var(--border-color);
  }

  .icon-buttons-container {
    padding: 12px;
    border-bottom: 1px solid var(--border-color);
    display: flex;
    gap: 8px;
  }

  .icon-btn {
    display: flex;
    align-items: center;
    justify-content: center;
    width: 40px;
    height: 40px;
    background: var(--bg-tertiary);
    border: 1px solid var(--border-color);
    border-radius: 8px;
    color: var(--text-primary);
    cursor: pointer;
    transition: all 0.2s;
    padding: 0;
  }

  .icon-btn:hover {
    background: var(--border-color);
    border-color: #3b82f6;
    color: #3b82f6;
  }

  .icon-btn:hover svg {
    transform: rotate(30deg);
  }

  .icon-btn svg {
    transition: transform 0.3s ease;
  }

  .icon-btn:active svg {
    transform: rotate(30deg) scale(0.95);
  }

  .resizer-v,
  .resizer-h {
    position: absolute;
    background: var(--border-color);
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
    color: var(--text-secondary);
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
    color: var(--text-secondary);
  }
  .input-box {
    border-top: 1px solid var(--border-color);
    background: var(--bg-tertiary);
  }

  /* 添加好友按钮样式 */
  .add-friend-wrapper {
    padding: 12px;
    border-top: 1px solid var(--border-color);
    margin-top: auto;
  }

  .add-friend-btn {
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 8px;
    width: 100%;
    padding: 10px;
    background: var(--bg-tertiary);
    border: 1px solid var(--border-color);
    border-radius: 8px;
    color: var(--text-primary);
    cursor: pointer;
    transition: all 0.2s;
    font-size: 14px;
  }

  .add-friend-btn:hover {
    background: var(--border-color);
    color: #3b82f6;
    border-color: #3b82f6;
  }

  /* 模态框样式 */
  .modal-overlay {
    position: fixed;
    top: 0;
    left: 0;
    width: 100%;
    height: 100%;
    background: rgba(0, 0, 0, 0.5);
    display: flex;
    justify-content: center;
    align-items: center;
    z-index: 1000;
  }

  .modal-content {
    background: var(--bg-secondary);
    padding: 24px;
    border-radius: 12px;
    width: 90%;
    max-width: 400px;
    box-shadow: 0 4px 20px rgba(0, 0, 0, 0.3);
    border: 1px solid var(--border-color);
  }

  .modal-content h3 {
    margin-top: 0;
    margin-bottom: 20px;
    color: var(--text-primary);
  }

  .form-group {
    margin-bottom: 16px;
  }

  .form-group label {
    display: block;
    margin-bottom: 6px;
    color: var(--text-secondary);
    font-size: 14px;
  }

  .form-group input,
  .form-group textarea {
    width: 100%;
    padding: 10px;
    background: var(--bg-primary);
    border: 1px solid var(--border-color);
    border-radius: 6px;
    color: var(--text-primary);
    font-family: inherit;
    box-sizing: border-box;
  }

  .form-group input:focus,
  .form-group textarea:focus {
    outline: none;
    border-color: #3b82f6;
  }

  .modal-actions {
    display: flex;
    justify-content: flex-end;
    gap: 12px;
    margin-top: 24px;
  }

  .btn-cancel,
  .btn-confirm {
    padding: 8px 16px;
    border-radius: 6px;
    cursor: pointer;
    font-size: 14px;
    transition: all 0.2s;
  }

  .btn-cancel {
    background: transparent;
    border: 1px solid var(--border-color);
    color: var(--text-secondary);
  }

  .btn-cancel:hover {
    background: var(--bg-tertiary);
    color: var(--text-primary);
  }

  .btn-confirm {
    background: #3b82f6;
    border: 1px solid #3b82f6;
    color: white;
  }

  .btn-confirm:hover {
    background: #2563eb;
  }
</style>
