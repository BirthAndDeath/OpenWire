<script lang="ts">
    import { invoke } from "@tauri-apps/api/core";
    import { listen, type UnlistenFn } from "@tauri-apps/api/event";
    import { onMount, tick } from "svelte";
    import { slide } from "svelte/transition";
    import { _ } from "svelte-i18n";
    import { tt } from "../lib/i18n";
    import { hasQuoteRef, quotePlainText } from "../lib/quote";
    import Spinner from "../lib/components/Spinner.svelte";
    import { renderMarkdown } from "../lib/markdown";

    let {
        onsend,
        disabled = false,
        fill = false,
        compact = false,
        mldsaPubkeyHex = "",
        quote = $bindable(""),
    }: {
        onsend?: (t: string) => void;
        disabled?: boolean;
        fill?: boolean;
        /** 输入栏被压缩（高度不足以容纳徽标/提示行）时收起非必要元素 */
        compact?: boolean;
        mldsaPubkeyHex?: string;
        /** 外部注入的文本（如长按消息"引用"），非空时追加到输入框并清空 */
        quote?: string;
    } = $props();

    let text = $state("");
    let sending = $state(false);
    let err = $state("");
    let ok = $state(false);
    // Markdown 预览开关
    let preview = $state(false);
    // 预览渲染结果（防抖：打开预览立即渲染一次，输入中 200ms 合并，避免逐键全量解析）
    let previewHtml = $state("");
    let previewTimer: ReturnType<typeof setTimeout> | undefined = undefined;
    let previewWasOn = false;
    $effect(() => {
        if (!preview) {
            clearTimeout(previewTimer);
            return;
        }
        if (!previewWasOn) {
            previewHtml = text.trim() ? renderMarkdown(text) : "";
        }
        previewWasOn = preview;
        clearTimeout(previewTimer);
        previewTimer = setTimeout(() => {
            previewHtml = text.trim() ? renderMarkdown(text) : "";
        }, 200);
        return () => clearTimeout(previewTimer);
    });
    let area: HTMLTextAreaElement;

    // 输入框内的引用预览：取"引用块"（> 开头的行）剥掉内部标记，用于渲染与长引用省略号
    function quotePreviewOf(content: string): string | null {
        const quoted = content
            .split("\n")
            .filter((line) => line.startsWith("> "))
            .join("\n");
        if (!quoted) return null;
        return quotePlainText(quoted) || null;
    }

    // 引用注入：追加到当前输入内容末尾（引用块自带结尾空行）
    $effect(() => {
        if (!quote) return;
        text = text.trim() ? `${text.trim()}\n\n${quote}` : quote;
        quote = "";
        tick().then(() => {
            if (!area) return;
            // 与 resize 同口径：先重置再测量，删字/替换后能正确缩放回
            if (!fill) {
                area.style.height = "auto";
                area.style.height =
                    Math.min(
                        area.scrollHeight,
                        Math.min(480, Math.round((window.innerHeight ?? 720) * 0.5)),
                    ) + "px";
            } else {
                area.style.height = "auto";
            }
        });
    });

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

    // 文件发送（后端弹原生对话框选文件，前端只传接收方）
    // 文件发送结果（后端对话框异步）：成功补气泡，失败显示错误
    let unlistenFileResult: UnlistenFn | undefined;
    onMount(() => {
        listen<{ ok: boolean; filename?: string; error?: string }>(
            "file-send-result",
            (event) => {
                const r = event.payload;
                if (r.ok && r.filename) {
                    // "[文件] " 前缀是文件消息的 wire 格式（与后端解析保持一致），不参与本地化
                    onsend?.(`[文件] ${r.filename}`);
                    ok = true;
                    setTimeout(() => (ok = false), 2000);
                } else {
                    err = r.error ?? tt("file_send_failed");
                }
            },
        )
            .then((u) => (unlistenFileResult = u))
            .catch((e) => console.error("注册 file-send-result 监听失败:", e));
        return () => unlistenFileResult?.();
    });

    // 文件发送（后端弹原生对话框选文件，结果经 file-send-result 事件回传）
    async function sendFile() {
        if (sending || disabled || !mldsaPubkeyHex) return;
        try {
            sending = true;
            err = "";
            await invoke("send_file", { mldsaPubkeyHex: mldsaPubkeyHex });
            // 对话框已打开，结果经 file-send-result 事件回传
        } catch (e) {
            err = String(e);
        } finally {
            sending = false;
        }
    }

    function resize(node: HTMLTextAreaElement, fill = false) {
        // 用 requestAnimationFrame 合并同一帧内的多次 input 事件，避免重复布局计算
        let rafId: number | null = null;
        // 非填充模式（移动端）内容可扩展高度：上限随视口（短窗口不顶到顶部）
        const MAX_HEIGHT = Math.min(480, Math.round((window.innerHeight ?? 720) * 0.5));
        const fn = () => {
            if (rafId !== null) return; // 同一帧内只执行一次
            rafId = requestAnimationFrame(() => {
                rafId = null;
                // fill 模式由 flex 决定高度，跟随面板；非 fill 随内容自动扩展。
                // 关键：先重置 height:auto 再测量——否则 scrollHeight 被当前高度钳制
                // （≥ 当前高度），删字后内容变矮也缩不回去（只增不减）。
                if (!fill) {
                    node.style.height = "auto";
                    node.style.height = Math.min(node.scrollHeight, MAX_HEIGHT) + "px";
                }
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

<div class="wrap" class:disabled class:fill class:compact>
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
            {#if preview}
                <div class="preview-pane">
                    {#if previewHtml}
                        <div class="preview-content">{@html previewHtml}</div>
                    {:else}
                        <span class="preview-empty">{$_("preview_empty")}</span>
                    {/if}
                </div>
            {:else}
                {@const qText = quotePreviewOf(text)}
                {#if qText}
                    <div class="quote-bar" title={qText}>
                        <span class="quote-text">{qText}</span>
                    </div>
                {/if}
            {/if}
            <textarea
                bind:this={area}
                bind:value={text}
                use:resize={fill}
                name="message"
                placeholder={disabled
                    ? $_("input_placeholder_select")
                    : $_("input_placeholder_message")}
                maxlength="4096"
                rows="1"
                style={preview ? "display: none" : undefined}
                disabled={sending || disabled}
                onkeydown={onKey}
            ></textarea>
            <div class="hints" class:disabled>
                <span>Ctrl+Enter</span>
                <span class:warn={text.length > 3500}>{text.length}/4096</span>
            </div>
        </div>

        <div class="btn-group">
            <!-- Markdown 预览切换按钮 -->
            <button
                type="button"
                class="btn preview-btn"
                class:active={preview}
                disabled={disabled}
                onclick={() => (preview = !preview)}
                title={$_("markdown_preview")}
                aria-label={$_("markdown_preview")}
                aria-pressed={preview}
            >
                <svg
                    width="20"
                    height="20"
                    viewBox="0 0 24 24"
                    fill="none"
                    stroke="currentColor"
                    stroke-width="2"
                >
                    <path d="M2 3h20v18H2z" />
                    <path d="M2 7h20" />
                    <path d="M6 12h4" />
                    <path d="M6 16h8" />
                </svg>
            </button>

            <!-- 文件发送按钮 -->
            <button
                type="button"
                class="btn file-btn"
                class:loading={sending}
                disabled={sending || disabled}
                onclick={sendFile}
                title={$_("send_file")}
                aria-label={$_("send_file")}
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
                    <Spinner size={20} />
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
        <div class="toast ok" transition:slide>{$_("sent")}</div>
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
        min-height: 0; /* 允许行随面板高度收缩 */
        align-items: stretch;
    }
    .wrap.fill .box {
        display: flex;
        flex-direction: column;
        min-height: 0;
    }
    .wrap.fill textarea {
        flex: 1;
        max-height: none;
    }
    .wrap.disabled {
        opacity: 0.6;
    }

    /* 高度被压缩（拉动底栏向下）时收起非必要元素，让输入栏跟随面板自动收缩 */
    .wrap.fill.compact {
        padding: 6px 8px;
    }
    .wrap.fill.compact .badge {
        display: none;
    }
    .wrap.fill.compact .hints {
        display: none;
    }
    .wrap.fill.compact .box {
        padding: 5px 8px;
    }
    .wrap.fill.compact textarea {
        min-height: 22px;
    }
    .wrap.fill.compact .btn {
        width: 36px;
        height: 36px;
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
        border: 1px solid var(--border-color);
        border-radius: 8px;
        padding: 10px 12px;
    }
    .box:focus-within {
        border-color: var(--color-primary);
    }

    textarea {
        width: 100%;
        min-height: 24px;
        max-height: 480px;
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
        background: var(--color-primary);
    }
    .send-btn:hover:not(:disabled) {
        background: var(--color-primary-strong);
        transform: translateY(-1px);
    }
    .file-btn {
        background: var(--text-muted, #6b7280);
    }
    .file-btn:hover:not(:disabled) {
        background: var(--text-muted-strong, #4b5563);
        transform: translateY(-1px);
    }
    .preview-btn {
        background: var(--text-muted, #6b7280);
    }
    .preview-btn:hover:not(:disabled) {
        background: var(--text-muted-strong, #4b5563);
        transform: translateY(-1px);
    }
    .preview-btn.active {
        background: var(--color-primary);
    }
    .btn:disabled {
        opacity: 0.4;
        cursor: not-allowed;
    }
    .btn.loading {
        background: var(--text-muted, #404040);
    }

    /* 输入框内的引用预览：长引用自动省略号（最多 2 行） */
    .quote-bar {
        display: flex;
        align-items: flex-start;
        margin-bottom: 8px;
        padding: 6px 10px;
        border-left: 3px solid var(--color-primary);
        background: rgba(128, 128, 128, 0.12);
        border-radius: 6px;
        overflow: hidden;
    }
    .quote-text {
        font-size: 13px;
        line-height: 1.4;
        color: var(--text-secondary, #a3a3a3);
        white-space: pre-wrap;
        word-break: break-word;
        display: -webkit-box;
        -webkit-line-clamp: 2;
        line-clamp: 2;
        -webkit-box-orient: vertical;
        overflow: hidden;
    }

    /* Markdown 预览面板：样式与消息气泡一致 */
    .preview-pane {
        max-height: 220px;
        overflow-y: auto;
        padding: 10px 12px;
        margin-bottom: 8px;
        border-radius: 8px;
        border: 1px solid var(--border-color, #2a2a2a);
        background: var(--bg-tertiary, #1a1a1a);
    }
    .preview-content {
        font-size: 15px;
        line-height: 1.5;
        word-break: break-word;
    }
    .preview-content :global(p) {
        margin: 0 0 6px;
    }
    .preview-content :global(p:last-child) {
        margin-bottom: 0;
    }
    .preview-content :global(a) {
        color: inherit;
    }
    .preview-content :global(.md-link-url) {
        font-size: 11px;
        opacity: 0.6;
        word-break: break-all;
        margin-left: 2px;
    }
    .preview-content :global(code) {
        background: rgba(128, 128, 128, 0.25);
        border-radius: 4px;
        padding: 0 4px;
        font-size: 0.9em;
    }
    .preview-content :global(pre) {
        overflow-x: auto;
        margin: 6px 0;
    }
    .preview-content :global(blockquote) {
        border-left: 3px solid rgba(128, 128, 128, 0.5);
        margin: 4px 0;
        padding-left: 8px;
        opacity: 0.85;
    }
    .preview-empty {
        font-size: 13px;
        color: var(--text-secondary, #737373);
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
