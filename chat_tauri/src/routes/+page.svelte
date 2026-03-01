<script lang="ts">
  import "../lib/i18n";
  import { _ } from "svelte-i18n";
  import { listen } from "@tauri-apps/api/event";
  import { onMount } from "svelte";
  import Input from "./Input.svelte";
  import Messagelist from "./Messagelist.svelte";
  import Contactlist from "./Contactlist.svelte";

  // === 组件引用 ===
  let msgListRef = $state<ReturnType<typeof Messagelist> | undefined>(
    undefined,
  );
  let contactListRef: ReturnType<typeof Contactlist>;

  // === 状态 ===
  let selectedContactId = $state<string | null>(null);
  let contacts = $state([
    {
      id: "1",
      name: "Alice",
      lastMessage: "好的，明天见",
      lastTime: Date.now(),
      unread: 2,
      isOnline: true,
    },
    {
      id: "2",
      name: "Bob",
      lastMessage: "收到",
      lastTime: Date.now() - 86400000,
      unread: 0,
      isOnline: false,
    },
    {
      id: "3",
      name: "Charlie",
      lastMessage: "在吗？",
      lastTime: Date.now() - 172800000,
      unread: 5,
      isOnline: true,
    },
  ]);

  // === 新增状态：分隔条位置 ===
  let sidebarWidth = $state(300);
  let inputHeight = $state(150);

  // === 新增：添加好友状态 ===
  let friendInputValue = $state("");

  // === 监听远程消息 ===
  onMount(() => {
    let unlisten: (() => void) | undefined;
    (async () => {
      unlisten = await listen<string>("chat-message", (e) => {
        // TODO: 根据 sender_id 路由到对应会话
        msgListRef?.add(e.payload, false);

        // 更新联系人列表的最后消息
        contacts = contacts.map((c) =>
          c.id === selectedContactId
            ? { ...c, lastMessage: e.payload, lastTime: Date.now() }
            : c,
        );
      });
    })();
    return () => unlisten?.();
  });

  // === 处理拖动事件 ===
  function handleDrag(event: MouseEvent, isSidebar: boolean) {
    if (isSidebar) {
      sidebarWidth = Math.max(200, Math.min(500, event.clientX));
    } else {
      const windowHeight = window.innerHeight;
      inputHeight = Math.max(
        100,
        Math.min(300, windowHeight - event.clientY - 16),
      );
    }
  }

  // === 简化拖动处理 ===
  function startDrag(isSidebar: boolean) {
    const handleMouseMove = (e: MouseEvent) => handleDrag(e, isSidebar);
    const handleMouseUp = () => {
      document.removeEventListener("mousemove", handleMouseMove);
      document.removeEventListener("mouseup", handleMouseUp);
    };
    document.addEventListener("mousemove", handleMouseMove);
    document.addEventListener("mouseup", handleMouseUp);
  }

  // === 简化选择联系人处理 ===
  function handleSelectContact(id: string) {
    selectedContactId = id;
    // TODO: 加载该联系人的消息历史
  }

  // === 发送消息 ===
  function handleSend(text: string) {
    if (!selectedContactId) return;
    msgListRef?.add(text, true);

    // 更新联系人列表
    contacts = contacts.map((c) =>
      c.id === selectedContactId
        ? { ...c, lastMessage: text, lastTime: Date.now() }
        : c,
    );
  }

  // === 新增：添加好友 ===
  function handleAddFriend() {
    const pubkey = friendInputValue.trim();
    if (!pubkey) return;

    // TODO: 调用 Rust 验证公钥并创建会话
    console.log("Adding friend:", pubkey);

    // 临时模拟添加成功
    const newContact = {
      id: crypto.randomUUID(),
      name: `User ${pubkey.slice(0, 8)}...`,
      lastMessage: "新朋友",
      lastTime: Date.now(),
      unread: 0,
      isOnline: false,
    };

    contacts = [...contacts, newContact];
    friendInputValue = ""; // 清空输入
  }

  function handleFriendKeydown(e: KeyboardEvent) {
    if (e.key === "Enter") {
      e.preventDefault();
      handleAddFriend();
    }
  }
</script>

<main class="container">
  <!-- 左侧：联系人列表 -->
  <aside class="sidebar" style={`width: ${sidebarWidth}px`}>
    <div class="sidebar-content">
      <Contactlist
        {contacts}
        selectedId={selectedContactId}
        onselect={handleSelectContact}
      ></Contactlist>
    </div>

    <!-- 底部：添加好友输入框 -->
    <div class="add-friend-box">
      <input
        type="text"
        class="friend-input"
        placeholder={$_("add_friend_placeholder") || "输入公钥添加好友..."}
        bind:value={friendInputValue}
        onkeydown={handleFriendKeydown}
      />
      <button
        class="add-btn"
        onclick={handleAddFriend}
        disabled={!friendInputValue.trim()}
        aria-label={$_("add_friend") || "添加好友"}
      >
        <svg
          width="16"
          height="16"
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          stroke-width="2"
        >
          <line x1="12" y1="5" x2="12" y2="19"></line>
          <line x1="5" y1="12" x2="19" y2="12"></line>
        </svg>
      </button>
    </div>
  </aside>

  <!-- 左侧分隔条：使用 slider 角色（交互式） -->
  <div
    class="resize-handle vertical"
    style={`left: ${sidebarWidth}px`}
    role="slider"
    aria-label="侧边栏宽度"
    aria-valuenow={sidebarWidth}
    aria-valuemin={200}
    aria-valuemax={500}
    tabindex="0"
    onmousedown={(e) => {
      e.preventDefault();
      startDrag(true);
    }}
    onkeydown={(e) => {
      if (e.key === "ArrowLeft")
        sidebarWidth = Math.max(200, sidebarWidth - 10);
      if (e.key === "ArrowRight")
        sidebarWidth = Math.min(500, sidebarWidth + 10);
    }}
  ></div>

  <!-- 右侧：聊天区域 -->
  <div class="main-content">
    <!-- 顶部：关于链接 -->
    <a class="about-link" href="./about" title={$_("about")}>{$_("about")}</a>

    <!-- 中部：消息列表 -->
    <div class="chat-area" style={`height: calc(100% - ${inputHeight}px)`}>
      {#if selectedContactId}
        <Messagelist bind:this={msgListRef}></Messagelist>
      {:else}
        <div class="empty-state">
          <svg
            width="64"
            height="64"
            viewBox="0 0 24 24"
            fill="none"
            stroke="#525252"
            stroke-width="1"
          >
            <path
              d="M21 15a2 2 0 0 1-2 2H7l-4 4V5a2 2 0 0 1 2-2h14a2 2 0 0 1 2 2z"
            />
          </svg>
          <p>选择联系人开始聊天</p>
        </div>
      {/if}
    </div>

    <!-- 可拖动分隔条：使用 slider 角色 -->
    <div
      class="resize-handle horizontal"
      style={`top: calc(100% - ${inputHeight}px)`}
      role="slider"
      aria-label="输入框高度"
      aria-valuenow={inputHeight}
      aria-valuemin={100}
      aria-valuemax={300}
      tabindex="0"
      onmousedown={(e) => {
        e.preventDefault();
        startDrag(false);
      }}
      onkeydown={(e) => {
        if (e.key === "ArrowUp") inputHeight = Math.min(300, inputHeight + 10);
        if (e.key === "ArrowDown")
          inputHeight = Math.max(100, inputHeight - 10);
      }}
    ></div>

    <!-- 底部：输入框 -->
    <div class="input-area" style={`height: ${inputHeight}px`}>
      <Input onsend={handleSend} disabled={!selectedContactId} fill={true}
      ></Input>
    </div>
  </div>
</main>

<style>
  :root {
    font-family: Inter, Avenir, Helvetica, Arial, sans-serif;
    font-size: 16px;
    line-height: 24px;
    font-weight: 400;
    color: #f6f6f6;
    background-color: #0f0f0f;
  }

  .container {
    margin: 0;
    display: flex;
    height: 100vh;
    width: 100vw;
    overflow: hidden;
    position: relative;
  }

  /* 左侧边栏：联系人列表 */
  .sidebar {
    flex-shrink: 0;
    border-right: 1px solid #2a2a2a;
    background: #0a0a0a;
    position: relative;
    display: flex;
    flex-direction: column;
  }

  .sidebar-content {
    flex: 1;
    overflow: hidden;
    display: flex;
    flex-direction: column;
  }

  /* 添加好友区域 */
  .add-friend-box {
    padding: 12px;
    border-top: 1px solid #2a2a2a;
    background: #141414;
    display: flex;
    gap: 8px;
    align-items: center;
  }

  .friend-input {
    flex: 1;
    background: #1a1a1a;
    border: 1px solid #2a2a2a;
    border-radius: 6px;
    padding: 8px 12px;
    color: #f6f6f6;
    font-size: 13px;
    outline: none;
    transition: border-color 0.2s;
  }

  .friend-input:focus {
    border-color: #3b82f6;
  }

  .friend-input::placeholder {
    color: #525252;
  }

  .add-btn {
    width: 32px;
    height: 32px;
    display: flex;
    align-items: center;
    justify-content: center;
    background: #3b82f6;
    border: none;
    border-radius: 6px;
    color: white;
    cursor: pointer;
    transition: background 0.2s;
    flex-shrink: 0;
  }

  .add-btn:hover:not(:disabled) {
    background: #2563eb;
  }

  .add-btn:disabled {
    background: #2a2a2a;
    color: #525252;
    cursor: not-allowed;
  }

  .resize-handle {
    position: absolute;
    background: rgba(42, 42, 42, 0.7);
    z-index: 10;
    transition: background 0.2s ease;
    outline: none;
  }

  .resize-handle:focus {
    background: #3b82f6;
  }

  .resize-handle.vertical {
    width: 6px;
    height: 100%;
    cursor: col-resize;
    top: 0;
  }

  .resize-handle.horizontal {
    height: 6px;
    width: 100%;
    cursor: row-resize;
    left: 0;
  }

  .resize-handle:hover {
    background: #3b82f6;
  }

  .resize-handle:active {
    background: #2563eb;
  }

  /* 右侧主内容区 */
  .main-content {
    flex: 1;
    display: flex;
    flex-direction: column;
    position: relative;
    width: calc(100% - var(--sidebar-width, 300px) - 6px);
  }

  /* 关于链接 */
  .about-link {
    position: absolute;
    top: 16px;
    right: 16px;
    color: #737373;
    text-decoration: none;
    font-size: 14px;
    z-index: 10;
  }
  .about-link:hover {
    color: #3b82f6;
  }

  /* 消息列表区域 */
  .chat-area {
    flex: 1;
    overflow: hidden;
    position: relative;
  }

  /* 空状态 */
  .empty-state {
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    height: 100%;
    color: #525252;
    gap: 16px;
  }
  .empty-state p {
    font-size: 14px;
  }

  /* 输入框区域 */
  .input-area {
    border-top: 1px solid #2a2a2a;
    background: #1a1a1a;
    display: flex;
    align-items: center;
  }

  /* 响应式：小屏幕隐藏侧边栏 */
  @media (max-width: 768px) {
    .sidebar {
      position: absolute;
      z-index: 100;
      height: 100%;
      transform: translateX(-100%);
      transition: transform 0.3s;
    }

    .main-content {
      width: 100%;
    }
  }
</style>
