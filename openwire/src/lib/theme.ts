import { writable } from 'svelte/store';
import { initSettingsStore, getSetting, setSetting } from './settings';

// 主题类型定义
export type Theme = 'dark' | 'light';

// 创建可写的主题 store，默认值为 'dark'
const themeStore = writable<Theme>('dark');

// 导出订阅方法，组件可以使用 $theme 语法
export const theme = themeStore;

// 检测系统主题偏好
function detectSystemTheme(): Theme {
  if (typeof window !== 'undefined' && window.matchMedia) {
    if (window.matchMedia('(prefers-color-scheme: dark)').matches) {
      return 'dark';
    }
    if (window.matchMedia('(prefers-color-scheme: light)').matches) {
      return 'light';
    }
  }
  // 如果无法检测系统主题，默认返回 'dark'
  return 'dark';
}

// 初始化主题管理器
export async function initTheme() {
  try {
    // 初始化全局 Store
    await initSettingsStore();
    
    // 从 Store 读取保存的主题
    const savedTheme = await getSetting<Theme>('theme');
    
    if (savedTheme && (savedTheme === 'dark' || savedTheme === 'light')) {
      // 如果存在有效的保存主题，使用它
      applyTheme(savedTheme);
    } else {
      // 否则尝试使用系统主题，并持久化选择的主题
      const systemTheme = detectSystemTheme();
      applyTheme(systemTheme);
      await persistTheme(systemTheme);
    }
  } catch (error) {
    console.error('Failed to initialize theme:', error);
    // 失败时使用默认主题并持久化
    const fallbackTheme = detectSystemTheme();
    applyTheme(fallbackTheme);
    await persistTheme(fallbackTheme);
  }
}

// 应用主题（更新 DOM）
function applyTheme(themeValue: Theme) {
  // 更新 Svelte store
  themeStore.set(themeValue);
  
  // 更新 DOM
  if (typeof document !== 'undefined') {
    document.documentElement.setAttribute('data-theme', themeValue);
  }
}

// 持久化主题设置
async function persistTheme(themeValue: Theme) {
  await setSetting('theme', themeValue);
}

// 切换主题（公开 API）
export async function setTheme(newTheme: Theme) {
  if (newTheme !== 'dark' && newTheme !== 'light') {
    console.warn(`Theme '${newTheme}' is not valid`);
    return;
  }
  
  // 立即更新 UI（乐观更新）
  applyTheme(newTheme);
  
  // 异步保存到持久化存储
  await persistTheme(newTheme);
}

