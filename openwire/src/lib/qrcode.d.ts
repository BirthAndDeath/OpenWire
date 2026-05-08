/**
 * 在 canvas 上绘制二维码。
 * @param canvas 目标 canvas 元素
 * @param data   要编码的数据（字符串或二进制字节数组）
 * @param size   二维码尺寸（默认 256）
 */
export function drawQrCode(
    canvas: HTMLCanvasElement,
    data: string | Uint8Array,
    size?: number,
): void;

/**
 * 从 ImageData 中解码二维码。
 * @param imageData 图像像素数据
 * @returns 解码出的字符串，失败返回 null
 */
export function decodeQrCode(imageData: ImageData): string | null;
