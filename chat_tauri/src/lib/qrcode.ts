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
        version: 39,
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
 * 从 ImageData 中解码二维码。
 *
 * 使用 jsQR 库解码，通过 binaryData 获取原始字节。
 * jsQR 的 binaryData 是 Uint8Array，包含 QR 码中编码的原始字节数据，
 * 不受 UTF-8 解码影响，适合解码二进制数据。
 *
 * @param imageData 图像像素数据
 * @returns 解码出的字符串（Latin-1 编码，每个字符的 charCodeAt 对应原始字节值），失败返回 null
 */
export function decodeQrCode(imageData: ImageData): string | null {
    try {
        const result = jsQR(imageData.data, imageData.width, imageData.height);
        if (!result) {
            return null;
        }
        // binaryData 是原始字节数组（number[]），转为 Uint8Array 后再转 Latin-1 字符串
        // 上层通过 charCodeAt 提取原始字节值
        return bytesToLatin1(new Uint8Array(result.binaryData));
    } catch (e) {
        console.error("[decodeQrCode] jsQR 抛出异常:", e);
        return null;
    }
}
