import { writable } from "svelte/store";

export type TransferStatus = "downloading" | "completed" | "failed" | "error";

export interface FileTransferProgress {
  file_id: string;
  filename: string;
  chunk_index: number;
  total_chunks: number;
  received_bytes: number;
  total_size: number;
  /** 发送方 ML-DSA 公钥 hex（全局面板重试下载用） */
  sender_pubkey_hex?: string;
  status: TransferStatus;
}

/**
 * 全局文件传输任务 store（file_id → 任务）。
 *
 * 单一权威来源：聊天消息卡片与全局"传输任务"面板都订阅它；
 * 事件驱动增量合并（received_bytes 单调不回退），失败/完成均持久保留，
 * 用户可在面板中取消/重试/删除，无需翻历史消息。
 */
export const transferTasks = writable<Record<string, FileTransferProgress>>({});

/** 合并一条进度事件（单调：已收字节不回退，避免乱序事件让进度条后退） */
export function upsertTransfer(progress: FileTransferProgress) {
  transferTasks.update((prev) => {
    const key = progress.file_id || progress.filename;
    const old = prev[key];
    // 新传输接管旧条目（旧为终态 error/failed/completed，新为 downloading）：
    // 直接采用新进度，不继承旧的高字节——否则失败后重试会卡在 100%
    // （失败终态事件把 received_bytes 置为 total_size）。
    const isNewTransfer =
      old !== undefined &&
      old.status !== "downloading" &&
      progress.status === "downloading";
    const merged =
      !isNewTransfer &&
      old !== undefined &&
      old.received_bytes > progress.received_bytes
        ? { ...progress, received_bytes: old.received_bytes }
        : progress;
    return { ...prev, [key]: merged };
  });
}

/** 删除任务（面板"删除"：清理本地条目；后端文件清理由 cancel_file_transfer 完成） */
export function removeTransfer(key: string) {
  transferTasks.update((prev) => {
    const next = { ...prev };
    delete next[key];
    return next;
  });
}

/** 进度百分比（0-100，total_size 未知时为 0） */
export function calcProgress(p: FileTransferProgress): number {
  if (!p.total_size) return 0;
  return Math.min(100, Math.round((p.received_bytes / p.total_size) * 100));
}
