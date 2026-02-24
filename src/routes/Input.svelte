<script lang="ts">
    import { invoke } from "@tauri-apps/api/core";
    import { slide } from "svelte/transition";

    // === Props ===
    let {
        onsend,
        disabled = false, // 新增：外部禁用控制
    } = $props();

    // === 状态 ===
    let content = $state("");
    let isSending = $state(false);
    let error = $state("");
    let success = $state(false);
    let inputRef: HTMLTextAreaElement;

    // === 发送逻辑 ===
    async function submit() {
        if (!content.trim() || isSending || disabled) return;

        isSending = true;
        error = "";
        success = false;

        try {
            await invoke("send", { message: content.trim() });
            onsend?.(content.trim());
            content = "";
            success = true;
            resetHeight();
            setTimeout(() => (success = false), 3000);
        } catch (err) {
            error = (err as Error).message;
        } finally {
            isSending = false;
        }
    }

    function resetHeight() {
        if (inputRef) inputRef.style.height = "auto";
    }

    function onKeydown(e: KeyboardEvent) {
        if ((e.ctrlKey || e.metaKey) && e.key === "Enter") {
            e.preventDefault();
            submit();
        }
    }

    function autoResize(node: HTMLTextAreaElement) {
        const resize = () => {
            node.style.height = "auto";
            node.style.height = Math.min(node.scrollHeight, 300) + "px";
        };
        node.addEventListener("input", resize);
        return { destroy: () => node.removeEventListener("input", resize) };
    }
</script>

<div class="input-wrapper" class:disabled>
    {#if disabled}
        <div class="overlay">选择联系人以开始聊天</div>
    {/if}

    <div class="security-badge">
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

    <form
        onsubmit={(e) => {
            e.preventDefault();
            submit();
        }}
        class="input-row"
    >
        <div class="input-box">
            <textarea
                bind:this={inputRef}
                bind:value={content}
                use:autoResize
                placeholder={disabled ? "请先选择联系人..." : "输入消息..."}
                maxlength="4096"
                rows="1"
                disabled={isSending || disabled}
                onkeydown={onKeydown}
            ></textarea>
            <div class="input-hints" class:disabled>
                <span>Ctrl + Enter</span>
                <span class:limit={content.length > 3500}>
                    {content.length}/4096
                </span>
            </div>
        </div>

        <button
            type="submit"
            class="send-btn"
            class:loading={isSending}
            disabled={!content.trim() || isSending || disabled}
        >
            {#if isSending}
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
                    <line x1="22" y1="2" x2="11" y2="13" />
                    <polygon points="22,2 15,22 11,13 2,9" />
                </svg>
            {/if}
        </button>
    </form>

    {#if error}
        <div class="toast error" transition:slide>{error}</div>
    {:else if success}
        <div class="toast success" transition:slide>已发送</div>
    {/if}
</div>

<style>
    .input-wrapper {
        position: relative;
        background: #1a1a1a;
        border: 1px solid #2a2a2a;
        border-radius: 12px;
        padding: 12px 16px;
        transition: opacity 0.2s;
    }

    .input-wrapper.disabled {
        opacity: 0.6;
    }

    .overlay {
        position: absolute;
        inset: 0;
        display: flex;
        align-items: center;
        justify-content: center;
        background: rgba(15, 15, 15, 0.8);
        border-radius: 12px;
        color: #525252;
        font-size: 14px;
        z-index: 10;
    }

    .security-badge {
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

    .input-row {
        display: flex;
        gap: 12px;
        align-items: flex-end;
    }

    .input-box {
        flex: 1;
        background: #0f0f0f;
        border: 1px solid #333;
        border-radius: 8px;
        padding: 10px 12px;
        transition: border-color 0.2s;
    }

    .input-box:focus-within {
        border-color: #3b82f6;
    }

    textarea {
        width: 100%;
        min-height: 24px;
        max-height: 300px;
        background: transparent;
        border: none;
        color: #fafafa;
        font-size: 15px;
        resize: none;
        outline: none;
    }

    textarea::placeholder {
        color: #525252;
    }

    textarea:disabled {
        cursor: not-allowed;
    }

    .input-hints {
        display: flex;
        justify-content: space-between;
        margin-top: 6px;
        font-size: 11px;
        color: #525252;
        font-family: monospace;
    }

    .input-hints.disabled {
        opacity: 0.5;
    }

    .input-hints .limit {
        color: #ef4444;
        font-weight: 600;
    }

    .send-btn {
        width: 40px;
        height: 40px;
        display: flex;
        align-items: center;
        justify-content: center;
        background: #3b82f6;
        border: none;
        border-radius: 8px;
        color: white;
        cursor: pointer;
        transition: all 0.2s;
        flex-shrink: 0;
    }

    .send-btn:hover:not(:disabled) {
        background: #2563eb;
        transform: translateY(-1px);
    }

    .send-btn:disabled {
        opacity: 0.4;
        cursor: not-allowed;
        transform: none;
    }

    .send-btn.loading {
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

    .toast.error {
        background: rgba(239, 68, 68, 0.1);
        color: #ef4444;
    }

    .toast.success {
        background: rgba(34, 197, 94, 0.1);
        color: #22c55e;
    }
</style>
