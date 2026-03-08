<script lang="ts">
  import "../lib/i18n";
  import { _ } from "svelte-i18n";
  import { listen } from "@tauri-apps/api/event";
  import { onMount } from "svelte";
  import Input from "./Input.svelte";
  import Messagelist from "./Messagelist.svelte";
  import Contactlist from "./Contactlist.svelte";

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

    <div
      class="resizer-h"
      style="top: calc(100% - {inputH}px)"
      onmousedown={() => startDrag(false)}
      role="button"
      aria-label="调整输入框高度"
      tabindex="0"
    ></div>

    <div class="input-box" style="height: {inputH}px">
      <Input onsend={send} disabled={!selectedId} fill />
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
</style>
