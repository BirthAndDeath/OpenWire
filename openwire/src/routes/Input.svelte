<script lang="ts">
    import { invoke } from "@tauri-apps/api/core";
    import { open } from "@tauri-apps/plugin-dialog";
    import { slide } from "svelte/transition";
    import "../lib/i18n";
    import { _ } from "svelte-i18n";
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
            // 不再在此处调用 invoke("send")，由父组件的 send() 统一处理
            // 父组件会调用 invoke("send") 并管理消息列表
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

    // 文件发送
    async function sendFile() {
        if (sending || disabled || !mldsaPubkeyHex) return;
        try {
            const selected = await open({
                multiple: false,
                filters: [],
            });
            if (!selected) return; // 用户取消选择
            sending = true;
            err = "";
            const filePath = selected as string;
            await invoke("send_file", {
                mldsaPubkeyHex: mldsaPubkeyHex,
                filePath: filePath,
            });
            onsend?.(`[文件] ${filePath.split(/[/\\]/).pop()}`);
            ok = true;
            setTimeout(() => (ok = false), 2000);
        } catch (e) {
            err = String(e);
        } finally {
            sending = false;
        }
    }

    function resize(node: HTMLTextAreaElement) {
        // 使用 requestAnimationFrame 批量处理，避免在同一个帧内多次触发 ResizeObserver
        let rafId: number | null = null;
        const fn = () => {
            if (rafId !== null) return; // 同一帧内只执行一次
            rafId = requestAnimationFrame(() => {
                rafId = null;
                node.style.height = Math.min(node.scrollHeight, 300) + "px";
            });
        };
        node.addEventListener("input", fn);
        return {
            destroy: () => {
                node.removeEventListener("input", fn);
                if (rafId !== null) cancelAnimationFrame(rafId);
            },
        };
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
                name="message"
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

        <div class="btn-group">
            <!-- 文件发送按钮 -->
            <button
                type="button"
                class="btn file-btn"
                class:loading={sending}
                disabled={sending || disabled}
                onclick={sendFile}
                title="发送文件"
                aria-label="发送文件"
            >
                <svg
                    width="20"
                    height="20"
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
                    <line x1="9" y1="15" x2="12" y2="12" />
                    <line x1="15" y1="15" x2="12" y2="12" />
                </svg>
            </button>

            <button
                type="submit"
                class="btn send-btn"
                class:loading={sending}
                disabled={!text.trim() || sending || disabled}
            >
                {#if sending}
                    <svg
                        class="spin"
                        width="20"
                        height="20"
                        viewBox="0 0 24 24"
                    >
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
        </div>
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
        box-sizing: border-box; /* width:100% 含 padding，防止整体溢出被裁 */
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
        min-width: 0; /* 允许 flex 收缩，防止 btn-group 溢出右侧被裁 */
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

    .btn-group {
        display: flex;
        gap: 8px;
        flex-shrink: 0;
    }

    .btn {
        width: 40px;
        height: 40px;
        display: grid;
        place-items: center;
        border: none;
        border-radius: 8px;
        color: white;
        cursor: pointer;
        transition: all 0.2s;
        flex-shrink: 0;
    }
    .send-btn {
        background: #3b82f6;
    }
    .send-btn:hover:not(:disabled) {
        background: #2563eb;
        transform: translateY(-1px);
    }
    .file-btn {
        background: #6b7280;
    }
    .file-btn:hover:not(:disabled) {
        background: #4b5563;
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

    @media (width <= 480px) {
        .wrap {
            border-radius: 0;
            border-left: none;
            border-right: none;
            border-bottom: none;
            padding: 8px 8px;
            padding-bottom: calc(8px + var(--safe-area-bottom));
        }
        .badge {
            display: none;
        }
        .row {
            gap: 6px;
        }
        .box {
            padding: 8px 10px;
        }
        textarea {
            font-size: 16px;
            min-height: 22px;
        }
        .btn {
            width: 44px;
            height: 44px;
        }
        .hints span:first-child {
            display: none;
        }
        .hints {
            margin-top: 4px;
            font-size: 10px;
        }
    }
</style>
