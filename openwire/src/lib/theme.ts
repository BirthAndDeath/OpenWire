import { get, writable } from 'svelte/store';
import { initSettingsStore, getSetting, setSetting } from './settings';

// 主题类型定义
export type Theme = 'dark' | 'light';

// 主题色预设
export const ACCENT_PRESETS: { name: string; color: string }[] = [
  { name: '默认蓝', color: '#3b82f6' },
  { name: '青绿', color: '#10b981' },
  { name: '紫罗兰', color: '#8b5cf6' },
  { name: '琥珀', color: '#f59e0b' },
  { name: '玫红', color: '#f472b6' },
];

// 创建可写的主题 store，默认值为 'dark'
const themeStore = writable<Theme>('dark');

// 主题色 store（hex）
const accentStore = writable<string>('#3b82f6');

// UI 字号缩放（0.9 ~ 1.2）
const uiScaleStore = writable<number>(1);

// 界面密度：'cozy' 舒适（默认）/ 'compact' 紧凑
const densityStore = writable<'cozy' | 'compact'>('cozy');

// 导出订阅方法，组件可以使用 $theme 语法
export const theme = themeStore;
export const accent = accentStore;
export const uiScale = uiScaleStore;
export const density = densityStore;

// 计算主色的 -strong（加深 15%）与 -soft（12% 透明度）
function deriveAccent(hex: string): { strong: string; soft: string } {
  const h = hex.replace('#', '');
  const r = parseInt(h.slice(0, 2), 16) || 0;
  const g = parseInt(h.slice(2, 4), 16) || 0;
  const b = parseInt(h.slice(4, 6), 16) || 0;
  const strong = `rgb(${Math.max(0, Math.round(r * 0.85))}, ${Math.max(0, Math.round(g * 0.85))}, ${Math.max(0, Math.round(b * 0.85))})`;
  const soft = `rgba(${r}, ${g}, ${b}, 0.18)`;
  return { strong, soft };
}

// 应用主题色/缩放/密度到 DOM
function applyUiPreferences() {
  if (typeof document === 'undefined') return;
  const el = document.documentElement;
  // 主题色（primary/strong/soft 派生）
  const color = get(accentStore);
  const d = deriveAccent(color);
  el.style.setProperty('--color-primary', color);
  el.style.setProperty('--color-primary-strong', d.strong);
  el.style.setProperty('--color-primary-soft', d.soft);
  // 界面字号：CSS zoom 等比缩放整个界面（组件字号多为硬编码 px，
  // 令牌乘算无法覆盖；zoom 零组件改动且 Chromium/WebKit 均支持）
  el.style.zoom = String(get(uiScaleStore));
  el.dataset.density = get(densityStore);
}

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
      applyTheme(savedTheme);
    } else {
      const systemTheme = detectSystemTheme();
      applyTheme(systemTheme);
      await persistTheme(systemTheme);
    }

    // 读取外观偏好（主题色/字号缩放/密度）
    const savedAccent = await getSetting<string>('accent');
    if (savedAccent && /^#[0-9a-fA-F]{6}$/.test(savedAccent)) accentStore.set(savedAccent);
    const savedScale = await getSetting<number>('ui_scale');
    if (savedScale && savedScale >= 0.85 && savedScale <= 1.25) uiScaleStore.set(savedScale);
    const savedDensity = await getSetting<'cozy' | 'compact'>('density');
    if (savedDensity === 'compact') densityStore.set('compact');
    applyUiPreferences();
  } catch (error) {
    console.error('Failed to initialize theme:', error);
    applyTheme(detectSystemTheme());
  }
}

// 应用主题（更新 DOM）
function applyTheme(themeValue: Theme) {
  themeStore.set(themeValue);
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
  applyTheme(newTheme);
  await persistTheme(newTheme);
}

// 设置主题色（公开 API）
export async function setAccent(color: string) {
  if (!/^#[0-9a-fA-F]{6}$/.test(color)) return;
  accentStore.set(color);
  applyUiPreferences();
  await setSetting('accent', color);
}

// 设置 UI 字号缩放（公开 API）
export async function setUiScale(scale: number) {
  const clamped = Math.min(1.25, Math.max(0.85, scale));
  uiScaleStore.set(clamped);
  applyUiPreferences();
  await setSetting('ui_scale', clamped);
}

// 设置界面密度（公开 API）
export async function setDensity(d: 'cozy' | 'compact') {
  densityStore.set(d);
  applyUiPreferences();
  await setSetting('density', d);
}

