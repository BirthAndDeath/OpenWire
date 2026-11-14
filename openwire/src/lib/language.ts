import { writable } from 'svelte/store';
import { initSettingsStore, getSetting, setSetting } from './settings';
import { locale } from 'svelte-i18n';
import { invoke } from '@tauri-apps/api/core';
import en from './locales/en.json';
import zh from './locales/zh.json';
import fr from './locales/fr.json';
import es from './locales/es.json';
import de from './locales/de.json';
import ja from './locales/ja.json';

// 托盘菜单文案按语言映射（单一数据源：与 i18n 的 JSON 同源）
const TRAY_LABELS: Record<string, { show_window: string; quit: string }> = {
  en: { show_window: en.tray_show_window, quit: en.tray_quit },
  zh: { show_window: zh.tray_show_window, quit: zh.tray_quit },
  fr: { show_window: fr.tray_show_window, quit: fr.tray_quit },
  es: { show_window: es.tray_show_window, quit: es.tray_quit },
  de: { show_window: de.tray_show_window, quit: de.tray_quit },
  ja: { show_window: ja.tray_show_window, quit: ja.tray_quit },
};

// 同步托盘菜单文本到原生侧（切换/初始化语言时调用，fire-and-forget）
function syncTrayMenu(lang: string) {
  const labels = TRAY_LABELS[lang] ?? TRAY_LABELS.en;
  invoke('set_tray_menu_texts', {
    showWindowText: labels.show_window,
    quitText: labels.quit,
  }).catch((e) => console.error('更新托盘菜单失败:', e));
}

// 支持的语言列表（代码 + 显示名称）
export const SUPPORTED_LANGUAGES_WITH_NAMES = [
  { code: 'en', name: 'English' },
  { code: 'zh', name: '中文' },
  { code: 'fr', name: 'Français' },
  { code: 'es', name: 'Español' },
  { code: 'de', name: 'Deutsch' },
  { code: 'ja', name: '日本語' },
];

// 支持的语言代码列表（由 WITH_NAMES 派生，单一数据源）
export const SUPPORTED_LANGUAGES = SUPPORTED_LANGUAGES_WITH_NAMES.map((l) => l.code);

// 创建可写的语言 store，默认值为 'en'
const languageStore = writable<string>('en');

// 导出订阅方法，组件可以使用 $language 语法
export const language = languageStore;

// 初始化语言管理器
export async function initLanguage() {
  try {
    // 初始化全局 Store
    await initSettingsStore();
    
    // 从 Store 读取保存的语言
    const savedLang = await getSetting<string>('language');
    
    if (savedLang && SUPPORTED_LANGUAGES.includes(savedLang)) {
      // 如果存在有效的保存语言，使用它
      applyLanguage(savedLang);
    } else {
      // 否则尝试从系统获取默认语言
      const systemLang = getSystemLanguage();
      
      if (SUPPORTED_LANGUAGES.includes(systemLang)) {
        // 系统语言受支持，使用它
        applyLanguage(systemLang);
      } else {
        // 系统语言不支持，回退到英文
        applyLanguage('en');
      }
      
      // 持久化最终确定的语言设置
      await persistLanguage(getLanguage());
    }
  } catch (error) {
    console.error('Failed to initialize language:', error);
    // 失败时使用默认语言
    applyLanguage('en');
    await persistLanguage('en');
  }
}

// 获取系统默认语言
function getSystemLanguage(): string {
  if (typeof navigator !== 'undefined' && navigator.language) {
    // 获取浏览器/WebView 语言，如 "zh-CN" -> "zh"
    const lang = navigator.language.split('-')[0].toLowerCase();
    return lang;
  }
  return 'en';
}

// 应用语言（更新 svelte-i18n）
function applyLanguage(lang: string) {
  // 更新 Svelte store
  languageStore.set(lang);

  // 更新 svelte-i18n
  locale.set(lang);

  // 同步原生托盘菜单文案
  syncTrayMenu(lang);
}

// 持久化语言设置
async function persistLanguage(lang: string) {
  await setSetting('language', lang);
}

// 切换语言（公开 API）
export async function setLanguage(newLang: string) {
  if (!SUPPORTED_LANGUAGES.includes(newLang)) {
    console.warn(`Language '${newLang}' is not supported`);
    return;
  }
  
  // 立即更新 UI（乐观更新）
  applyLanguage(newLang);
  
  // 异步保存到持久化存储
  await persistLanguage(newLang);
}

// 获取当前语言（同步访问）
export function getLanguage(): string {
  let currentLang: string = 'en';
  languageStore.subscribe(value => {
    currentLang = value;
  })();
  return currentLang;
}
