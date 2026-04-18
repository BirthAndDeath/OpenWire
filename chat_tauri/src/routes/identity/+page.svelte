<script lang="ts">
  import "../../lib/i18n";
  import { _, locale } from "svelte-i18n";
  import { listen } from "@tauri-apps/api/event";
  import { invoke } from "@tauri-apps/api/core";
  import { onMount } from "svelte";
  import { goto } from "$app/navigation";
  import Toast from "../Toast.svelte";

  interface IdentityDto {
    id: number;
    peer_id: string;
    is_current: boolean;
  }

  let identities = $state<IdentityDto[]>([]);
  let currentIdentity = $state("");
  let loadingIdentities = $state(false);
  let warning = $state<string>("");

  // 显示 warning 的统一函数
  const showWarning = (message: string, duration: number = 5000) => {
    warning = message;
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
        showWarning("身份切换成功", 3000);
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
        showWarning("身份删除成功", 3000);
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

  const copyPeerId = async (peerId: string) => {
    if (!peerId) return;
    try {
      await navigator.clipboard.writeText(peerId);
      showWarning("PeerID 已复制到剪贴板", 3000);
    } catch (e) {
      showWarning(`复制失败：${e}`);
    }
  };

  // 返回首页
  function goBack() {
    goto("/");
  }

  onMount(() => {
    let unlisten: (() => void) | undefined;

    (async () => {
      unlisten = await listen<string>("warning", (e) => {
        showWarning(e.payload, 5000);
        console.warn("Received warning from backend:", e.payload);
      });
    })();

    loadIdentities();

    return () => {
      unlisten?.();
    };
  });
</script>

<div class="identity-container">
  <header class="identity-header">
    <button class="back-button" onclick={goBack} aria-label="返回主页">
      ← {$_("back")}
    </button>
    <h1>{$_("identity_management")}</h1>
  </header>

  <main class="identity-content">
    <!-- 当前身份显示 -->
    {#if currentIdentity}
      <section class="current-identity-section">
        <h2>{$_("current_identity")}</h2>
        <button
          class="current-identity-card"
          onclick={() => copyPeerId(currentIdentity)}
          title={$_("click_to_copy")}
          type="button"
        >
          <code class="peerid-display">{currentIdentity}</code>
          <span class="copy-hint">{$_("click_to_copy")}</span>
        </button>
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
          {#each identities as identity (identity.peer_id)}
            <div class="identity-item" class:current={identity.is_current}>
              <div class="identity-info">
                <code class="identity-peerid">{identity.peer_id}</code>
                {#if identity.is_current}
                  <span class="current-badge">{$_("current")}</span>
                {/if}
              </div>
              <div class="identity-actions">
                {#if !identity.is_current}
                  <button
                    class="icon-btn select-btn"
                    onclick={() => selectIdentity(identity.peer_id)}
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
                  onclick={() => copyPeerId(identity.peer_id)}
                  title={$_("copy_peer_id")}
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
                    onclick={() => deleteIdentity(identity.peer_id)}
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

  <Toast message={warning} />
</div>

<style>
  .identity-container {
    display: flex;
    flex-direction: column;
    height: 100vh;
    background: var(--bg-primary, #0f0f0f);
    color: var(--text-primary, #f6f6f6);
    font-family: system-ui;
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
    cursor: pointer;
    transition: all 0.2s;
    width: 100%;
    text-align: left;
    font: inherit;
    color: inherit;
  }

  .current-identity-card:hover {
    background: var(--bg-primary, #111111);
    border-color: #60a5fa;
    transform: translateY(-2px);
    box-shadow: 0 4px 12px rgba(59, 130, 246, 0.2);
  }

  .current-identity-card:focus {
    outline: 2px solid #3b82f6;
    outline-offset: 2px;
  }

  .peerid-display {
    display: block;
    font-family: "Courier New", monospace;
    font-size: 13px;
    color: var(--text-primary, #ffffff);
    word-break: break-all;
    margin-bottom: 8px;
  }

  .copy-hint {
    font-size: 12px;
    color: var(--text-secondary, #737373);
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

  .identity-peerid {
    font-family: "Courier New", monospace;
    font-size: 12px;
    color: var(--text-primary, #fafafa);
    word-break: break-all;
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
