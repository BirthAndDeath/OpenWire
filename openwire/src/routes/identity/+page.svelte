<script lang="ts">
  import { _ } from "svelte-i18n";
  import { tt } from "../../lib/i18n";
  import { listen } from "@tauri-apps/api/event";
  import { invoke } from "@tauri-apps/api/core";
  import { onMount } from "svelte";
  import { goto } from "$app/navigation";
  import Toast from "../Toast.svelte";
  import { drawQrCode } from "../../lib/qrcode";

  interface IdentityDto {
    id: number;
    identity_id: string;
    is_current: boolean;
    mlkem_pubkey_hex: string | null;
  }

  let identities = $state<IdentityDto[]>([]);
  let currentIdentity = $state<IdentityDto | null>(null);
  let loadingIdentities = $state(false);
  let warning = $state<string>("");
  let qrCanvas = $state<HTMLCanvasElement | undefined>(undefined);
  // 删除身份确认：置入待删身份 id 时弹确认框（删除将级联清除联系人/消息/密钥）
  let confirmDeleteId = $state<string | null>(null);

  // 完整公钥（3904 字符）默认隐藏：展示 SHA256 摘要（与扫码 digest 同源），可点击展开完整
  let currentShort = $state("");
  let currentReveal = $state(false);
  let rowReveal = $state<Record<number, boolean>>({});
  let rowShort = $state<Record<number, string>>({});

  const sha256Hex = async (text: string): Promise<string> => {
    try {
      const buf = await crypto.subtle.digest(
        "SHA-256",
        new TextEncoder().encode(text),
      );
      return [...new Uint8Array(buf)]
        .map((b) => b.toString(16).padStart(2, "0"))
        .join("");
    } catch {
      return `${text.slice(0, 16)}…`; // 降级：裁剪显示
    }
  };

  // 显示 warning 的统一函数
  const showWarning = (message: string, duration: number = 5000) => {
    warning = message;
  };

  const loadIdentities = async () => {
    loadingIdentities = true;
    try {
      identities = await invoke<IdentityDto[]>("list_identities");
      currentIdentity = identities.find((id) => id.is_current) ?? null;
      if (currentIdentity) {
        currentShort = await sha256Hex(currentIdentity.identity_id);
        await loadQrData();
      }
      // 行级摘要缓存（默认展示，展开时才显示完整公钥）
      const shorts: Record<number, string> = {};
      for (const ident of identities) {
        shorts[ident.id] = await sha256Hex(ident.identity_id);
      }
      rowShort = shorts;
    } catch (e) {
      showWarning(tt("load_identity_failed", { values: { error: e } }));
    } finally {
      loadingIdentities = false;
    }
  };

  const loadQrData = async () => {
    try {
      console.log("正在加载二维码数据...");
      // 后端返回 ML-DSA 公钥的原始字节（Vec<u8> → number[]）
      const data: number[] = await invoke("get_identity_qr_data");
      const binary = new Uint8Array(data);
      console.log("二维码二进制数据加载成功，长度:", binary.length, "字节");
      // 数据加载完成后，等待 DOM 更新再绘制二维码
      setTimeout(() => drawQrCodeOnCanvas(binary), 100);
    } catch (e) {
      console.error("加载二维码数据失败:", e);
      // 没有当前身份时不显示二维码
    }
  };

  // 在 canvas 上绘制二维码
  const drawQrCodeOnCanvas = (binaryData?: Uint8Array) => {
    if (!qrCanvas || !binaryData) return;
    drawQrCode(qrCanvas, binaryData, 540);
  };

  const selectIdentity = async (identityId: string) => {
    if (!identityId) return;
    try {
      await invoke("select_identity", { identityId });
      // 切换身份后刷新页面，重新初始化所有状态
      window.location.reload();
    } catch (e) {
      showWarning(tt("switch_identity_failed", { values: { error: e } }));
    }
  };

  const deleteIdentity = async (identityId: string) => {
    if (!identityId) return;
    try {
      await invoke("delete_identity", { identityId });
      // 删除身份后刷新页面，重新初始化所有状态
      window.location.reload();
    } catch (e) {
      showWarning(tt("delete_identity_failed", { values: { error: e } }));
    }
  };

  const createIdentity = async () => {
    try {
      await invoke("generate_identity");
      await loadIdentities();
      showWarning(tt("identity_generated"), 3000);
    } catch (e) {
      showWarning(tt("generate_identity_failed", { values: { error: e } }));
    }
  };

  const copyToClipboard = async (text: string, label: string = tt("copy_content")) => {
    if (!text) return;
    try {
      await navigator.clipboard.writeText(text);
      showWarning(tt("copied_to_clipboard", { values: { label } }), 3000);
    } catch (e) {
      showWarning(tt("copy_failed", { values: { error: e } }));
    }
  };

  // 返回首页
  function goBack() {
    goto("/");
  }

  onMount(() => {
    let unlisten: (() => void) | undefined;
    let destroyed = false;

    (async () => {
      try {
        const un = await listen<string>("warning", (e) => {
          showWarning(e.payload, 5000);
          console.warn("Received warning from backend:", e.payload);
        });
        // 注册期间页面可能已卸载：此时立即注销，避免监听泄漏到已销毁页面
        if (destroyed) un();
        else unlisten = un;
      } catch (e) {
        console.error("注册 warning 监听失败:", e);
      }
    })();

    loadIdentities();

    return () => {
      destroyed = true;
      unlisten?.();
    };
  });
</script>

<div class="identity-container">
  <header class="identity-header">
    <button class="back-button" onclick={goBack} aria-label={$_("back_to_home")}>
      ← {$_("back")}
    </button>
    <h1>{$_("identity_management")}</h1>
  </header>

  <main class="identity-content">
    <!-- 当前身份显示 -->
    {#if currentIdentity}
      {@const identity = currentIdentity}
      <section class="current-identity-section">
        <h2>{$_("current_identity")}</h2>
        <div class="current-identity-card">
          <div class="identity-field">
            <label for="identity-id-display">{$_("identity_id_label")}</label>
            <button
              id="identity-id-display"
              class="copyable-value"
              onclick={() => copyToClipboard(identity.identity_id, $_("identity_id"))}
              title={$_("click_to_copy")}
              type="button"
            >
              <code>{currentReveal ? identity.identity_id : currentShort}</code>
              <span class="copy-icon">📋</span>
            </button>
            <button
              type="button"
              class="reveal-toggle"
              onclick={() => (currentReveal = !currentReveal)}
            >
              {currentReveal ? $_("hide_full_id") : $_("show_full_id")}
            </button>
          </div>
          {#if identity.mlkem_pubkey_hex}
            <div class="identity-field">
              <label for="mlkem-pubkey-display">{$_("mlkem_pubkey_current_label")}</label>
              <button
                id="mlkem-pubkey-display"
                class="copyable-value"
                onclick={() =>
                  copyToClipboard(identity.mlkem_pubkey_hex!, $_("mlkem_pubkey"))}
                title={$_("click_to_copy")}
                type="button"
              >
                <code>{identity.mlkem_pubkey_hex}</code>
                <span class="copy-icon">📋</span>
              </button>
            </div>
          {/if}
        </div>
      </section>

      <!-- 身份二维码 -->
      <section class="qr-section">
        <h2>{$_("identity_qr_code")}</h2>
        <p class="qr-desc">{$_("identity_qr_desc")}</p>
        <div class="qr-container">
          <canvas bind:this={qrCanvas} class="qr-canvas"></canvas>
        </div>
      </section>
    {/if}

    <!-- 身份操作按钮 -->
    <section class="identity-actions-section">
      <h2>{$_("identity_actions")}</h2>
      <div class="action-buttons">
        <button
          class="action-btn primary"
          onclick={createIdentity}
          disabled={loadingIdentities}
        >
          <svg
            xmlns="http://www.w3.org/2000/svg"
            width="20"
            height="20"
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            stroke-width="2"
            stroke-linecap="round"
            stroke-linejoin="round"
          >
            <line x1="12" y1="5" x2="12" y2="19"></line>
            <line x1="5" y1="12" x2="19" y2="12"></line>
          </svg>
          {$_("generate_identity")}
        </button>
      </div>
    </section>

    <!-- 身份列表 -->
    <section class="identities-list-section">
      <h2>{$_("all_identities")} ({identities.length})</h2>

      {#if loadingIdentities}
        <div class="loading-state">{$_("loading")}...</div>
      {:else if identities.length === 0}
        <div class="empty-state">
          <p>{$_("no_identities")}</p>
          <p class="hint">{$_("create_first_identity")}</p>
        </div>
      {:else}
        <div class="identities-list">
          {#each identities as identity (identity.id)}
            <div class="identity-item" class:current={identity.is_current}>
              <div class="identity-info">
                <div class="identity-details">
                  <div class="detail-row">
                    <span class="detail-label">ID:</span>
                    <button
                      type="button"
                      class="identity-id"
                      onclick={() =>
                        (rowReveal[identity.id] = !rowReveal[identity.id])}
                      title={
                        rowReveal[identity.id]
                          ? $_("hide_full_id")
                          : $_("show_full_id")
                      }
                      >{rowReveal[identity.id]
                        ? identity.identity_id
                        : rowShort[identity.id] ?? ""}</button
                    >
                  </div>
                </div>
                {#if identity.is_current}
                  <span class="current-badge">{$_("current")}</span>
                {/if}
              </div>
              <div class="identity-actions">
                {#if !identity.is_current}
                  <button
                    class="icon-btn select-btn"
                    onclick={() => selectIdentity(identity.identity_id)}
                    title={$_("switch_to_this_identity")}
                  >
                    <svg
                      xmlns="http://www.w3.org/2000/svg"
                      width="18"
                      height="18"
                      viewBox="0 0 24 24"
                      fill="none"
                      stroke="currentColor"
                      stroke-width="2"
                      stroke-linecap="round"
                      stroke-linejoin="round"
                    >
                      <polyline points="20 6 9 17 4 12"></polyline>
                    </svg>
                  </button>
                {/if}
                <button
                  class="icon-btn copy-btn"
                  onclick={() =>
                    copyToClipboard(identity.identity_id, $_("identity_id"))}
                  title={$_("copy_identity_id")}
                >
                  <svg
                    xmlns="http://www.w3.org/2000/svg"
                    width="18"
                    height="18"
                    viewBox="0 0 24 24"
                    fill="none"
                    stroke="currentColor"
                    stroke-width="2"
                    stroke-linecap="round"
                    stroke-linejoin="round"
                  >
                    <rect x="9" y="9" width="13" height="13" rx="2" ry="2"
                    ></rect>
                    <path
                      d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"
                    ></path>
                  </svg>
                </button>
                {#if !identity.is_current}
                  <button
                    class="icon-btn delete-btn"
                    onclick={() => (confirmDeleteId = identity.identity_id)}
                    title={$_("delete_identity")}
                  >
                    <svg
                      xmlns="http://www.w3.org/2000/svg"
                      width="18"
                      height="18"
                      viewBox="0 0 24 24"
                      fill="none"
                      stroke="currentColor"
                      stroke-width="2"
                      stroke-linecap="round"
                      stroke-linejoin="round"
                    >
                      <polyline points="3 6 5 6 21 6"></polyline>
                      <path
                        d="M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6m3 0V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2"
                      ></path>
                    </svg>
                  </button>
                {/if}
              </div>
            </div>
          {/each}
        </div>
      {/if}
    </section>
  </main>

  {#if confirmDeleteId}
    <div
      class="confirm-overlay"
      onclick={(e) => e.target === e.currentTarget && (confirmDeleteId = null)}
      onkeydown={(e) => e.key === "Escape" && (confirmDeleteId = null)}
      role="dialog"
      aria-label={$_("delete_identity")}
      tabindex="0"
    >
      <div class="confirm-box">
        <h3>{$_("delete_identity")}</h3>
        <p>{$_("delete_identity_confirm")}</p>
        <div class="confirm-actions">
          <button
            type="button"
            class="btn-ghost"
            onclick={() => (confirmDeleteId = null)}
          >
            {$_("action_cancel")}
          </button>
          <button
            type="button"
            class="btn-danger"
            onclick={() => {
              const id = confirmDeleteId;
              confirmDeleteId = null;
              if (id) void deleteIdentity(id);
            }}
          >
            {$_("confirm_delete")}
          </button>
        </div>
      </div>
    </div>
  {/if}

  <Toast message={warning} />
</div>

<style>
  .identity-container {
    display: flex;
    flex-direction: column;
    height: 100dvh;
    background: transparent;
    color: var(--text-primary, #f6f6f6);
    font-family: system-ui;
    position: relative;
    z-index: 1;
  }

  @media (width <= 480px) {
    .identity-container {
      padding-top: var(--safe-area-top);
      padding-bottom: var(--safe-area-bottom);
    }
    .identity-header {
      padding: 12px 12px;
    }
    .identity-content {
      padding: 12px;
    }
  }

  .identity-header {
    display: flex;
    align-items: center;
    gap: 16px;
    padding: 16px 24px;
    border-bottom: 1px solid var(--border-color, #2a2a2a);
    background: var(--bg-secondary, #0a0a0a);
  }

  .back-button {
    background: transparent;
    border: 1px solid var(--border-color, #2a2a2a);
    border-radius: 6px;
    padding: 8px 16px;
    color: var(--text-primary, #fafafa);
    cursor: pointer;
    font-size: 14px;
    transition: all 0.2s;
  }

  .back-button:hover {
    background: var(--bg-secondary, #1a1a1a);
    border-color: var(--primary, #3b82f6);
    color: var(--primary, #3b82f6);
  }

  .back-button:focus {
    outline: 2px solid var(--primary, #3b82f6);
    outline-offset: 2px;
  }

  .identity-header h1 {
    margin: 0;
    font-size: 24px;
    font-weight: 600;
    color: var(--text-primary, #fafafa);
  }

  .identity-content {
    flex: 1;
    overflow-y: auto;
    padding: 24px;
    scroll-behavior: smooth;
  }

  section {
    margin-bottom: 32px;
    background: var(--bg-tertiary, #1a1a1a);
    border: 1px solid var(--border-color, #2a2a2a);
    border-radius: 8px;
    padding: 20px;
  }

  section h2 {
    margin: 0 0 16px 0;
    font-size: 18px;
    font-weight: 600;
    color: var(--text-primary, #fafafa);
    border-bottom: 1px solid var(--border-color, #2a2a2a);
    padding-bottom: 8px;
  }

  /* 当前身份卡片 */
  .current-identity-card {
    background: var(--bg-secondary, #0a0a0a);
    border: 2px solid #3b82f6;
    border-radius: 8px;
    padding: 16px;
    width: 100%;
  }

  .identity-field {
    margin-bottom: 16px;
  }

  .identity-field:last-child {
    margin-bottom: 0;
  }

  .identity-field label {
    display: block;
    font-size: 12px;
    color: var(--text-secondary, #737373);
    margin-bottom: 6px;
    font-weight: 500;
  }

  .copyable-value {
    display: flex;
    align-items: center;
    justify-content: space-between;
    background: var(--bg-primary, #111111);
    border: 1px solid var(--border-color, #2a2a2a);
    border-radius: 6px;
    padding: 10px 12px;
    cursor: pointer;
    transition: all 0.2s;
    width: 100%;
    text-align: left;
    font: inherit;
    color: inherit;
  }

  .copyable-value:hover {
    background: var(--bg-tertiary, #1a1a1a);
    border-color: #60a5fa;
  }

  .copyable-value:focus {
    outline: 2px solid #3b82f6;
    outline-offset: 2px;
  }

  .copyable-value code {
    font-family: "Courier New", monospace;
    font-size: 12px;
    color: var(--text-primary, #ffffff);
    word-break: break-all;
    flex: 1;
    margin-right: 8px;
  }

  .copy-icon {
    font-size: 16px;
    opacity: 0.6;
    transition: opacity 0.2s;
  }

  .copyable-value:hover .copy-icon {
    opacity: 1;
  }

  .reveal-toggle {
    margin-top: 6px;
    background: none;
    border: none;
    color: #60a5fa;
    cursor: pointer;
    font-size: 12px;
    padding: 2px 4px;
    align-self: flex-start;
  }
  .reveal-toggle:hover {
    text-decoration: underline;
  }

  .identity-id {
    cursor: pointer;
    user-select: none;
  }
  .identity-id:hover {
    color: #60a5fa;
  }

  .confirm-overlay {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.5);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 1000;
  }
  .confirm-box {
    width: 90%;
    max-width: 380px;
    padding: 20px 22px;
    background: var(--bg-secondary);
    border: 1px solid var(--border-color, #2a2a2a);
    border-radius: 12px;
    box-shadow: 0 4px 20px rgba(0, 0, 0, 0.3);
  }
  .confirm-box h3 {
    margin: 0 0 12px;
    color: var(--text-primary, #fafafa);
  }
  .confirm-box p {
    margin: 0 0 16px;
    font-size: 14px;
    line-height: 1.5;
    color: var(--text-secondary, #a3a3a3);
  }
  .confirm-actions {
    display: flex;
    justify-content: flex-end;
    gap: 10px;
  }
  .btn-ghost,
  .btn-danger {
    padding: 8px 16px;
    border-radius: 6px;
    cursor: pointer;
    font-size: 14px;
    transition: all 0.2s;
  }
  .btn-ghost {
    background: transparent;
    border: 1px solid var(--border-color, #2a2a2a);
    color: var(--text-secondary, #a3a3a3);
  }
  .btn-ghost:hover {
    background: var(--bg-tertiary, #1a1a1a);
    color: var(--text-primary, #fafafa);
  }
  .btn-danger {
    background: var(--color-danger, #ef4444);
    border: none;
    color: #fff;
  }
  .btn-danger:hover {
    opacity: 0.85;
  }

  /* 二维码区域 */
  .qr-section {
    text-align: center;
  }

  .qr-desc {
    font-size: 13px;
    color: var(--text-secondary, #737373);
    margin: -8px 0 16px 0;
  }

  .qr-container {
    display: flex;
    justify-content: center;
    padding: 16px;
    background: #ffffff;
    border-radius: 8px;
    border: 1px solid var(--border-color, #2a2a2a);
    margin-bottom: 12px;
    max-width: 100%;
    overflow: hidden;
  }

  .qr-canvas {
    border-radius: 4px;
    image-rendering: pixelated;
    max-width: 100%;
    height: auto;
  }

  /* 操作按钮 */
  .action-buttons {
    display: flex;
    gap: 12px;
    flex-wrap: wrap;
  }

  .action-btn {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 12px 20px;
    border: 1px solid var(--border-color, #2a2a2a);
    border-radius: 8px;
    background: var(--bg-secondary, #0a0a0a);
    color: var(--text-primary, #fafafa);
    cursor: pointer;
    font-size: 14px;
    transition: all 0.2s;
  }

  .action-btn:focus {
    outline: 2px solid #3b82f6;
    outline-offset: 2px;
  }

  .action-btn.primary {
    background: var(--primary, #3b82f6);
    border-color: var(--primary, #3b82f6);
    color: white;
  }

  .action-btn.primary:hover:not(:disabled) {
    background: #2563eb;
    border-color: #2563eb;
    transform: translateY(-2px);
    box-shadow: 0 4px 12px rgba(59, 130, 246, 0.3);
  }

  .action-btn.primary:focus {
    outline: 2px solid #ffffff;
    outline-offset: 2px;
  }

  .action-btn:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }

  /* 身份列表 */
  .identities-list {
    display: flex;
    flex-direction: column;
    gap: 12px;
  }

  .identity-item {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 12px 16px;
    background: var(--bg-secondary, #0a0a0a);
    border: 1px solid var(--border-color, #2a2a2a);
    border-radius: 8px;
    transition: all 0.2s;
  }

  .identity-item.current {
    border-color: #3b82f6;
    background: rgba(59, 130, 246, 0.05);
  }

  .identity-item:hover {
    border-color: #3b82f6;
    transform: translateX(4px);
  }

  .identity-info {
    display: flex;
    align-items: center;
    gap: 12px;
    flex: 1;
    min-width: 0;
  }

  .identity-details {
    flex: 1;
    min-width: 0;
  }

  .detail-row {
    display: flex;
    align-items: center;
    gap: 8px;
    margin-bottom: 4px;
    min-width: 0;
  }

  .detail-row:last-child {
    margin-bottom: 0;
  }

  .detail-label {
    font-size: 11px;
    color: var(--text-secondary, #737373);
    min-width: 40px;
    flex-shrink: 0;
  }

.identity-id {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    /* 以按钮承载但保持代码外观 */
    background: none;
    border: none;
    padding: 0;
    font: inherit;
    color: inherit;
    text-align: left;
    cursor: pointer;
    user-select: none;
    min-width: 0;
  }
  .identity-id:hover {
    color: #60a5fa;
  }

  .current-badge {
    background: #3b82f6;
    color: white;
    padding: 2px 8px;
    border-radius: 12px;
    font-size: 11px;
    font-weight: 600;
    white-space: nowrap;
  }

  .identity-actions {
    display: flex;
    gap: 8px;
  }

  .icon-btn {
    display: flex;
    align-items: center;
    justify-content: center;
    width: 32px;
    height: 32px;
    background: transparent;
    border: 1px solid var(--border-color, #2a2a2a);
    border-radius: 6px;
    color: var(--text-primary, #fafafa);
    cursor: pointer;
    transition: all 0.2s;
    padding: 0;
  }

  .icon-btn:focus {
    outline: 2px solid #3b82f6;
    outline-offset: 2px;
  }

  .select-btn:hover {
    background: #3b82f6;
    border-color: #3b82f6;
    color: white;
  }

  .copy-btn:hover {
    background: #10b981;
    border-color: #10b981;
    color: white;
  }

  .delete-btn:hover {
    background: #ef4444;
    border-color: #ef4444;
    color: white;
  }

  /* 状态提示 */
  .loading-state,
  .empty-state {
    text-align: center;
    padding: 40px 20px;
    color: var(--text-secondary, #737373);
  }

  .empty-state p {
    margin: 8px 0;
  }

  .empty-state .hint {
    font-size: 13px;
    color: var(--text-secondary, #525252);
  }
</style>
