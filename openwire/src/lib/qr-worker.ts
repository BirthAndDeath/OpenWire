import jsQR from "jsqr";

// 二维码解码 worker：jsQR 解码大图（手机原图可达千万像素）在主线程会冻结界面，
// 放入 worker 后不阻塞 UI，从而可以放心用原图分辨率解码（不再受降采样上限限制）。
interface DecodeRequest {
    id: number;
    data: Uint8ClampedArray;
    width: number;
    height: number;
}

addEventListener("message", (e: MessageEvent<DecodeRequest>) => {
    const { id, data, width, height } = e.data;
    let result: Uint8Array | null = null;
    try {
        const decoded = jsQR(data, width, height);
        if (decoded) result = new Uint8Array(decoded.binaryData);
    } catch {
        result = null;
    }
    (self as unknown as Worker).postMessage({ id, data: result });
});
