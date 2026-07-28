<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { _ } from "svelte-i18n";
  import { VList } from "virtua/svelte";

  let { show = $bindable(false) }: { show?: boolean } = $props();

  interface SentFile {
    file_hash: string;
    filename: string;
    total_size: number;
    sent_at: number;
  }

  let files = $state<SentFile[]>([]);
  let loading = $state(false);

  const load = async () => {
    loading = true;
    try {
      files = await invoke<SentFile[]>("list_sent_files");
    } catch (e) {
      console.error("加载已发送文件失败:", e);
    } finally {
      loading = false;
    }
  };

  const revoke = async (f: SentFile) => {
    try {
      await invoke("delete_sent_file", { fileHashHex: f.file_hash });
      files = files.filter((x) => x.file_hash !== f.file_hash);
    } catch (e) {
      console.error("撤销失败:", e);
    }
  };

  const fmtSize = (bytes: number) => {
    if (bytes === 0) return "0 B";
    const u = ["B", "KB", "MB", "GB"];
    const i = Math.floor(Math.log(bytes) / Math.log(1024));
    return (bytes / Math.pow(1024, i)).toFixed(1) + " " + u[i];
  };

  const fmtDate = (ts: number) => {
    const d = new Date(ts * 1000);
    return d.toLocaleString();
  };

  $effect(() => {
    if (show) load();
  });
</script>

{#if show}
  <div class="overlay" onclick={() => (show = false)} onkeydown={(e) => e.key === "Escape" && (show = false)} role="presentation">
    <div class="modal" onclick={(e) => e.stopPropagation()} onkeydown={() => {}} role="dialog" aria-label="已发送文件管理" tabindex="-1">
      <div class="header">
        <h2>{$_("sent_files")}</h2>
        <button class="close" onclick={() => (show = false)} aria-label="关闭">&times;</button>
      </div>

      {#if loading}
        <div class="loading">{$_("loading")}</div>
      {:else if files.length === 0}
        <div class="empty">{$_("no_sent_files")}</div>
      {:else}
        <div class="table-header">
          <span class="col-name">{$_("filename")}</span>
          <span class="col-size">{$_("size")}</span>
          <span class="col-date">{$_("sent_at")}</span>
          <span class="col-action"></span>
        </div>
        <div class="list-wrap">
          <VList data={files} style="height: 100%">
            {#snippet children(f: SentFile)}
              <div class="row">
                <span class="col-name" title={f.filename}>{f.filename}</span>
                <span class="col-size">{fmtSize(f.total_size)}</span>
                <span class="col-date">{fmtDate(f.sent_at)}</span>
                <span class="col-action">
                  <button class="revoke" onclick={() => revoke(f)} aria-label={$_("revoke")}>
                    {$_("revoke")}
                  </button>
                </span>
              </div>
            {/snippet}
          </VList>
        </div>
      {/if}
    </div>
  </div>
{/if}

<style>
  .overlay {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.5);
    display: grid;
    place-items: center;
    z-index: 100;
  }
  .modal {
    background: var(--bg-primary);
    border: 1px solid var(--border-color);
    border-radius: 12px;
    width: min(640px, 90vw);
    max-height: 80vh;
    display: flex;
    flex-direction: column;
    overflow: hidden;
    box-shadow: 0 8px 32px rgba(0, 0, 0, 0.4);
  }
  .header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 16px 20px;
    border-bottom: 1px solid var(--border-color);
  }
  .header h2 {
    margin: 0;
    font-size: 18px;
  }
  .close {
    background: none;
    border: none;
    font-size: 24px;
    cursor: pointer;
    color: var(--text-secondary);
    padding: 0 4px;
    line-height: 1;
  }
  .close:hover {
    color: var(--text-primary);
  }
  .loading,
  .empty {
    padding: 40px;
    text-align: center;
    color: var(--text-secondary);
  }
  .table-header {
    display: flex;
    padding: 10px 20px;
    border-bottom: 1px solid var(--border-color);
    font-size: 12px;
    color: var(--text-secondary);
    text-transform: uppercase;
    letter-spacing: 0.5px;
  }
  .list-wrap {
    flex: 1;
    overflow: hidden;
  }
  .row {
    display: flex;
    align-items: center;
    padding: 10px 20px;
    border-bottom: 1px solid var(--border-color);
    transition: background 0.15s;
  }
  .row:hover {
    background: var(--bg-tertiary);
  }
  .col-name {
    flex: 1;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    padding-right: 12px;
  }
  .col-size {
    width: 80px;
    text-align: right;
    padding-right: 12px;
    color: var(--text-secondary);
  }
  .col-date {
    width: 160px;
    color: var(--text-secondary);
    font-size: 13px;
  }
  .col-action {
    width: 80px;
    text-align: right;
  }
  .revoke {
    padding: 4px 12px;
    border: 1px solid #ef4444;
    background: transparent;
    color: #ef4444;
    border-radius: 6px;
    cursor: pointer;
    font-size: 13px;
    transition: all 0.15s;
  }
  .revoke:hover {
    background: #ef4444;
    color: #fff;
  }
</style>