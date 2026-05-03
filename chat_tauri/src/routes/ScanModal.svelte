<script lang="ts">
    import { _ } from "svelte-i18n";

    let {
        show = $bindable(false),
        isScanning = false,
        openFilePicker = () => {},
    }: {
        show?: boolean;
        isScanning?: boolean;
        openFilePicker?: () => void;
    } = $props();

    const close = () => {
        show = false;
    };
</script>

{#if show}
    <div
        class="modal-overlay"
        onclick={(e) => e.target === e.currentTarget && close()}
        onkeydown={(e) => e.key === "Escape" && close()}
        role="dialog"
        aria-label="扫码对话框"
        tabindex="0"
    >
        <div
            class="modal-content scan-modal-content"
            role="document"
            aria-label="扫码表单"
        >
            <h3>{$_("scan_qr_code")}</h3>
            <p class="scan-hint">{$_("scan_qr_hint")}</p>
            <div class="scan-actions">
                <button
                    type="button"
                    class="action-btn scan-action-btn"
                    onclick={openFilePicker}
                    disabled={isScanning}
                >
                    <svg
                        width="24"
                        height="24"
                        viewBox="0 0 24 24"
                        fill="none"
                        stroke="currentColor"
                        stroke-width="2"
                    >
                        <path
                            d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z"
                        />
                        <polyline points="14 2 14 8 20 8" />
                        <line x1="12" y1="18" x2="12" y2="12" />
                        <line x1="9" y1="15" x2="15" y2="15" />
                    </svg>
                    {$_("scan_qr_desktop")}
                </button>
            </div>
            {#if isScanning}
                <div class="scanning-indicator">{$_("loading")}...</div>
            {/if}
            <div class="modal-actions">
                <button type="button" class="btn-cancel" onclick={close}>
                    取消
                </button>
            </div>
        </div>
    </div>
{/if}

<style>
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

    .scan-modal-content {
        text-align: center;
    }

    .scan-hint {
        font-size: 13px;
        color: var(--text-secondary);
        margin-bottom: 20px;
    }

    .scan-actions {
        display: flex;
        justify-content: center;
        gap: 12px;
        margin-bottom: 16px;
    }

    .scan-action-btn {
        display: flex;
        align-items: center;
        gap: 8px;
        padding: 16px 24px;
        border: 2px dashed var(--border-color);
        border-radius: 12px;
        background: var(--bg-tertiary);
        color: var(--text-primary);
        cursor: pointer;
        font-size: 14px;
        transition: all 0.2s;
        width: 100%;
        justify-content: center;
    }

    .scan-action-btn:hover:not(:disabled) {
        border-color: #3b82f6;
        color: #3b82f6;
        background: var(--bg-secondary);
    }

    .scan-action-btn:disabled {
        opacity: 0.5;
        cursor: not-allowed;
    }

    .scanning-indicator {
        padding: 12px;
        color: var(--text-secondary);
        font-size: 14px;
    }

    .modal-actions {
        display: flex;
        justify-content: flex-end;
        gap: 12px;
        margin-top: 24px;
    }

    .btn-cancel {
        padding: 8px 16px;
        border-radius: 6px;
        cursor: pointer;
        font-size: 14px;
        transition: all 0.2s;
        background: transparent;
        border: 1px solid var(--border-color);
        color: var(--text-secondary);
    }

    .btn-cancel:hover {
        background: var(--bg-tertiary);
        color: var(--text-primary);
    }
</style>
