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
  import NotificationBadge from "./NotificationBadge.svelte";
  import AddFriendModal from "./AddFriendModal.svelte";
  import SentFileManager from "./SentFileManager.svelte";
  import ResizablePanel from "$lib/components/ResizablePanel.svelte";
  interface IdentityDto {
    id: number;
    identity_id: string;
    is_current: boolean;
    mlkem_pubkey_hex: string | null;
  }

  // 响应式侧边栏可见性
  let sidebarVisible = $state(true);
  let isMobile = $state(false);
  $effect(() => {
    const mq = window.matchMedia(`(max-width: 520px)`);
    const handler = (e: MediaQueryListEvent | MediaQueryList) => {
      isMobile = e.matches;
      if (e.matches) sidebarVisible = false;
      else sidebarVisible = true;
    };
    handler(mq);
    mq.addEventListener('change', handler);
    return () => mq.removeEventListener('change', handler);
  });

  // 侧边栏宽度：与窗口宽度按比例缩放
  let sidebarW = $state(Math.round(window.innerWidth * 0.28));
  let lastWindowW = $state(window.innerWidth);
  $effect(() => {
    const onResize = () => {
      const ratio = sidebarW / lastWindowW;
      lastWindowW = window.innerWidth;
      sidebarW = Math.round(window.innerWidth * ratio);
    };
    window.addEventListener('resize', onResize);
    return () => window.removeEventListener('resize', onResize);
  });

  // 移动端：选择联系人后自动关闭侧边栏，显示聊天界面
  function selectMobile(id: string) {
    selectedId = id;
    if (isMobile) sidebarVisible = false;
  }

  // 移动端：返回联系人列表
  function backToContacts() {
    selectedId = null;
    sidebarVisible = true;
  }

  // 主题和语言在layout.svelte 中统一初始化

  // 格式化文件大小
  function formatFileSize(bytes: number): string {
    if (bytes === 0) return "0 B";
    const units = ["B", "KB", "MB", "GB"];
    const i = Math.floor(Math.log(bytes) / Math.log(1024));
    return (bytes / Math.pow(1024, i)).toFixed(1) + " " + units[i];
  }

  let warning = $state<string>("");
  let onNotif: ((msg: string) => void) | undefined = $state();
  let notifBuffer: string[] = [];

  const showWarning = (message: string, duration: number = 5000) => {
    warning = message;
    if (onNotif) {
      onNotif(message);
    } else {
      notifBuffer.push(message);
    }
    console.warn("Warning:", message);
  };

  // 当 onNotif 就绪时，flush 缓冲队列
  $effect(() => {
    if (onNotif) {
      for (const msg of notifBuffer) {
        onNotif(msg);
      }
      notifBuffer = [];
    }
  });

  // 语言选项
  const languages = [
    { code: "en", name: "English" },
    { code: "zh", name: "中文" },
    { code: "fr", name: "Français" },
    { code: "es", name: "Español" },
    { code: "de", name: "Deutsch" },
    { code: "ja", name: "日本語" },
  ];

  let currentIdentityId = $state<string>("");

  // 获取当前身份 ID，用于检查是否添加自己
  const loadCurrentIdentity = async () => {
    try {
      const identities: IdentityDto[] = await invoke("list_identities");
      const current = identities.find((id) => id.is_current);
      currentIdentityId = current?.identity_id ?? "";
    } catch {
      currentIdentityId = "";
    }
  };

  interface ContactDto {
    mldsa_pubkey_hex: string;
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
        pubkey_hex: c.mldsa_pubkey_hex,
        name: c.name,
        lastMsg: "最近聊天",
        lastTime: c.added_at * 1000,
        unread: 0,
        online: false,
      }));
      if (!selectedId && contacts.length) selectedId = contacts[0].pubkey_hex;
    } catch (e) {
      showWarning(`加载联系人失败：${e}`);
    } finally {
      loadingContacts = false;
    }
  };

  // === 状态 ===
  let msgListRef = $state<ReturnType<typeof Messagelist>>();
  let selectedId = $state<string | null>(null);
  let inputH = $state(150);

  // 添加好友相关状态
  let showAddFriendModal = $state(false);

  // 已发送文件管理
  let showSentFiles = $state(false);

  let contacts = $state<
    {
      order: number;
      pubkey_hex: string;
      name: string;
      lastMsg: string;
      lastTime: number;
      unread: number;
      online: boolean;
    }[]
  >([]);

  // === 核心是否已就绪（后端核心初始化完成） ===
  let coreReady = $state(false);
  let coreInitError = $state<string | null>(null);

  // === 监听消息和事件 ===
  onMount(() => {
    let unlistenWarning: (() => void) | undefined;
  let unlistenCoreInitFailed: (() => void) | undefined;
    let unlistenMessage: (() => void) | undefined;
    let unlistenFileProgress: (() => void) | undefined;
    let unlistenCoreReady: (() => void) | undefined;
    let unlistenDeliveryReceipt: (() => void) | undefined;
    let unlistenOnlineStatus: (() => void) | undefined;
    let unlistenMessageSent: (() => void) | undefined;
    let pollingTimer: ReturnType<typeof setInterval> | undefined;

    (async () => {
      // 监听 core-ready 事件（核心初始化完成后由后端发送）
      // 注意：Tauri 的 emit 是 fire-and-forget，如果前端尚未注册 listener，
      // 事件可能丢失。因此同时使用 check_core_ready 命令轮询作为可靠兜底。
unlistenCoreReady = await listen<boolean>("core-ready", () => {
        coreReady = true;
        loadContacts();
        loadCurrentIdentity();
      });

      unlistenCoreInitFailed = await listen<string>("core-init-failed", (e) => {
        coreInitError = e.payload;
      });

      unlistenWarning = await listen<string>("warning", (e) => {
        showWarning(e.payload, 5000);
        console.warn("Received warning from backend:", e.payload);
      });

      // 监听送达回执事件，将 pending 消息标记为已送达
      unlistenDeliveryReceipt = await listen<string>(
        "delivery-receipt",
        (e) => {
          msgListRef?.markSent(e.payload);
        },
      );

      unlistenMessage = await listen<string>("chat-message", (e) => {
        // 尝试解析结构化 JSON 消息（FileHash 类型）
        let displayText = e.payload;
        let msgType: "text" | "file_hash" | "file_stream" = "text";
        let fileHashInfo: any = undefined;
        let senderPubkey: string | undefined = undefined;

        try {
          const parsed = JSON.parse(e.payload);
          if (parsed.type === "file_hash") {
            msgType = "file_hash";
            displayText = `文件分享: ${parsed.filename} (${formatFileSize(parsed.total_size)})`;
            fileHashInfo = {
              filename: parsed.filename,
              total_size: parsed.total_size,
              file_hash: parsed.file_hash,
              file_id: parsed.file_id,
            };
            senderPubkey = parsed.sender;
          } else if (parsed.type === "text" && parsed.sender) {
            // 结构化文本消息，包含发送方信息
            displayText = parsed.text || displayText;
            senderPubkey = parsed.sender;
          }
        } catch {
          // 不是 JSON，作为普通文本消息处理
        }

        // 将消息添加到消息列表（传入 senderPubkey 作为 mldsa_pubkey_hex 用于过滤）
        msgListRef?.add(
          displayText,
          false,
          msgType,
          fileHashInfo,
          senderPubkey,
          senderPubkey, // mldsa_pubkey_hex 参数，用于按联系人过滤
        );

        // 更新联系人列表中的最后一条消息
        contacts = contacts.map((c) =>
          c.pubkey_hex === senderPubkey || c.pubkey_hex === selectedId
            ? { ...c, lastMsg: displayText, lastTime: Date.now() }
            : c,
        );
      });

      // 监听在线状态事件：更新每个联系人的 online 字段
      unlistenOnlineStatus = await listen<string[]>("online-status", (e) => {
        const onlinePubkeys = new Set(e.payload);
        contacts = contacts.map((c) => ({
          ...c,
          online: onlinePubkeys.has(c.pubkey_hex),
        }));
      });

      // 监听消息已发送事件：更新对应消息的 message_hash 字段
      // 这样后续送达回执能通过 message_hash 精确匹配
      unlistenMessageSent = await listen<string>("message-sent", (e) => {
        try {
          const payload = JSON.parse(e.payload);
          const messageHash = payload.message_hash;
          const peerId = payload.peer_id;
          if (messageHash) {
            msgListRef?.updateMessageHash(messageHash);
          }
        } catch (err) {
          console.error("解析 message-sent 事件失败:", err);
        }
      });

      // 监听文件传输进度事件
      unlistenFileProgress = await listen<string>(
        "file-transfer-progress",
        (e) => {
          try {
            const progress = JSON.parse(e.payload);
            msgListRef?.updateFileProgress(progress);
          } catch (err) {
            console.error("解析文件传输进度失败:", err);
          }
        },
      );
    })();

    // 轮询检查 Core 就绪状态（兜底机制）
    // 由于 Tauri 的 emit 是 fire-and-forget，如果前端尚未注册 listener，
    // core-ready 事件会丢失。通过轮询 check_core_ready 命令可靠检测。
    pollingTimer = setInterval(async () => {
      if (coreReady) {
        clearInterval(pollingTimer);
        return;
      }
      try {
        await invoke("check_core_ready");
        coreReady = true;
        clearInterval(pollingTimer);
        loadContacts();
        loadCurrentIdentity();
      } catch {
      }
    }, 200);

    return () => {
      if (pollingTimer) clearInterval(pollingTimer);
      unlistenWarning?.();
      unlistenCoreInitFailed?.();
      unlistenMessage?.();
      unlistenFileProgress?.();
      unlistenCoreReady?.();
      unlistenDeliveryReceipt?.();
      unlistenOnlineStatus?.();
      unlistenMessageSent?.();
    };
  });

  // === 拖动调整（使用 ResizablePanel 组件） ===

  // === 操作 ===
  const select = (id: string) => (selectedId = id);

  const send = async (text: string) => {
    if (!selectedId) return;
    // 添加消息到列表，标记为 pending（待确认送达）
    msgListRef?.add(text, true, "text", undefined, undefined, undefined, true);
    contacts = contacts.map((c) =>
      c.pubkey_hex === selectedId
        ? { ...c, lastMsg: text, lastTime: Date.now() }
        : c,
    );
    // 同时调用后端发送消息
    try {
      await invoke("send", { mldsaPubkeyHex: selectedId, message: text });
    } catch (e) {
      showWarning(`发送消息失败：${e}`);
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

  function openSentFiles() {
    showSentFiles = true;
  }
</script>

<main class="container" class:mobile={isMobile} class:sidebar-open={sidebarVisible && isMobile} style={!isMobile ? `grid-template-columns: ${sidebarW}px 1fr` : ''}>
  <!-- 移动端侧边栏遮罩层 -->
  {#if isMobile && sidebarVisible}
    <div class="mobile-overlay" onclick={() => (sidebarVisible = false)} role="presentation" aria-hidden="true"></div>
  {/if}

  <!-- 桌面端：侧边栏作为可拖拽面板 -->
  {#if !isMobile}
    <ResizablePanel
      min={200}
      max={500}
      defaultSize={300}
      bind:size={sidebarW}
      position="left"
    >
      <aside class="sidebar">
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
              ><path
                fill="currentColor"
                d="M10.825 22q-.675 0-1.162-.45t-.588-1.1L8.85 18.8q-.325-.125-.612-.3t-.563-.375l-1.55.65q-.625.275-1.25.05t-.975-.8l-1.175-2.05q-.35-.575-.2-1.225t.675-1.075l1.325-1Q4.5 12.5 4.5 12.337v-.675q0-.162.025-.337l-1.325-1Q2.675 9.9 2.525 9.25t.2-1.225L3.9 5.975q.35-.575.975-.8t1.25.05l1.55.65q.275-.2.575-.375t.6-.3l.225-1.65q.1-.65.588-1.1T10.825 2h2.35q.675 0 1.163.45t.587 1.1l.225 1.65q.325.125.613.3t.562.375l1.55-.65q.625-.275 1.25-.05t.975.8l1.175 2.05q.35.575.2 1.225t-.675 1.075l-1.325 1q.025.175.025.338v.674q0 .163-.05.338l1.325 1q.525.425.675 1.075t-.2 1.225l-1.2 2.05q-.35.575-.975.8t-1.25-.05l-1.5-.65q-.275.2-.575.375t-.6.3l-.225 1.65q-.1.65-.587 1.1t-1.163.45zm1.225-6.5q1.45 0 2.475-1.025T15.55 12t-1.025-2.475T12.05 8.5q-1.475 0-2.488 1.025T8.55 12t1.013 2.475T12.05 15.5"
              /></svg
            >
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
              ><path
                fill="currentColor"
                d="M10.95 19.55q.5.3 1.05.288t1.05-.313l4.55-2.775q-1.25-.875-2.675-1.312T12 15t-2.937.438t-2.713 1.287zm3.525-7.575Q15.5 10.95 15.5 9.5t-1.025-2.475T12 6T9.525 7.025T8.5 9.5t1.025 2.475T12 13t2.475-1.025m-3.525 9.9l-7-4.3q-.45-.275-.7-.725T3 15.875v-7.75q0-.525.25-.975t.7-.725l7-4.3q.5-.3 1.05-.3t1.05.3l7 4.3q.45.275.7.725t.25.975v7.75q0 .525-.25.975t-.7.725l-7 4.3q-.5.3-1.05.3t-1.05-.3"
              /></svg
            >
          </button>

          <button
            class="icon-btn"
            onclick={openSentFiles}
            title={$_("sent_files")}
            aria-label="已发送文件"
          >
            <svg
              xmlns="http://www.w3.org/2000/svg"
              width="24"
              height="24"
              viewBox="0 0 24 24"
              ><path
                fill="currentColor"
                d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8zM6 20V4h7v5h5v11z"
              /><path fill="currentColor" d="M8 12h8v2H8zm0 4h5v2H8zm0-8h3v2H8z"/></svg
            >
          </button>
        </div>

        <Contactlist
          {contacts}
          {selectedId}
          onselect={select}
          ondelete={(id) => {
            contacts = contacts.filter((c) => c.pubkey_hex !== id);
            if (selectedId === id)
              selectedId = contacts.length > 0 ? contacts[0].pubkey_hex : null;
          }}
        />

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
    </ResizablePanel>
  {:else}
    <!-- 移动端：侧边栏以全屏覆盖层方式显示 -->
    <aside class="sidebar sidebar-mobile" class:visible={sidebarVisible}>
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
            ><path
              fill="currentColor"
              d="M10.825 22q-.675 0-1.162-.45t-.588-1.1L8.85 18.8q-.325-.125-.612-.3t-.563-.375l-1.55.65q-.625.275-1.25.05t-.975-.8l-1.175-2.05q-.35-.575-.2-1.225t.675-1.075l1.325-1Q4.5 12.5 4.5 12.337v-.675q0-.162.025-.337l-1.325-1Q2.675 9.9 2.525 9.25t.2-1.225L3.9 5.975q.35-.575.975-.8t1.25.05l1.55.65q.275-.2.575-.375t.6-.3l.225-1.65q.1-.65.588-1.1T10.825 2h2.35q.675 0 1.163.45t.587 1.1l.225 1.65q.325.125.613.3t.562.375l1.55-.65q.625-.275 1.25-.05t.975.8l1.175 2.05q.35.575.2 1.225t-.675 1.075l-1.325 1q.025.175.025.338v.674q0 .163-.05.338l1.325 1q.525.425.675 1.075t-.2 1.225l-1.2 2.05q-.35.575-.975.8t-1.25-.05l-1.5-.65q-.275.2-.575.375t-.6.3l-.225 1.65q-.1.65-.587 1.1t-1.163.45zm1.225-6.5q1.45 0 2.475-1.025T15.55 12t-1.025-2.475T12.05 8.5q-1.475 0-2.488 1.025T8.55 12t1.013 2.475T12.05 15.5"
            /></svg
          >
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
            ><path
              fill="currentColor"
              d="M10.95 19.55q.5.3 1.05.288t1.05-.313l4.55-2.775q-1.25-.875-2.675-1.312T12 15t-2.937.438t-2.713 1.287zm3.525-7.575Q15.5 10.95 15.5 9.5t-1.025-2.475T12 6T9.525 7.025T8.5 9.5t1.025 2.475T12 13t2.475-1.025m-3.525 9.9l-7-4.3q-.45-.275-.7-.725T3 15.875v-7.75q0-.525.25-.975t.7-.725l7-4.3q.5-.3 1.05-.3t1.05.3l7 4.3q.45.275.7.725t.25.975v7.75q0 .525-.25.975t-.7.725l-7 4.3q-.5.3-1.05.3t-1.05-.3"
            /></svg
          >
        </button>

        <button
          class="icon-btn"
          onclick={openSentFiles}
          title={$_("sent_files")}
          aria-label="已发送文件"
        >
          <svg
            xmlns="http://www.w3.org/2000/svg"
            width="24"
            height="24"
            viewBox="0 0 24 24"
            ><path
              fill="currentColor"
              d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8zM6 20V4h7v5h5v11z"
            /><path fill="currentColor" d="M8 12h8v2H8zm0 4h5v2H8zm0-8h3v2H8z"/></svg
          >
        </button>
      </div>

      <Contactlist
        {contacts}
        {selectedId}
        onselect={selectMobile}
        ondelete={(id) => {
          contacts = contacts.filter((c) => c.pubkey_hex !== id);
          if (selectedId === id)
            selectedId = contacts.length > 0 ? contacts[0].pubkey_hex : null;
        }}
      />

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
  {/if}

  <div class="main">
    <!-- 移动端标题栏：返回按钮 + 关于 -->
    <div class="mobile-header" class:visible={isMobile && selectedId != null}>
      <button class="back-btn" onclick={backToContacts} aria-label="返回联系人列表">
        <svg width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
          <polyline points="15 18 9 12 15 6" />
        </svg>
      </button>
    </div>

    <a class="about" href="./about">{$_("about")}</a>

    <div class="chat">
      {#if selectedId}
        <Messagelist bind:this={msgListRef} contactId={selectedId} />
      {:else if !isMobile}
        <div class="empty">{$_("select_contact_to_chat")}</div>
      {/if}
    </div>

    <Toast message={warning || ""} />
    <NotificationBadge onNotification={(cb) => { onNotif = cb; }} />

    <ResizablePanel
      min={80}
      max={300}
      defaultSize={150}
      bind:size={inputH}
      position="bottom"
      mobileHidden={isMobile}
    >
      <div class="input-box">
        <Input
          onsend={send}
          disabled={!selectedId}
          mldsaPubkeyHex={selectedId ?? ""}
          fill
        />
      </div>
    </ResizablePanel>
  </div>
</main>

<!-- 添加好友模态框 -->
<AddFriendModal
  bind:show={showAddFriendModal}
  {currentIdentityId}
  onFriendAdded={loadContacts}
/>

<!-- 已发送文件管理 -->
<SentFileManager bind:show={showSentFiles} />

<style>
  :global(:root) {
    font-family: system-ui;
  }

  /* 暗色主题（默认） */
  :global([data-theme="dark"]) {
    --bg-primary: #0f0f0f;
    --bg-secondary: rgba(10, 10, 10, 0.7);
    --bg-tertiary: rgba(26, 26, 26, 0.6);
    --text-primary: #f6f6f6;
    --text-secondary: #737373;
    --border-color: rgba(42, 42, 42, 0.5);
  }

  /* 亮色主题 */
  :global([data-theme="light"]) {
    --bg-primary: #ffffff;
    --bg-secondary: rgba(245, 245, 245, 0.7);
    --bg-tertiary: rgba(250, 250, 250, 0.6);
    --text-primary: #1a1a1a;
    --text-secondary: #666666;
    --border-color: rgba(224, 224, 224, 0.5);
  }

  .container {
    display: grid;
    grid-template-columns: var(--sidebar-default) 1fr;
    grid-template-rows: 1fr;
    height: 100dvh;
    overflow: hidden;
    background: transparent;
    color: var(--text-primary);
    position: relative;
    z-index: 1;
  }

  .container.mobile {
    grid-template-columns: 1fr !important;
  }

  .sidebar {
    display: flex;
    flex-direction: column;
    height: 100%;
    background: var(--bg-secondary);
    border-right: 1px solid var(--border-color);
  }

  /* 移动端侧边栏：全屏覆盖层 */
  .sidebar-mobile {
    position: fixed;
    top: 0;
    left: 0;
    width: 85vw;
    max-width: 320px;
    height: 100dvh;
    z-index: 100;
    border-right: 1px solid var(--border-color);
    transform: translateX(-100%);
    transition: transform 0.25s cubic-bezier(0.4, 0, 0.2, 1);
  }
  .sidebar-mobile.visible {
    transform: translateX(0);
  }

  /* 移动端遮罩层 */
  .mobile-overlay {
    position: fixed;
    inset: 0;
    z-index: 99;
    background: rgba(0, 0, 0, 0.5);
    backdrop-filter: blur(4px);
  }

  /* 移动端标题栏 */
  .mobile-header {
    display: none;
    position: absolute;
    top: 0;
    left: 0;
    right: 0;
    height: 44px;
    z-index: 20;
    align-items: center;
    padding-left: 4px;
    background: transparent;
  }
  .mobile-header.visible {
    display: flex;
  }
  .back-btn {
    display: flex;
    align-items: center;
    justify-content: center;
    width: 40px;
    height: 40px;
    background: var(--bg-tertiary);
    border: 1px solid var(--border-color);
    border-radius: 50%;
    color: var(--text-primary);
    cursor: pointer;
    transition: all 0.2s;
  }
  .back-btn:active {
    background: var(--border-color);
    transform: scale(0.95);
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
    transform: rotate(60deg);
  }

  .icon-btn svg {
    transition: transform 0.3s ease;
  }

  .icon-btn:active svg {
    transform: rotate(30deg) scale(0.95);
  }

  .main {
    display: flex;
    flex-direction: column;
    position: relative;
    min-width: 0;
    overflow: hidden;
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
    min-height: 0;
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
</style>
