<script lang="ts">
  import { get } from "svelte/store";
  import { _ } from "svelte-i18n";
  import { invoke } from "@tauri-apps/api/core";
  import {
    transferTasks,
    calcProgress,
    removeTransfer,
  } from "../lib/transfers";
  import type { FileTransferProgress } from "../lib/transfers";

  let { show = $bindable(false) }: { show?: boolean } = $props();

  function cancelTransfer(p: FileTransferProgress) {
    if (!p.file_id) return;
    invoke("cancel_file_transfer", { fileIdHex: p.file_id }).catch((e) =>
      console.error("取消传输失败:", e),
    );
  }

  function deleteTransfer(p: FileTransferProgress) {
    if (!p.file_id) return;
    invoke("cancel_file_transfer", { fileIdHex: p.file_id })
      .then(() => removeTransfer(p.file_id))
      .catch((e) => console.error("删除传输任务失败:", e));
  }

  // 重试失败下载：重新弹保存对话框发起下载（与消息卡片"重试"一致）
  function retryTransfer(p: FileTransferProgress) {
    if (!p.sender_pubkey_hex || !p.file_id) return;
    invoke("request_file_download", {
      senderMldsaPubkeyHex: p.sender_pubkey_hex,
      fileHashHex: p.file_id,
      filename: p.filename,
    }).catch((e) => console.error("重试下载失败:", e));
  }

  const statusLabel = (p: FileTransferProgress): string => {
    switch (p.status) {
      case "downloading":
        return get(_)("transfer_status_downloading");
      case "completed":
        return get(_)("transfer_status_completed");
      default:
        return get(_)("transfer_status_failed");
    }
  };

  const isBusy = (p: FileTransferProgress) => p.status === "downloading";
  const isFailed = (p: FileTransferProgress) =>
    p.status === "error" || p.status === "failed";
</script>

{#if show}
  <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
  <div
    class="modal-backdrop"
    role="dialog"
    tabindex="0"
    onclick={() => (show = false)}
    onkeydown={(e) => e.key === "Escape" && (show = false)}
  >
    <div
      class="panel"
      role="document"
      onclick={(e) => e.stopPropagation()}
      onkeydown={(e) => e.key === "Escape" && (show = false)}
    >
      <div class="head">
        <h2>{$_("transfer_manager")}</h2>
        <button class="close" onclick={() => (show = false)} aria-label={$_("action_close")}>
          ×
        </button>
      </div>

      <div class="tasks">
        {#if Object.values($transferTasks).length === 0}
          <div class="empty">{$_("transfer_tasks_empty")}</div>
        {:else}
          {#each Object.values($transferTasks) as p (p.file_id)}
            <div class="task" class:failed={isFailed(p)} class:done={p.status === "completed"}>
              <div class="row1">
                <span class="name" title={p.filename}>{p.filename}</span>
                <span class="status">{statusLabel(p)}</span>
              </div>
              {#if p.status === "downloading"}
                <div class="bar">
                  <div class="fill" style="width: {calcProgress(p)}%"></div>
                </div>
                <span class="hint">
                  {$_("downloading_progress", { values: { p: calcProgress(p) } })}
                </span>
              {/if}
              <div class="actions">
                {#if isBusy(p)}
                  <button
                    class="act"
                    onclick={() => cancelTransfer(p)}
                    aria-label={$_("action_cancel")}
                    >{$_("action_cancel")}</button
                  >
                {:else if isFailed(p)}
                  {#if p.sender_pubkey_hex}
                    <button
                      class="act"
                      onclick={() => retryTransfer(p)}
                      aria-label={$_("action_retry")}
                      >{$_("action_retry")}</button
                    >
                  {/if}
                  <button
                    class="act"
                    onclick={() => deleteTransfer(p)}
                    aria-label={$_("action_delete")}
                    >{$_("action_delete")}</button
                  >
                {:else}
                  <button
                    class="act"
                    onclick={() => deleteTransfer(p)}
                    aria-label={$_("action_delete")}
                    >{$_("action_delete")}</button
                  >
                {/if}
              </div>
            </div>
          {/each}
        {/if}
      </div>
    </div>
  </div>
{/if}

<style>
  .modal-backdrop {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.5);
    display: grid;
    place-items: center;
    z-index: 100;
  }
  .panel {
    width: min(520px, 92vw);
    max-height: 80vh;
    display: flex;
    flex-direction: column;
    background: var(--bg-secondary, #1f1f1f);
    border: 1px solid var(--border-color, #333);
    border-radius: 12px;
    overflow: hidden;
  }
  .head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 12px 16px;
    border-bottom: 1px solid var(--border-color, #333);
  }
  .head h2 {
    margin: 0;
    font-size: 15px;
  }
  .close {
    background: none;
    border: none;
    color: var(--text-secondary, #999);
    font-size: 18px;
    cursor: pointer;
  }
  .tasks {
    overflow-y: auto;
    padding: 8px 12px 12px;
  }
  .empty {
    color: var(--text-secondary, #999);
    text-align: center;
    padding: 24px 0;
  }
  .task {
    border: 1px solid var(--border-color, #333);
    border-radius: 8px;
    padding: 8px 10px;
    margin-top: 8px;
  }
  .task.failed {
    border-color: #b91c1c;
  }
  .task.done {
    opacity: 0.85;
  }
  .row1 {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
  }
  .name {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .status {
    flex-shrink: 0;
    font-size: 12px;
  }
  .task.failed .status {
    color: #f87171;
  }
  .task.done .status {
    color: #4ade80;
  }
  .bar {
    height: 4px;
    border-radius: 2px;
    background: #333;
    margin-top: 6px;
    overflow: hidden;
  }
  .fill {
    height: 100%;
    background: #3b82f6;
    border-radius: 2px;
    transition: width 0.2s;
  }
  .hint {
    display: block;
    font-size: 11px;
    color: var(--text-secondary, #999);
    margin-top: 4px;
  }
  .actions {
    display: flex;
    gap: 8px;
    margin-top: 8px;
  }
  .act {
    background: none;
    border: 1px solid var(--border-color, #444);
    border-radius: 6px;
    color: inherit;
    padding: 2px 10px;
    cursor: pointer;
    font-size: 12px;
  }
  .act:hover {
    border-color: #3b82f6;
    color: #3b82f6;
  }
</style>
