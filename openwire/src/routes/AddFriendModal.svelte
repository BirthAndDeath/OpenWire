<script lang="ts">
    import { _ } from "svelte-i18n";
    import { invoke } from "@tauri-apps/api/core";
    import { decodeQrCode } from "../lib/qrcode";
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

    const showWarning = (message: string, duration: number = 5000) => {
        warning = message;
        console.warn("Warning:", message);
        setTimeout(() => {
            warning = "";
        }, duration);
    };

    let scanFileInput: HTMLInputElement | undefined = $state();
    let isScanning = $state(false);

    const handleScanFile = async (e: Event) => {
        const input = e.target as HTMLInputElement;
        const file = input.files?.[0];
        if (!file) return;

        isScanning = true;
        try {
            const bitmap = await createImageBitmap(file);
            const canvas = document.createElement("canvas");
            canvas.width = bitmap.width;
            canvas.height = bitmap.height;
            const ctx = canvas.getContext("2d");
            if (!ctx) {
                showWarning("无法创建画布上下文");
                return;
            }
            ctx.drawImage(bitmap, 0, 0);
            const imageData = ctx.getImageData(
                0,
                0,
                bitmap.width,
                bitmap.height,
            );
            bitmap.close();

            const result = await decodeQrCode(imageData);
            if (result) {
                const bytes: number[] = [];
                for (let i = 0; i < result.length; i++) {
                    bytes.push(result.charCodeAt(i));
                }
                const hexBytes: string[] = [];
                for (const b of bytes) {
                    hexBytes.push(b.toString(16).padStart(2, "0"));
                }
                const mldsaHex = hexBytes.join("");

                if (mldsaHex) {
                    if (mldsaHex === currentIdentityId) {
                        showWarning("不能添加自己为好友", 5000);
                        showScanModal = false;
                        return;
                    }
                    newFriendPubkeyIdentityId = mldsaHex;
                    showWarning("扫码成功", 3000);
                    showScanModal = false;
                } else {
                    showWarning("二维码数据缺少公钥字段");
                }
            } else {
                showWarning("扫码失败，请确保二维码清晰完整且光线充足");
            }
        } catch (e) {
            console.error("扫码失败:", e);
            showWarning(`扫码失败：${e}`);
        } finally {
            isScanning = false;
            if (scanFileInput) {
                scanFileInput.value = "";
            }
        }
    };

    const openScanner = () => {
        const isMobile =
            typeof window !== "undefined" &&
            (navigator.userAgent.includes("Mobile") ||
                navigator.userAgent.includes("Android") ||
                navigator.userAgent.includes("iPhone"));

        if (isMobile) {
            if (scanFileInput) {
                scanFileInput.setAttribute("capture", "environment");
                scanFileInput.click();
            }
        } else {
            showScanModal = true;
        }
    };

    const openFilePicker = () => {
        if (scanFileInput) {
            scanFileInput.removeAttribute("capture");
            scanFileInput.click();
        }
    };

    const addFriend = async () => {
        if (!newFriendPubkeyIdentityId.trim()) {
            showWarning("请输入 Pubkey 身份 ID (ML-DSA公钥的hex编码)");
            return;
        }

        try {
            const hex = newFriendPubkeyIdentityId.replace(/\s/g, "");
            if (hex.length % 2 !== 0) {
                showWarning("Pubkey身份ID格式无效：长度必须是偶数");
                return;
            }

            if (!/^[0-9a-fA-F]+$/.test(hex)) {
                showWarning("Pubkey身份ID格式无效：只能包含0-9, a-f, A-F字符");
                return;
            }

            if (hex === currentIdentityId) {
                showWarning("不能添加自己为好友", 5000);
                return;
            }

            let mlkemHex: string | undefined;
            const mlkemInput = newFriendMlkemPubkey.replace(/\s/g, "");
            if (mlkemInput) {
                if (mlkemInput.length % 2 !== 0) {
                    showWarning("ML-KEM 公钥格式无效：长度必须是偶数");
                    return;
                }
                if (!/^[0-9a-fA-F]+$/.test(mlkemInput)) {
                    showWarning(
                        "ML-KEM 公钥格式无效：只能包含0-9, a-f, A-F字符",
                    );
                    return;
                }
                mlkemHex = mlkemInput;
            }

            const success: boolean = await invoke("add_contact", {
                mldsaPubkeyHex: hex,
                name: newFriendName.trim() || undefined,
                mlkemPubkeyHex: mlkemHex,
            });

            if (success) {
                showWarning("好友添加成功！");
                show = false;
                newFriendPubkeyIdentityId = "";
                newFriendName = "";
                newFriendMlkemPubkey = "";
                onFriendAdded();
            }
        } catch (e) {
            showWarning(`添加好友失败：${e}`);
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
        aria-label="添加好友对话框"
        tabindex="0"
    >
        <div class="modal-content" role="document" aria-label="添加好友表单">
            <h3>{$_("add_friend")}</h3>
            <div class="form-group">
                <label for="pubkey-identity-id">Pubkey 身份 ID *</label>
                <input
                    id="pubkey-identity-id"
                    type="text"
                    placeholder="ML-DSA公钥的hex编码 (例如: 1234abcd...)"
                    bind:value={newFriendPubkeyIdentityId}
                />
                <small>ML-DSA公钥的十六进制编码，作为唯一身份标识</small>
            </div>
            <div class="form-group">
                <label for="friend-name">{$_("name")} (可选)</label>
                <input
                    id="friend-name"
                    type="text"
                    placeholder="好友姓名"
                    bind:value={newFriendName}
                />
            </div>
            <div class="form-group">
                <label for="friend-mlkem-pubkey">ML-KEM 公钥 (可选)</label>
                <input
                    id="friend-mlkem-pubkey"
                    type="text"
                    placeholder="对方的ML-KEM公钥hex编码，留空则通过DHT自动查找"
                    bind:value={newFriendMlkemPubkey}
                />
                <small>如果对方已添加你为好友，可留空由系统自动查找</small>
            </div>
            {#if warning}
                <div class="warning-msg">{warning}</div>
            {/if}
            <div class="modal-actions">
                <button type="button" class="btn-cancel" onclick={close}>
                    取消
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
                    添加好友
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
<ScanModal bind:show={showScanModal} {isScanning} {openFilePicker} />

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
        border-color: #3b82f6;
    }

    .warning-msg {
        color: #ef4444;
        font-size: 13px;
        margin-top: 8px;
        padding: 8px;
        background: rgba(239, 68, 68, 0.1);
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
        background: #3b82f6;
        border: 1px solid #3b82f6;
        color: white;
    }

    .btn-confirm:hover {
        background: #2563eb;
    }

    .btn-scan {
        background: transparent;
        border: 1px solid #10b981;
        color: #10b981;
    }

    .btn-scan:hover {
        background: rgba(16, 185, 129, 0.1);
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
