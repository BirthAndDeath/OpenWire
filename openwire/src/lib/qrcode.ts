import encodeQR from "qr";
import jsQR from "jsqr";

/**
 * 将 Uint8Array 转为 Latin-1 字符串（每个字节映射为对应字符）。
 * 用于编码端：将二进制数据转为 Latin-1 字符串传入 encodeQR，
 * textEncoder 回调再将其转回原始字节，绕过默认的 UTF-8 编码。
 */
function bytesToLatin1(data: Uint8Array): string {
    let s = "";
    for (let i = 0; i < data.length; i++) {
        s += String.fromCharCode(data[i]);
    }
    return s;
}

/**
 * 将 Latin-1 字符串转回 Uint8Array。
 * 用于 textEncoder 回调：将 Latin-1 字符串编码为原始字节。
 */
function latin1ToBytes(s: string): Uint8Array {
    const bytes = new Uint8Array(s.length);
    for (let i = 0; i < s.length; i++) {
        bytes[i] = s.charCodeAt(i);
    }
    return bytes;
}

/**
 * 在 canvas 上绘制二维码。
 *
 * 编码策略：
 * - 将二进制数据（Uint8Array）转为 Latin-1 字符串后传入 encodeQR。
 * - 使用 textEncoder 回调将 Latin-1 字符串转回原始字节，
 *   绕过默认的 UTF-8 编码（UTF-8 会将 0x80-0xFF 的字节扩展为多字节序列）。
 *
 * @param canvas 目标 canvas 元素
 * @param data   要编码的数据（字符串或二进制字节数组）
 * @param size   二维码显示尺寸（默认 256）
 */
export function drawQrCode(
    canvas: HTMLCanvasElement,
    data: string | Uint8Array,
    size: number = 256,
): void {
    const rawBytes = typeof data === "string" ? new TextEncoder().encode(data) : data;
    const text = bytesToLatin1(rawBytes);

    const matrix = encodeQR(text, "raw", {
        encoding: "byte",
        ecc: "medium",
        border: 4,
        textEncoder: (input: string) => {
            return latin1ToBytes(input);
        },
    });

    const totalModules = matrix.length;
    const cellSize = Math.floor(size / totalModules);
    const actualSize = cellSize * totalModules;

    canvas.width = actualSize;
    canvas.height = actualSize;

    const ctx = canvas.getContext("2d");
    if (!ctx) return;

    ctx.fillStyle = "#ffffff";
    ctx.fillRect(0, 0, actualSize, actualSize);

    ctx.fillStyle = "#000000";
    for (let row = 0; row < totalModules; row++) {
        for (let col = 0; col < totalModules; col++) {
            if (matrix[row][col]) {
                ctx.fillRect(
                    col * cellSize,
                    row * cellSize,
                    cellSize,
                    cellSize,
                );
            }
        }
    }
}

/**
 * 从 ImageData 中解码二维码（异步）。
 *
 * 优先交给 Web Worker 解码：jsQR 是同步 CPU 密集操作，手机原图可达千万像素，
 * 在主线程直接解码会冻结界面。Worker 解码失败/不可用时回退主线程同步解码，
 * 但对超大输入先降采样，避免回退路径再次冻结。
 *
 * @param imageData 图像像素数据
 * @returns 解码出的字符串（Latin-1 编码，每个字符的 charCodeAt 对应原始字节值），失败返回 null
 */

// 同步回退路径的单帧像素上限（≈2048²）：超过则先降采样，防止主线程冻结
const MAX_SYNC_PIXELS = 2048 * 2048;
const QR_WORKER_TIMEOUT_MS = 8000;

function decodeSync(imageData: ImageData): string | null {
    let data = imageData;
    if (imageData.width * imageData.height > MAX_SYNC_PIXELS) {
        const scale = Math.sqrt(MAX_SYNC_PIXELS / (imageData.width * imageData.height));
        const w = Math.max(1, Math.round(imageData.width * scale));
        const h = Math.max(1, Math.round(imageData.height * scale));
        const src = document.createElement("canvas");
        src.width = imageData.width;
        src.height = imageData.height;
        const sctx = src.getContext("2d");
        const dst = document.createElement("canvas");
        dst.width = w;
        dst.height = h;
        const dctx = dst.getContext("2d");
        if (sctx && dctx) {
            sctx.putImageData(imageData, 0, 0);
            dctx.drawImage(src, 0, 0, w, h);
            data = dctx.getImageData(0, 0, w, h);
        }
    }
    try {
        const result = jsQR(data.data, data.width, data.height);
        if (!result) return null;
        return bytesToLatin1(new Uint8Array(result.binaryData));
    } catch (e) {
        console.error("[decodeQrCode] jsQR 抛出异常:", e);
        return null;
    }
}

let qrWorker: Worker | null = null;
let qrWorkerBroken = false;
let qrSeq = 0;
const qrPending = new Map<number, (value: string | null) => void>();

function getQrWorker(): Worker | null {
    if (qrWorker) return qrWorker;
    if (qrWorkerBroken) return null;
    try {
        const worker = new Worker(new URL("./qr-worker.ts", import.meta.url), {
            type: "module",
        });
        worker.onmessage = (e: MessageEvent<{ id: number; data: Uint8Array | null }>) => {
            const { id, data } = e.data ?? {};
            const resolve = qrPending.get(id);
            if (!resolve) return;
            qrPending.delete(id);
            resolve(data ? bytesToLatin1(data) : null);
        };
        worker.onerror = () => {
            // worker 脚本加载失败（模块 worker 不可用）：标记失效，回退主线程同步解码
            qrWorkerBroken = true;
            qrWorker = null;
            for (const resolve of qrPending.values()) resolve(null);
            qrPending.clear();
        };
        qrWorker = worker;
        return worker;
    } catch {
        qrWorkerBroken = true;
        return null;
    }
}

export function decodeQrCode(imageData: ImageData): Promise<string | null> {
    const worker = getQrWorker();
    if (!worker) return Promise.resolve(decodeSync(imageData));
    return new Promise((resolve) => {
        const id = ++qrSeq;
        const timer = setTimeout(() => {
            qrPending.delete(id);
            resolve(null);
        }, QR_WORKER_TIMEOUT_MS);
        qrPending.set(id, (value) => {
            clearTimeout(timer);
            resolve(value);
        });
        try {
            // 转移像素缓冲（零拷贝）；调用方每帧都新建 ImageData，转移安全
            worker.postMessage(
                { id, data: imageData.data, width: imageData.width, height: imageData.height },
                [imageData.data.buffer],
            );
        } catch {
            clearTimeout(timer);
            qrPending.delete(id);
            resolve(decodeSync(imageData));
        }
    });
}
