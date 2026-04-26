<script lang="ts">
    import { invoke } from "@tauri-apps/api/core";
    import { slide } from "svelte/transition";
    import "../lib/i18n";
    import { _, locale } from "svelte-i18n";
    let {
        onsend,
        disabled = false,
        fill = false,
        mldsaPubkeyHex = "",
    } = $props<{
        onsend?: (t: string) => void;
        disabled?: boolean;
        fill?: boolean;
        mldsaPubkeyHex?: string;
    }>();

    let text = $state("");
    let sending = $state(false);
    let err = $state("");
    let ok = $state(false);
    let area: HTMLTextAreaElement;

    async function submit() {
        if (!text.trim() || sending || disabled || !mldsaPubkeyHex) return;
        sending = true;
        err = "";
        try {
            await invoke("send", {
                mldsaPubkeyHex: mldsaPubkeyHex,
                message: text.trim(),
            });
            onsend?.(text.trim());
            text = "";
            ok = true;
            setTimeout(() => (ok = false), 2000);
        } catch (e) {
            err = String(e);
        } finally {
            sending = false;
            area.style.height = "auto";
        }
    }

    function resize(node: HTMLTextAreaElement) {
        const fn = () =>
            (node.style.height = Math.min(node.scrollHeight, 300) + "px");
        node.addEventListener("input", fn);
        return { destroy: () => node.removeEventListener("input", fn) };
    }

    const onKey = (e: KeyboardEvent) => {
        if (e.ctrlKey && e.key === "Enter") e.preventDefault(), submit();
    };
</script>

<div class="wrap" class:disabled class:fill>
    {#if disabled}
        <div class="mask"><p>{$_("select_contact_to_chat")}</p></div>
    {/if}

    <div class="badge">
        <svg
            width="12"
            height="12"
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            stroke-width="2"
        >
            <rect x="3" y="11" width="18" height="11" rx="2" />
            <path d="M7 11V7a5 5 0 0 1 10 0v4" />
        </svg>
        <span>E2EE</span>
    </div>

    <form onsubmit={(e) => (e.preventDefault(), submit())} class="row">
        <div class="box">
            <textarea
                bind:this={area}
                bind:value={text}
                use:resize
                placeholder={disabled ? "请先选择联系人..." : "输入消息..."}
                maxlength="4096"
                rows="1"
                disabled={sending || disabled}
                onkeydown={onKey}
            ></textarea>
            <div class="hints" class:disabled>
                <span>Ctrl+Enter</span>
                <span class:warn={text.length > 3500}>{text.length}/4096</span>
            </div>
        </div>

        <button
            type="submit"
            class="btn"
            class:loading={sending}
            disabled={!text.trim() || sending || disabled}
        >
            {#if sending}
                <svg class="spin" width="20" height="20" viewBox="0 0 24 24">
                    <circle
                        cx="12"
                        cy="12"
                        r="10"
                        stroke="currentColor"
                        stroke-width="2"
                        fill="none"
                        stroke-dasharray="32"
                    />
                </svg>
            {:else}
                <svg
                    width="20"
                    height="20"
                    viewBox="0 0 24 24"
                    fill="none"
                    stroke="currentColor"
                    stroke-width="2"
                >
                    <line x1="22" y1="2" x2="11" y2="13" /><polygon
                        points="22,2 15,22 11,13 2,9"
                    />
                </svg>
            {/if}
        </button>
    </form>

    {#if err}
        <div class="toast err" transition:slide>{err}</div>
    {:else if ok}
        <div class="toast ok" transition:slide>已发送</div>
    {/if}
</div>

<style>
    .wrap {
        position: relative;
        background: transparent;
        backdrop-filter: blur(10px);
        border: 1px solid var(--border-color, #2a2a2a);
        border-radius: 12px;
        padding: 12px 16px;
    }
    .wrap.fill {
        width: 100%;
        height: 100%;
        display: flex;
        flex-direction: column;
    }
    .wrap.fill .row {
        flex: 1;
        align-items: stretch;
    }
    .wrap.fill .box {
        display: flex;
        flex-direction: column;
    }
    .wrap.fill textarea {
        flex: 1;
        max-height: none;
    }
    .wrap.disabled {
        opacity: 0.6;
    }

    .mask {
        position: absolute;
        inset: 0;
        display: grid;
        place-items: center;
        background: rgba(15, 15, 15, 0.8);
        border-radius: 12px;
        color: #525252;
        font-size: 14px;
        z-index: 10;
    }

    .badge {
        display: inline-flex;
        align-items: center;
        gap: 4px;
        margin-bottom: 8px;
        padding: 2px 8px;
        background: rgba(34, 197, 94, 0.1);
        border-radius: 12px;
        color: #22c55e;
        font-size: 11px;
        font-weight: 600;
    }

    .row {
        display: flex;
        gap: 12px;
        align-items: flex-end;
    }
    .box {
        flex: 1;
        background: var(--bg-primary, #0f0f0f);
        border: 1px solid #333;
        border-radius: 8px;
        padding: 10px 12px;
    }
    .box:focus-within {
        border-color: #3b82f6;
    }

    textarea {
        width: 100%;
        min-height: 24px;
        max-height: 300px;
        background: transparent;
        border: none;
        color: var(--text-primary, #fafafa);
        font-size: 15px;
        resize: none;
        outline: none;
    }
    textarea::placeholder {
        color: var(--text-secondary, #525252);
    }
    textarea:disabled {
        cursor: not-allowed;
    }

    .hints {
        display: flex;
        justify-content: space-between;
        margin-top: 6px;
        font-size: 11px;
        color: var(--text-secondary, #525252);
        font-family: monospace;
    }
    .hints.disabled {
        opacity: 0.5;
    }
    .hints .warn {
        color: #ef4444;
        font-weight: 600;
    }

    .btn {
        width: 40px;
        height: 40px;
        display: grid;
        place-items: center;
        background: #3b82f6;
        border: none;
        border-radius: 8px;
        color: white;
        cursor: pointer;
        transition: all 0.2s;
        flex-shrink: 0;
    }
    .btn:hover:not(:disabled) {
        background: #2563eb;
        transform: translateY(-1px);
    }
    .btn:disabled {
        opacity: 0.4;
        cursor: not-allowed;
    }
    .btn.loading {
        background: #404040;
    }

    .spin {
        animation: spin 1s linear infinite;
    }
    @keyframes spin {
        to {
            transform: rotate(360deg);
        }
    }

    .toast {
        margin-top: 8px;
        padding: 8px 12px;
        border-radius: 6px;
        font-size: 13px;
    }
    .toast.err {
        background: rgba(239, 68, 68, 0.1);
        color: #ef4444;
    }
    .toast.ok {
        background: rgba(34, 197, 94, 0.1);
        color: #22c55e;
    }
</style>
