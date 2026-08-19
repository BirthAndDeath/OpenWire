import { writable } from 'svelte/store';
import { initSettingsStore, getSetting, setSetting } from './settings';
import { locale } from 'svelte-i18n';

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
