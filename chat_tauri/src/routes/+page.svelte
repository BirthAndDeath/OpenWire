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
  import AddFriendModal from "./AddFriendModal.svelte";
  interface IdentityDto {
    id: number;
    identity_id: string;
    is_current: boolean;
    mlkem_pubkey_hex: string | null;
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
  // 显示 warning 的统一函数
  const showWarning = (message: string, duration: number = 5000) => {
    warning = message;
    console.warn("Warning:", message);
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
  let sidebarW = $state(300);
  let inputH = $state(150);

  // 添加好友相关状态
  let showAddFriendModal = $state(false);

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

  // === 启动密码输入模态框状态 ===
  let showPasswordModal = $state(false);
  let passwordModalStatus = $state<"input" | "retrying" | "success" | "failed">(
    "input",
  );
  let passwordModalError = $state("");
  let startupPassword = $state("");

  // === 核心是否已就绪（后端核心初始化完成） ===
  let coreReady = $state(false);

  // === 监听消息和事件 ===
  onMount(() => {
    let unlistenWarning: (() => void) | undefined;
    let unlistenMessage: (() => void) | undefined;
    let unlistenFileProgress: (() => void) | undefined;
    let unlistenNeedPassword: (() => void) | undefined;
    let unlistenCoreReady: (() => void) | undefined;
    let unlistenDeliveryReceipt: (() => void) | undefined;
    let pollingTimer: ReturnType<typeof setInterval> | undefined;

    (async () => {
      // 监听 core-ready 事件（核心初始化完成后由后端发送）
      // 注意：Tauri 的 emit 是 fire-and-forget，如果前端尚未注册 listener，
      // 事件可能丢失。因此同时使用 check_core_ready 命令轮询作为可靠兜底。
      unlistenCoreReady = await listen<boolean>("core-ready", () => {
        coreReady = true;
        // 核心就绪后加载数据
        loadContacts();
        loadCurrentIdentity();
      });

      // 监听 need-password 事件（Keyring 不可用时由后端发送）
      unlistenNeedPassword = await listen<boolean>("need-password", () => {
        showPasswordModal = true;
        passwordModalStatus = "input";
        passwordModalError = "";
        startupPassword = "";
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
        const ready = await invoke<boolean>("check_core_ready");
        if (ready) {
          coreReady = true;
          clearInterval(pollingTimer);
          loadContacts();
          loadCurrentIdentity();
        }
      } catch (e) {
        console.warn("check_core_ready 调用失败:", e);
      }
    }, 200);

    return () => {
      if (pollingTimer) clearInterval(pollingTimer);
      unlistenWarning?.();
      unlistenMessage?.();
      unlistenFileProgress?.();
      unlistenNeedPassword?.();
      unlistenCoreReady?.();
      unlistenDeliveryReceipt?.();
    };
  });

  // === 拖动调整 ===
  // 使用 requestAnimationFrame 批量处理拖动事件，
  // 避免在同一个帧内多次触发 ResizeObserver（VList 虚拟滚动组件内部使用）
  let dragRafId: number | null = null;

  function drag(min: number, max: number, vertical: boolean) {
    return (e: MouseEvent) => {
      if (dragRafId !== null) return; // 同一帧内只执行最后一次
      dragRafId = requestAnimationFrame(() => {
        dragRafId = null;
        const val = vertical
          ? Math.max(min, Math.min(max, e.clientX))
          : Math.max(min, Math.min(max, window.innerHeight - e.clientY - 16));
        vertical ? (sidebarW = val) : (inputH = val);
      });
    };
  }

  function startDrag(isSidebar: boolean) {
    const handler = drag(
      isSidebar ? 200 : 100,
      isSidebar ? 500 : 300,
      isSidebar,
    );
    const up = () => {
      document.removeEventListener("mousemove", handler);
      document.removeEventListener("mouseup", up);
      if (dragRafId !== null) {
        cancelAnimationFrame(dragRafId);
        dragRafId = null;
      }
    };
    document.addEventListener("mousemove", handler);
    document.addEventListener("mouseup", up);
  }

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

  // 启动密码输入提交处理
  async function handleStartupPassword(password: string) {
    passwordModalStatus = "retrying";
    passwordModalError = "";
    try {
      // 先设置密码到 AppData
      await invoke("set_password", { password });
      // 后端轮询会自动检测到密码已设置并重试初始化
      // 等待一小段时间让后端处理
      await new Promise((resolve) => setTimeout(resolve, 1000));
      // 检查是否 still needed（如果后端轮询成功，need_password 会被设为 false）
      // 如果后端轮询超时或失败，用户可能需要重新输入
      passwordModalStatus = "success";
      // 延迟关闭模态框
      setTimeout(() => {
        showPasswordModal = false;
      }, 1500);
    } catch (e) {
      passwordModalStatus = "failed";
      passwordModalError = `密码设置失败: ${e}`;
    }
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
    </div>

    <Contactlist
      {contacts}
      {selectedId}
      onselect={select}
      ondelete={(id) => {
        // 从联系人列表中移除
        contacts = contacts.filter((c) => c.pubkey_hex !== id);
        // 如果删除的是当前选中的联系人，清空选择
        if (selectedId === id)
          selectedId = contacts.length > 0 ? contacts[0].pubkey_hex : null;
      }}
    />

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
  <AddFriendModal
    bind:show={showAddFriendModal}
    {currentIdentityId}
    onFriendAdded={loadContacts}
  />

  <!-- 启动密码输入模态框（Keyring 不可用时弹出） -->
  {#if showPasswordModal}
    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <div class="password-modal-overlay" onclick={() => {}} onkeydown={() => {}}>
      <div
        class="password-modal"
        onclick={(e) => e.stopPropagation()}
        onkeydown={() => {}}
      >
        <h2>{$_("need_password_title")}</h2>
        <p class="password-modal-desc">{$_("need_password_desc")}</p>

        {#if passwordModalStatus === "input"}
          <div class="startup-password-input">
            <input
              id="startup-password"
              type="password"
              bind:value={startupPassword}
              placeholder={$_("password_placeholder")}
              autocomplete="current-password"
              onkeydown={(e) => {
                if (e.key === "Enter" && startupPassword.length >= 8) {
                  handleStartupPassword(startupPassword);
                }
              }}
            />
            {#if passwordModalError}
              <p class="startup-password-error">{passwordModalError}</p>
            {/if}
            <button
              class="startup-password-btn"
              disabled={startupPassword.length < 8}
              onclick={() => handleStartupPassword(startupPassword)}
            >
              {$_("need_password_retry")}
            </button>
          </div>
        {:else if passwordModalStatus === "retrying"}
          <div class="password-modal-status">
            <span class="spinner"></span>
            <p>{$_("need_password_retrying")}</p>
          </div>
        {:else if passwordModalStatus === "success"}
          <div class="password-modal-status success">
            <span class="checkmark">✓</span>
            <p>{$_("need_password_success")}</p>
          </div>
        {:else if passwordModalStatus === "failed"}
          <div class="password-modal-status failed">
            <span class="cross">✗</span>
            <p>{$_("need_password_failed")}</p>
            {#if passwordModalError}
              <p class="password-modal-error">{passwordModalError}</p>
            {/if}
            <button
              class="password-modal-retry-btn"
              onclick={() => (passwordModalStatus = "input")}
            >
              {$_("need_password_retry")}
            </button>
          </div>
        {/if}
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
        <Messagelist bind:this={msgListRef} contactId={selectedId} />
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
        mldsaPubkeyHex={selectedId ?? ""}
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
    display: flex;
    height: 100vh;
    overflow: hidden;
    background: transparent;
    color: var(--text-primary);
    position: relative;
    z-index: 1;
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
    transform: rotate(60deg);
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

  /* 启动密码输入模态框 */
  .password-modal-overlay {
    position: fixed;
    top: 0;
    left: 0;
    width: 100%;
    height: 100%;
    background: rgba(0, 0, 0, 0.6);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 1000;
    backdrop-filter: blur(4px);
  }

  .password-modal {
    background: var(--bg-primary);
    border: 1px solid var(--border-color);
    border-radius: 16px;
    padding: 32px;
    max-width: 420px;
    width: 90%;
    box-shadow: 0 8px 32px rgba(0, 0, 0, 0.3);
  }

  .password-modal h2 {
    margin: 0 0 8px 0;
    font-size: 20px;
    color: var(--text-primary);
  }

  .password-modal-desc {
    margin: 0 0 24px 0;
    font-size: 14px;
    color: var(--text-secondary);
    line-height: 1.5;
  }

  .startup-password-input {
    display: flex;
    flex-direction: column;
    gap: 12px;
  }

  .startup-password-input input {
    width: 100%;
    padding: 12px;
    border: 1px solid var(--border-color);
    border-radius: 8px;
    background: var(--bg-tertiary);
    color: var(--text-primary);
    font-size: 16px;
    box-sizing: border-box;
    outline: none;
    transition: border-color 0.2s;
  }

  .startup-password-input input:focus {
    border-color: #3b82f6;
  }

  .startup-password-btn {
    width: 100%;
    padding: 12px;
    border: none;
    border-radius: 8px;
    background: #3b82f6;
    color: white;
    font-size: 16px;
    cursor: pointer;
    transition: background 0.2s;
  }

  .startup-password-btn:hover:not(:disabled) {
    background: #2563eb;
  }

  .startup-password-btn:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }

  .startup-password-error {
    color: #ef4444;
    font-size: 13px;
    margin: 0;
  }

  .password-modal-status {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 12px;
    padding: 24px 0;
    text-align: center;
  }

  .password-modal-status p {
    margin: 0;
    color: var(--text-primary);
    font-size: 15px;
  }

  .password-modal-status .spinner {
    width: 32px;
    height: 32px;
    border: 3px solid var(--border-color);
    border-top-color: #3b82f6;
    border-radius: 50%;
    animation: spin 0.8s linear infinite;
  }

  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }

  .password-modal-status .checkmark {
    font-size: 32px;
    color: #10b981;
  }

  .password-modal-status .cross {
    font-size: 32px;
    color: #ef4444;
  }

  .password-modal-status.success p {
    color: #10b981;
  }

  .password-modal-status.failed p {
    color: #ef4444;
  }

  .password-modal-error {
    color: #ef4444;
    font-size: 13px;
    margin: 0;
  }

  .password-modal-retry-btn {
    padding: 8px 24px;
    border: 1px solid var(--border-color);
    border-radius: 8px;
    background: var(--bg-tertiary);
    color: var(--text-primary);
    cursor: pointer;
    font-size: 14px;
    transition: all 0.2s;
  }

  .password-modal-retry-btn:hover {
    border-color: #3b82f6;
    color: #3b82f6;
  }
</style>
