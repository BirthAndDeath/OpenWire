import { Store } from '@tauri-apps/plugin-store';

// 全局唯一的 Store 实例
let globalStore: Store | null = null;

/**
 * 初始化全局设置 Store（应用启动时调用一次）
 */
export async function initSettingsStore(): Promise<Store> {
  if (!globalStore) {
    globalStore = await Store.load('.settings.dat');
  }
  return globalStore;
}

/**
 * 获取全局 Store 实例
 */
export function getSettingsStore(): Store | null {
  return globalStore;
}

/**
 * 通用的设置读取方法
 */
export async function getSetting<T>(key: string): Promise<T | undefined> {
  if (!globalStore) {
    console.warn('Settings store not initialized');
    return undefined;
  }
  
  try {
    return await globalStore.get<T>(key);
  } catch (error) {
    console.error(`Failed to read setting '${key}':`, error);
    return undefined;
  }
}

/**
 * 通用的设置写入方法
 */
export async function setSetting<T>(key: string, value: T): Promise<void> {
  if (!globalStore) {
    console.warn('Settings store not initialized');
    return;
  }
  
  try {
    await globalStore.set(key, value);
    await globalStore.save();
  } catch (error) {
    console.error(`Failed to write setting '${key}':`, error);
  }
}
