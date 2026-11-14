<script lang="ts">
    import { _ } from "svelte-i18n";
    import { onDestroy } from "svelte";
    import { decodeQrCode } from "../lib/qrcode";

    let {
        show = $bindable(false),
        isScanning = false,
        openFilePicker = () => {},
        openCameraCapture = () => {},
        onManualSubmit = () => {},
        onCameraResult = () => {},
        scanResult = null,
    }: {
        show?: boolean;
        isScanning?: boolean;
        openFilePicker?: () => void;
        /** 系统相机拍照兜底（capture 文件输入）：getUserMedia 取景器不可用时启用 */
        openCameraCapture?: () => void;
        /** 手动输入（粘贴身份 ID hex / 二维码原始内容）后的回调 */
        onManualSubmit?: (content: string) => void;
        /** 相机扫描解码成功后的回调（二维码内容文本） */
        onCameraResult?: (content: string) => void;
        /** 扫码结果的完整文本（ok/err），由父级驱动，在模态内联展示 */
        scanResult?: { kind: "ok" | "err"; text: string } | null;
    } = $props();

    // 全平台统一走取景器自动连扫（无需手动拍照，命中即回调）；
    // getUserMedia 不可用（部分 WebView）时在 startCamera 内回退系统相机
    const openCamera = () => {
        void startCamera();
    };

    let manual = $state(false);
    let manualText = $state("");

    // 相机扫描状态
    let cameraOn = $state(false);
    let cameraError = $state("");
    let cameraStream: MediaStream | null = null;
    let cameraRaf = 0;
    let cameraDetected = false;
    let cameraStartedAt = 0;
    let videoElement: HTMLVideoElement | undefined = $state();

    const close = () => {
        stopCamera();
        show = false;
    };

    const submitManual = () => {
        const content = manualText.trim();
        if (!content) return;
        onManualSubmit(content);
        manualText = "";
        manual = false;
    };

    // 启动/停止实时相机扫码
    const startCamera = async () => {
        cameraError = "";
        cameraDetected = false;
        try {
            const stream = await navigator.mediaDevices.getUserMedia({
                video: { facingMode: "environment" },
                audio: false,
            });
            cameraStream = stream;
            // 挂载 `{#if cameraOn}` 里的 <video> 后由 $effect 挂流并播放
            cameraOn = true;
            cameraStartedAt = Date.now();
            scheduleTick();
        } catch (e) {
            cameraOn = false;
            cameraError = String((e as Error)?.message ?? e);
            // 取景器不可用（WebView 未开放 getUserMedia/无摄像头权限）：
            // 回退系统相机拍照（移动端原生相机 / 桌面文件选择）
            openCameraCapture();
        }
    };

    // 视频元素挂载/流就绪时挂流并播放（bind:this 在 cameraOn 渲染后才赋值，
    // 不能在同一同步块内读取）
    $effect(() => {
        if (cameraOn && cameraStream && videoElement) {
            videoElement.srcObject = cameraStream;
            void videoElement.play().catch(() => {});
        }
    });

    // 模态关闭（含父级直接置 show=false）时释放相机与解码循环，防摄像头资源泄漏
    $effect(() => {
        if (!show) stopCamera();
    });

    const stopCamera = () => {
        cameraOn = false;
        cancelAnimationFrame(cameraRaf);
        if (cameraStream) {
            cameraStream.getTracks().forEach((t) => t.stop());
            cameraStream = null;
        }
        if (videoElement) {
            videoElement.srcObject = null;
        }
    };

    // 复用的离屏采样 canvas（固定 480x360），避免每帧新建对象
    let frameCanvas: HTMLCanvasElement | null = null;
    let frameCtx: CanvasRenderingContext2D | null = null;
    const getFrameSurface = () => {
        if (!frameCanvas) {
            frameCanvas = document.createElement("canvas");
            frameCanvas.width = 480;
            frameCanvas.height = 360;
            frameCtx = frameCanvas.getContext("2d");
        }
        return frameCtx;
    };

    // 逐帧抽样：绘制到离屏 canvas 后解码；命中即回调并停止
    const decodeFrame = async () => {
        if (!cameraOn || !videoElement || cameraDetected) return;
        if (videoElement.readyState < 2) {
            // 就绪看门狗：摄像头流始终未就绪（设备被占用/轨道立即结束）时，
            // 不能无限空转造成"取景器卡住"，超时后报错并释放，交由用户重试/换系统相机
            if (Date.now() - cameraStartedAt > 10000) {
                cameraError = $_("scan_camera_error");
                stopCamera();
                return;
            }
            scheduleTick();
            return;
        }
        try {
            const ctx = getFrameSurface();
            if (!ctx) {
                scheduleTick();
                return;
            }
            ctx.drawImage(videoElement, 0, 0, 480, 360);
            const imageData = ctx.getImageData(0, 0, 480, 360);
            const text = await decodeQrCode(imageData);
            if (text) {
                cameraDetected = true;
                stopCamera();
                onCameraResult(text);
                return;
            }
        } catch {
            // 单帧解码失败（模糊/未对齐）继续下一帧
        }
        scheduleTick();
    };

    // 节流：每帧约 150ms 采样一次，避免持续全速编码
    const scheduleTick = () => {
        cancelAnimationFrame(cameraRaf);
        cameraRaf = requestAnimationFrame(() =>
            setTimeout(() => {
                if (cameraOn) void decodeFrame();
            }, 150),
        );
    };

    // 组件卸载时释放相机
    onDestroy(() => stopCamera());
</script>

{#if show}
    <div
        class="modal-overlay"
        onclick={(e) => e.target === e.currentTarget && close()}
        onkeydown={(e) => e.key === "Escape" && close()}
        role="dialog"
        aria-label={$_("scan_dialog")}
        tabindex="0"
    >
        <div
            class="modal-content scan-modal-content"
            role="document"
            aria-label={$_("scan_form")}
        >
            <h3>{$_("scan_qr_code")}</h3>

            {#if scanResult}
                <div class="scan-result" class:err={scanResult.kind === "err"} role="status">
                    {scanResult.text}
                </div>
            {/if}

            {#if cameraOn}
                <!-- 相机模式：镜头画面居中为主视图，便于对准 -->
                <div class="camera-wrap">
                    <div class="camera-viewport">
                        <video
                            bind:this={videoElement}
                            autoplay
                            muted
                            playsinline
                            class="camera-preview"
                        ></video>
                        <div class="scan-frame"></div>
                    </div>
                    <p class="camera-tip">{$_("scan_camera_hint")}</p>
                    <button
                        type="button"
                        class="scan-stop-btn"
                        onclick={stopCamera}
                        disabled={isScanning}
                    >
                        {$_("scan_camera_stop")}
                    </button>
                </div>
            {:else}
                {#if cameraError}
                    <p class="camera-error" role="alert">
                        {$_("scan_camera_error")} {cameraError}
                    </p>
                {/if}
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
                    <button
                        type="button"
                        class="action-btn scan-action-btn"
                        onclick={openCamera}
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
                            <path d="M23 19a2 2 0 0 1-2 2H3a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h4l2-3h6l2 3h4a2 2 0 0 1 2 2z" />
                            <circle cx="12" cy="13" r="4" />
                        </svg>
                        {$_("scan_camera")}
                    </button>
                </div>
            {/if}

            <!-- 扫码不便时的手动输入兜底（相机模式下隐藏） -->
            {#if !cameraOn}
                <button
                    type="button"
                    class="scan-toggle-btn"
                    disabled={isScanning}
                    onclick={() => (manual = !manual)}
                >
                    {manual ? $_("scan_manual_close") : $_("scan_manual_toggle")}
                </button>
                {#if manual}
<textarea
                    bind:value={manualText}
                    class="scan-manual-input"
                    rows="3"
                    placeholder={$_("scan_manual_placeholder")}
                    autocomplete="off"
                    autocapitalize="off"
                    spellcheck="false"
                ></textarea>
                    <button
                        type="button"
                        class="scan-submit-btn"
                        disabled={isScanning || !manualText.trim()}
                        onclick={submitManual}
                    >
                        {$_("scan_manual_submit")}
                    </button>
                {/if}
            {/if}
            {#if isScanning}
                <div class="scanning-indicator">{$_("loading")}...</div>
            {/if}
            <div class="modal-actions">
                <button type="button" class="btn-cancel" onclick={close}>
                    {$_("action_cancel")}
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
        flex-direction: column;
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
        border-color: var(--color-primary);
        color: var(--color-primary);
        background: var(--bg-secondary);
    }

    .scan-action-btn:disabled {
        opacity: 0.5;
        cursor: not-allowed;
    }

    .camera-wrap {
        display: flex;
        flex-direction: column;
        align-items: center;
        margin-bottom: 16px;
    }
    .camera-viewport {
        position: relative;
        width: 100%;
        max-width: 340px;
        border-radius: 8px;
        overflow: hidden;
        background: #000;
    }
    .camera-preview {
        display: block;
        width: 100%;
        border-radius: 8px;
        border: 1px solid var(--border-color, #2a2a2a);
        background: #000;
        object-fit: cover;
        aspect-ratio: 4 / 3;
    }
    .scan-frame {
        position: absolute;
        top: 50%;
        left: 50%;
        width: 60%;
        height: 60%;
        transform: translate(-50%, -50%);
        border: 2px solid rgba(59, 130, 246, 0.85);
        border-radius: 12px;
        pointer-events: none;
        box-shadow: 0 0 0 9999px rgba(0, 0, 0, 0.25);
        box-sizing: border-box;
    }
    .camera-tip {
        margin-top: 8px;
        font-size: 12px;
        color: var(--text-secondary, #737373);
    }
    .camera-error {
        margin-bottom: 12px;
        font-size: 13px;
        color: var(--color-danger, #ef4444);
    }
    .scan-result {
        margin-bottom: 12px;
        padding: 10px 12px;
        border-radius: 8px;
        font-size: 13px;
        line-height: 1.5;
        word-break: break-all;
        text-align: left;
        background: rgba(34, 197, 94, 0.12);
        border: 1px solid rgba(34, 197, 94, 0.4);
        color: var(--text-primary, #fafafa);
    }
    .scan-result.err {
        background: rgba(239, 68, 68, 0.12);
        border-color: rgba(239, 68, 68, 0.45);
        color: #fca5a5;
    }

    .scanning-indicator {
        padding: 12px;
        color: var(--text-secondary);
        font-size: 14px;
    }

    .scan-toggle-btn {
        margin-top: 4px;
        background: transparent;
        border: none;
        color: var(--color-primary, #3b82f6);
        cursor: pointer;
        font-size: 13px;
        padding: 6px 8px;
        transition: opacity 0.2s;
    }
    .scan-toggle-btn:hover:not(:disabled) {
        opacity: 0.8;
    }
    .scan-toggle-btn:disabled {
        opacity: 0.4;
        cursor: not-allowed;
    }
    .scan-manual-input {
        width: 100%;
        box-sizing: border-box;
        margin-top: 6px;
        padding: 10px;
        background: var(--bg-primary, #141414);
        border: 1px solid var(--border-color, #2a2a2a);
        border-radius: 6px;
        color: var(--text-primary, #fafafa);
        font-family: inherit;
        font-size: 13px;
        resize: vertical;
    }
    .scan-manual-input:focus {
        outline: none;
        border-color: var(--color-primary, #3b82f6);
    }
    .scan-submit-btn {
        margin-top: 8px;
        padding: 8px 16px;
        border-radius: 6px;
        cursor: pointer;
        font-size: 14px;
        background: var(--color-primary, #3b82f6);
        border: none;
        color: #fff;
        transition: opacity 0.2s;
    }
    .scan-submit-btn:hover:not(:disabled) {
        opacity: 0.85;
    }
    .scan-submit-btn:disabled {
        opacity: 0.4;
        cursor: not-allowed;
    }
    .scan-stop-btn {
        margin-top: 12px;
        padding: 8px 20px;
        border-radius: 6px;
        cursor: pointer;
        font-size: 14px;
        background: transparent;
        border: 1px solid var(--color-danger, #ef4444);
        color: var(--color-danger, #ef4444);
        transition: all 0.2s;
    }
    .scan-stop-btn:hover:not(:disabled) {
        background: rgba(239, 68, 68, 0.12);
    }
    .scan-stop-btn:disabled {
        opacity: 0.4;
        cursor: not-allowed;
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

    @media (width <= 480px) {
        .scan-modal-content {
            width: 100%;
            max-width: 100%;
            height: 100dvh;
            border-radius: 0;
            display: flex;
            flex-direction: column;
            justify-content: center;
            padding: 16px;
            padding-top: calc(16px + var(--safe-area-top));
            padding-bottom: calc(16px + var(--safe-area-bottom));
        }
        .scan-action-btn {
            font-size: 16px;
            padding: 14px 20px;
        }
        .btn-cancel {
            flex: 1;
            text-align: center;
            padding: 12px 16px;
            font-size: 16px;
        }
    }
</style>
