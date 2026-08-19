// 聊天背景图统一存储（全平台）：WebView IndexedDB 保存 Blob，单键覆盖。
// 避免桌面/移动端文件路径与 content:// URI 的差异。

import { chatBackgroundStore, getSetting, setSetting } from "./settings";

/** 设置中存储的标记值：背景图在 IndexedDB 中 */
export const BG_MARKER = "idb://background";

const IDB_NAME = "openwire";
const IDB_STORE = "backgrounds";
const IDB_KEY = "current";

/** 单例数据库连接，避免每次操作新建连接导致资源累计 */
let _db: Promise<IDBDatabase> | null = null;

function getDb(): Promise<IDBDatabase> {
  if (!_db) {
    _db = new Promise((resolve, reject) => {
      const req = indexedDB.open(IDB_NAME, 1);
      req.onupgradeneeded = () => {
        if (!req.result.objectStoreNames.contains(IDB_STORE)) {
          req.result.createObjectStore(IDB_STORE);
        }
      };
      req.onsuccess = () => resolve(req.result);
      req.onerror = () => {
        _db = null; // 失败后允许下次重试
        reject(req.error);
      };
    });
  }
  return _db;
}

async function withStore<T>(
  mode: "readonly" | "readwrite",
  fn: (store: IDBObjectStore) => IDBRequest,
): Promise<T> {
  const db = await getDb();
  return new Promise<T>((resolve, reject) => {
    const tx = db.transaction(IDB_STORE, mode);
    tx.onerror = () => reject(tx.error);
    tx.onabort = () => reject(tx.error);
    const req = fn(tx.objectStore(IDB_STORE));
    req.onsuccess = () => resolve(req.result as T);
    req.onerror = () => reject(req.error);
  });
}

/** 保存背景图：单键 put 覆写旧图，并写入设置标记 */
export async function saveBackgroundBlob(blob: Blob): Promise<void> {
  await withStore("readwrite", (s) => s.put(blob, IDB_KEY));
  await setSetting("chat_background", BG_MARKER);
  // 自增后缀保证每次都是新值，触发 store 订阅（Svelte 同值不触发）
  _bgVersion++;
  chatBackgroundStore.set(`${BG_MARKER}:${_bgVersion}`);
}

let _bgVersion = 0;

/** 读取当前背景图 Blob */
export async function loadBackgroundBlob(): Promise<Blob | null> {
  const db = await getDb();
  return new Promise((resolve, reject) => {
    const tx = db.transaction(IDB_STORE, "readonly");
    const req = tx.objectStore(IDB_STORE).get(IDB_KEY);
    req.onsuccess = () => resolve((req.result as Blob | undefined) ?? null);
    req.onerror = () => reject(req.error);
  });
}

/** 清除背景图（删除 Blob + 清空设置）；旧 objectURL 由调用方 revoke */
export async function clearBackground(): Promise<void> {
  await withStore("readwrite", (s) => s.delete(IDB_KEY));
  await setSetting("chat_background", "");
  chatBackgroundStore.set("");
}

/** 当前背景图的渲染 URL：标记 → 从 IDB 取 Blob 生成 objectURL；否则空 */
export async function resolveBackgroundUrl(): Promise<string> {
  const saved = await getSetting<string>("chat_background");
  if (!saved || saved !== BG_MARKER) return "";
  const blob = await loadBackgroundBlob();
  return blob ? URL.createObjectURL(blob) : "";
}