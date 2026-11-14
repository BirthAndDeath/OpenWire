<script lang="ts">
    import { _ } from "svelte-i18n";
    import { tt } from "../lib/i18n";
    import { invoke } from "@tauri-apps/api/core";
    import { decodeQrCode } from "../lib/qrcode";
    import { getSetting, setSetting } from "../lib/settings";
    import ScanModal from "./ScanModal.svelte";

    let {
        show = $bindable(false),
        currentIdentityId = "",
        onFriendAdded = () => {},
    }: {
        show?: boolean;
        currentIdentityId?: string;
        onFriendAdded?: () => void;
    } = $props();

    let newFriendPubkeyIdentityId = $state("");
    let newFriendName = $state("");
    let newFriendMlkemPubkey = $state("");
    let showScanModal = $state(false);
    let warning = $state("");
    // 扫码结果（完整文本，模态内联展示：ok/err）
    let scanResult = $state<{ kind: "ok" | "err"; text: string } | null>(null);

    // 最近添加记录（应用自存，非 WebView 记忆；可复用/可清空）
    type AddHistoryEntry = {
        kind: "pubkey" | "digest";
        value: string;
        name?: string;
        ts: number;
    };
    const ADD_HISTORY_KEY = "add_history";
    const ADD_HISTORY_MAX = 20;
    let history = $state<AddHistoryEntry[]>([]);
    let historyOpen = $state(false);

    const loadAddHistory = async () => {
        history = (await getSetting<AddHistoryEntry[]>(ADD_HISTORY_KEY)) ?? [];
    };

    const recordAddHistory = async (entry: Omit<AddHistoryEntry, "ts">) => {
        const list = (await getSetting<AddHistoryEntry[]>(ADD_HISTORY_KEY)) ?? [];
        const next = [
            { ...entry, ts: Date.now() },
            ...list.filter((e) => e.value !== entry.value),
        ].slice(0, ADD_HISTORY_MAX);
        await setSetting(ADD_HISTORY_KEY, next);
        history = next;
    };

    const clearAddHistory = async () => {
        await setSetting(ADD_HISTORY_KEY, []);
        history = [];
    };

    // 点历史项：回填公钥/摘要 + 昵称，收起面板
    const applyHistoryEntry = (entry: AddHistoryEntry) => {
        newFriendPubkeyIdentityId = entry.value;
        if (entry.name) newFriendName = entry.name;
        historyOpen = false;
    };

    // 打开模态时加载历史
    $effect(() => {
        if (show) void loadAddHistory();
    });

    // 本模态关闭时同步收起扫码子模态，避免其内相机继续占用（ScanModal 内 show-effect 负责释放流）
    $effect(() => {
        if (!show) showScanModal = false;
    });

    const showWarning = (message: string, duration: number = 5000) => {
        warning = message;
        console.warn("Warning:", message);
        setTimeout(() => {
            warning = "";
        }, duration);
    };

    let scanFileInput: HTMLInputElement | undefined = $state();
    let isScanning = $state(false);

    // 扫码落地超时：核心无响应时不能把"扫码中"永久卡住（用户可重试/关闭）
    const SCAN_TIMEOUT_MS = 20000;
    const withTimeout = <T,>(p: Promise<T>, ms: number): Promise<T | undefined> =>
        Promise.race([
            p,
            new Promise<undefined>((resolve) => setTimeout(() => resolve(undefined), ms)),
        ]);

    const handleScanFile = async (e: Event) => {
        const input = e.target as HTMLInputElement;
        const file = input.files?.[0];
        if (!file) return;

        isScanning = true;
        try {
            const bitmap = await createImageBitmap(file);
            const maxOriginal = Math.max(bitmap.width, bitmap.height);
            const canvas = document.createElement("canvas");
            const ctx = canvas.getContext("2d");
            if (!ctx) {
                showWarning(tt("err_canvas_context"));
                return;
            }

            // 多尺度解码：1024 抗屏幕摩尔纹、速度快；2048 覆盖"码占画面比例小"的密集码；
            // 仍失败则回退原图尺寸（封顶 4096，避免超大图 getImageData 在主线程拷贝数百 MB 造成冻结）。
            // jsQR 在 Web Worker 中执行，原图解码不阻塞主线程（worker 不可用时同步路径会先降采样兜底）。
            let result: string | null = null;
            const tried = new Set<number>();
            for (const maxDim of [1024, 2048, Math.min(maxOriginal, 4096)]) {
                const effective = Math.min(maxDim, maxOriginal);
                if (tried.has(effective)) continue;
                tried.add(effective);
                const scale = effective / maxOriginal;
                const w = Math.max(1, Math.round(bitmap.width * scale));
                const h = Math.max(1, Math.round(bitmap.height * scale));
                canvas.width = w;
                canvas.height = h;
                ctx.drawImage(bitmap, 0, 0, w, h);
                const imageData = ctx.getImageData(0, 0, w, h);
                result = await decodeQrCode(imageData);
                if (result) break;
            }
            bitmap.close();

            if (!result) {
                showWarning(tt("scan_fail_unclear"));
                return;
            }
            await processScanResult(result);
        } catch (e) {
            console.error("扫码失败:", e);
            showWarning(tt("scan_failed", { values: { error: e } }));
        } finally {
            isScanning = false;
            if (scanFileInput) {
                scanFileInput.value = "";
            }
        }
    };

    // 解码后的二维码内容统一处理：v2 二进制（0x02）→ 后端解码暂存；否则按原始公钥字节 → hex 直填
    const processScanResult = async (text: string) => {
        scanResult = null;
        const bytes: number[] = [];
        for (let i = 0; i < text.length; i++) {
            bytes.push(text.charCodeAt(i));
        }

        // v2 二进制格式：以 0x02 开头（载荷精简，解码在核心侧完成）
        if (bytes.length > 0 && bytes[0] === 0x02) {
            try {
                const digest = await withTimeout(
                    invoke<string | null>("add_contact_by_scan_binary", { bytes }),
                    SCAN_TIMEOUT_MS,
                );
                if (digest === undefined) {
                    scanResult = {
                        kind: "err",
                        text: tt("add_friend_failed_error", { values: { error: "timeout" } }),
                    };
                    return;
                }
                if (digest) {
                    // 保持扫码模态打开，内联展示完整结果，用户确认后手动关闭
                    scanResult = {
                        kind: "ok",
                        text: tt("scan_success_add_pending"),
                    };
                    await recordAddHistory({
                        kind: "digest",
                        value: digest as string,
                        name: newFriendName.trim() || undefined,
                    });
                } else {
                    scanResult = { kind: "err", text: tt("qr_parse_failed") };
                }
            } catch (e) {
                scanResult = {
                    kind: "err",
                    text: tt("add_friend_failed_error", { values: { error: e } }),
                };
            }
            return;
        }

        // 旧格式：原始公钥字节 → hex
        const hexBytes: string[] = [];
        for (const b of bytes) {
            hexBytes.push(b.toString(16).padStart(2, "0"));
        }
        const mldsaHex = hexBytes.join("");

        if (mldsaHex) {
            if (mldsaHex === currentIdentityId) {
                scanResult = { kind: "err", text: tt("cannot_add_self") };
                return;
            }
            scanResult = { kind: "ok", text: tt("scan_success") };
            newFriendPubkeyIdentityId = mldsaHex;
            showScanModal = false;
        } else {
            scanResult = { kind: "err", text: tt("qr_missing_pubkey") };
        }
    };

    // 相机扫码结果：与图片扫描共用同一处理管线
    const handleCameraResult = async (text: string) => {
        await processScanResult(text);
    };

    const openScanner = () => {
        // 全平台统一走 ScanModal：桌面用取景器/文件，移动端用原生拍照/文件
        scanResult = null;
        showScanModal = true;
    };

    const openFilePicker = () => {
        scanResult = null;
        if (scanFileInput) {
            scanFileInput.removeAttribute("capture");
            scanFileInput.click();
        }
    };

    // 移动端原生相机拍照（capture 属性让文件选择直接调起系统相机）
    const openCameraCapture = () => {
        scanResult = null;
        if (scanFileInput) {
            scanFileInput.setAttribute("capture", "environment");
            scanFileInput.click();
        }
    };

    // ScanModal 手动输入兜底：当作 ML-DSA 公钥 hex 直填添加
    const handleManualContent = async (content: string) => {
        scanResult = null;
        const c = content.trim();
        if (!c) {
            showWarning(tt("input_pubkey_hint"));
            return;
        }
        newFriendPubkeyIdentityId = c;
        await addFriend();
    };

    const addFriend = async () => {
        if (!newFriendPubkeyIdentityId.trim()) {
            showWarning(tt("input_pubkey_hint"));
            return;
        }

        try {
            const hex = newFriendPubkeyIdentityId.replace(/\s/g, "");
            if (hex.length % 2 !== 0) {
                showWarning(tt("pubkey_invalid_even"));
                return;
            }

            if (!/^[0-9a-fA-F]+$/.test(hex)) {
                showWarning(tt("pubkey_invalid_chars"));
                return;
            }

            if (hex === currentIdentityId) {
                showWarning(tt("cannot_add_self"), 5000);
                return;
            }

            let mlkemHex: string | undefined;
            const mlkemInput = newFriendMlkemPubkey.replace(/\s/g, "");
            if (mlkemInput) {
                if (mlkemInput.length % 2 !== 0) {
                    showWarning(tt("mlkem_invalid_even"));
                    return;
                }
                if (!/^[0-9a-fA-F]+$/.test(mlkemInput)) {
                    showWarning(tt("mlkem_invalid_chars"));
                    return;
                }
                mlkemHex = mlkemInput;
            }

            // SHA256 摘要（64 hex）：与扫码同款，客户端无完整公钥无法直接加密，
            // 走"扫码式暂存"，等对方上线后由 FriendOnline 携带的完整公钥回填接入。
            if (hex.length === 64) {
                const ok = await withTimeout(
                    invoke<boolean>("add_contact_by_scan", {
                        info: {
                            digestHex: hex.toLowerCase(),
                            peerId: undefined,
                            addrs: [],
                            relayAddrs: [],
                            name: newFriendName.trim() || undefined,
                            version: 1,
                        },
                    }),
                    SCAN_TIMEOUT_MS,
                );
                if (ok) {
                    const nameSnapshot = newFriendName.trim() || undefined;
                    showWarning(tt("scan_success_add_pending"), 5000);
                    show = false;
                    newFriendPubkeyIdentityId = "";
                    newFriendName = "";
                    await recordAddHistory({
                        kind: "digest",
                        value: hex.toLowerCase(),
                        name: nameSnapshot,
                    });
                    onFriendAdded();
                } else {
                    showWarning(tt("add_friend_failed"));
                }
                return;
            }

            const success = await withTimeout(
                invoke<boolean>("add_contact", {
                    mldsaPubkeyHex: hex,
                    name: newFriendName.trim() || undefined,
                    mlkemPubkeyHex: mlkemHex,
                }),
                SCAN_TIMEOUT_MS,
            );
            if (success) {
                const nameSnapshot = newFriendName.trim() || undefined;
                showWarning(tt("friend_added_success"));
                show = false;
                newFriendPubkeyIdentityId = "";
                newFriendName = "";
                newFriendMlkemPubkey = "";
                await recordAddHistory({
                    kind: "pubkey",
                    value: hex.toLowerCase(),
                    name: nameSnapshot,
                });
                onFriendAdded();
            } else {
                showWarning(tt("add_friend_failed"));
            }
        } catch (e) {
            showWarning(tt("add_friend_failed_error", { values: { error: e } }));
        }
    };

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
        aria-label={$_("add_friend_dialog")}
        tabindex="0"
    >
        <div class="modal-content" role="document" aria-label={$_("add_friend_form")}>
            <div class="modal-header">
                <h3>{$_("add_friend")}</h3>
                <button
                    type="button"
                    class="history-btn"
                    onclick={() => (historyOpen = !historyOpen)}
                    title={$_("add_history_hint")}
                    aria-label={$_("add_history")}
                    aria-expanded={historyOpen}
                >
                    <svg
                        width="18"
                        height="18"
                        viewBox="0 0 24 24"
                        fill="none"
                        stroke="currentColor"
                        stroke-width="2"
                    >
                        <circle cx="12" cy="12" r="9" />
                        <polyline points="12 7 12 12 15.5 14" />
                    </svg>
                </button>
            </div>
            {#if historyOpen}
                <div class="history-panel">
                    {#if history.length === 0}
                        <p class="history-empty">{$_("add_history_empty")}</p>
                    {:else}
                        <ul class="history-list">
                            {#each history as entry (entry.value)}
                                <li>
                                    <button
                                        type="button"
                                        class="history-item"
                                        onclick={() => applyHistoryEntry(entry)}
                                        title={$_("add_history_apply")}
                                    >
                                        <span class="history-kind"
                                            >{entry.kind === "digest"
                                                ? $_("add_history_digest")
                                                : $_("add_history_pubkey")}</span
                                        >
                                        <code class="history-value"
                                            >{entry.value.slice(0, 16)}…</code
                                        >
                                        {#if entry.name}
                                            <span class="history-name">{entry.name}</span>
                                        {/if}
                                    </button>
                                </li>
                            {/each}
                        </ul>
                        <button
                            type="button"
                            class="history-clear"
                            onclick={clearAddHistory}
                        >
                            {$_("add_history_clear")}
                        </button>
                    {/if}
                </div>
            {/if}
            <div class="form-group">
                <label for="pubkey-identity-id">{$_("pubkey_identity_id_label")}</label>
                <input
                    id="pubkey-identity-id"
                    type="text"
                    placeholder={$_("pubkey_placeholder")}
                    autocomplete="off"
                    autocapitalize="off"
                    autocorrect="off"
                    spellcheck="false"
                    bind:value={newFriendPubkeyIdentityId}
                />
                <small>{$_("pubkey_helper")}</small>
            </div>
            <div class="form-group">
                <label for="friend-name">{$_("name")} ({$_("optional")})</label>
                <input
                    id="friend-name"
                    type="text"
                    placeholder={$_("friend_name_placeholder")}
                    autocomplete="off"
                    autocapitalize="off"
                    autocorrect="off"
                    spellcheck="false"
                    bind:value={newFriendName}
                />
            </div>
            <div class="form-group">
                <label for="friend-mlkem-pubkey">{$_("mlkem_pubkey_label")}</label>
                <input
                    id="friend-mlkem-pubkey"
                    type="text"
                    placeholder={$_("mlkem_placeholder")}
                    autocomplete="off"
                    autocapitalize="off"
                    autocorrect="off"
                    spellcheck="false"
                    bind:value={newFriendMlkemPubkey}
                />
                <small>{$_("mlkem_helper")}</small>
            </div>
            {#if warning}
                <div class="warning-msg">{warning}</div>
            {/if}
            <div class="modal-actions">
                <button type="button" class="btn-cancel" onclick={close}>
                    {$_("action_cancel")}
                </button>
                <button type="button" class="btn-scan" onclick={openScanner}>
                    <svg
                        width="16"
                        height="16"
                        viewBox="0 0 24 24"
                        fill="none"
                        stroke="currentColor"
                        stroke-width="2"
                        style="vertical-align: middle; margin-right: 4px;"
                    >
                        <path d="M3 7V5a2 2 0 0 1 2-2h2" />
                        <path d="M17 3h2a2 2 0 0 1 2 2v2" />
                        <path d="M21 17v2a2 2 0 0 1-2 2h-2" />
                        <path d="M7 21H5a2 2 0 0 1-2-2v-2" />
                        <rect x="7" y="9" width="10" height="6" rx="1" />
                    </svg>
                    {$_("scan_qr")}
                </button>
                <button type="button" class="btn-confirm" onclick={addFriend}>
                    {$_("add_friend")}
                </button>
            </div>
        </div>
    </div>
{/if}

<!-- 隐藏的文件输入（用于扫码） -->
<input
    type="file"
    accept="image/*"
    bind:this={scanFileInput}
    onchange={handleScanFile}
    style="display: none"
/>

<!-- 扫码模态框 -->
<ScanModal
    bind:show={showScanModal}
    {isScanning}
    {openFilePicker}
    {openCameraCapture}
    onManualSubmit={handleManualContent}
    onCameraResult={handleCameraResult}
    {scanResult}
/>

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

    .modal-header {
        display: flex;
        align-items: center;
        justify-content: space-between;
        margin-bottom: 20px;
    }
    .modal-header h3 {
        margin: 0;
    }
    .history-btn {
        background: transparent;
        border: none;
        color: var(--text-secondary, #737373);
        cursor: pointer;
        padding: 4px;
        border-radius: 6px;
        transition: color 0.2s;
    }
    .history-btn:hover {
        color: var(--color-primary, #3b82f6);
        background: var(--bg-tertiary, #1a1a1a);
    }
    .history-panel {
        border: 1px solid var(--border-color, #2a2a2a);
        border-radius: 8px;
        background: var(--bg-primary, #141414);
        padding: 8px;
        margin-bottom: 16px;
    }
    .history-list {
        list-style: none;
        margin: 0;
        padding: 0;
        max-height: 180px;
        overflow-y: auto;
    }
    .history-item {
        display: flex;
        align-items: center;
        gap: 8px;
        width: 100%;
        padding: 8px 10px;
        background: none;
        border: none;
        border-radius: 6px;
        color: var(--text-primary, #fafafa);
        cursor: pointer;
        font: inherit;
        text-align: left;
    }
    .history-item:hover {
        background: var(--bg-tertiary, #1a1a1a);
    }
    .history-kind {
        font-size: 11px;
        color: var(--text-secondary, #737373);
        flex-shrink: 0;
    }
    .history-value {
        font-family: "Courier New", monospace;
        font-size: 12px;
        color: #60a5fa;
        overflow: hidden;
        text-overflow: ellipsis;
        white-space: nowrap;
        min-width: 0;
    }
    .history-name {
        font-size: 12px;
        color: var(--text-secondary, #737373);
        flex-shrink: 0;
    }
    .history-empty {
        margin: 0;
        padding: 8px 4px;
        font-size: 13px;
        color: var(--text-secondary, #737373);
    }
    .history-clear {
        margin-top: 4px;
        width: 100%;
        padding: 6px;
        background: none;
        border: none;
        border-top: 1px solid var(--border-color, #2a2a2a);
        color: var(--color-danger, #ef4444);
        cursor: pointer;
        font-size: 12px;
    }
    .history-clear:hover {
        opacity: 0.8;
    }

    .form-group {
        margin-bottom: 16px;
    }

    .form-group label {
        display: block;
        margin-bottom: 6px;
        color: var(--text-secondary);
        font-size: 14px;
    }

    .form-group input {
        width: 100%;
        padding: 10px;
        background: var(--bg-primary);
        border: 1px solid var(--border-color);
        border-radius: 6px;
        color: var(--text-primary);
        font-family: inherit;
        box-sizing: border-box;
    }

    .form-group input:focus {
        outline: none;
        border-color: var(--color-primary);
    }

    .warning-msg {
        color: var(--color-danger);
        font-size: 13px;
        margin-top: 8px;
        padding: 8px;
        background: var(--color-danger-soft);
        border-radius: 4px;
    }

    .modal-actions {
        display: flex;
        justify-content: flex-end;
        gap: 12px;
        margin-top: 24px;
    }

    .btn-cancel,
    .btn-confirm,
    .btn-scan {
        padding: 8px 16px;
        border-radius: 6px;
        cursor: pointer;
        font-size: 14px;
        transition: all 0.2s;
    }

    .btn-cancel {
        background: transparent;
        border: 1px solid var(--border-color);
        color: var(--text-secondary);
    }

    .btn-cancel:hover {
        background: var(--bg-tertiary);
        color: var(--text-primary);
    }

    .btn-confirm {
        background: var(--color-primary);
        border: 1px solid var(--color-primary);
        color: white;
    }

    .btn-confirm:hover {
        background: var(--color-primary-strong);
    }

    .btn-scan {
        background: transparent;
        border: 1px solid var(--color-success);
        color: var(--color-success);
    }

    .btn-scan:hover {
        background: var(--color-success-soft);
    }

    @media (width <= 480px) {
        .modal-content {
            width: 100%;
            max-width: 100%;
            height: 100dvh;
            border-radius: 0;
            display: flex;
            flex-direction: column;
            padding: 16px;
            padding-top: calc(16px + var(--safe-area-top));
            padding-bottom: calc(16px + var(--safe-area-bottom));
        }
        .modal-actions {
            margin-top: auto;
            padding-top: 16px;
        }
        .btn-cancel,
        .btn-confirm,
        .btn-scan {
            flex: 1;
            text-align: center;
            padding: 12px 16px;
            font-size: 16px;
        }
        .form-group input {
            font-size: 16px;
            padding: 12px;
        }
    }
</style>
