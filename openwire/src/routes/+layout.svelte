<script lang="ts">
  import "../lib/i18n";
  import "../lib/styles/variables.css";
  import { onMount, type Snippet } from "svelte";
  import { openUrl } from "@tauri-apps/plugin-opener";
  import { initTheme } from "../lib/theme";
  import { initLanguage } from "../lib/language";
  import { initSettingsStore, getSetting, chatBackgroundStore, fontSizeScale } from "../lib/settings";
  import { resolveBackgroundUrl } from "../lib/background";
  import Background from "./Background.svelte";

  let { children }: { children?: Snippet } = $props();

  let bgUrl = $state("");
  let bgObjectUrl: string | null = null;
  let bgStore = $state("");
  let settingsReady = $state(false);

// 在布局层初始化主题和语言
  $effect(() => {
    async function init() {
      await Promise.all([initTheme(), initLanguage()]);
    }
    init().catch(console.error);
  });

  // 桥接：store 订阅 → $state 更新（确保 $effect 能追踪）
  $effect(() => {
    const unsub = chatBackgroundStore.subscribe((v) => {
      bgStore = v;
    });
    return unsub;
  });

  // 响应 $state 变化，异步加载背景图（settingsReady 前跳过，避免 initSettingsStore 未完成时误读）
  $effect(() => {
    bgStore;
    settingsReady;
    if (!settingsReady) return;
    let cancelled = false;
    (async () => {
      let url: string;
      try {
        url = await resolveBackgroundUrl();
      } catch (e) {
        console.error("加载背景图失败:", e);
        return;
      }
      if (cancelled) {
        if (url.startsWith("blob:")) URL.revokeObjectURL(url);
        return;
      }
      if (bgObjectUrl?.startsWith("blob:")) URL.revokeObjectURL(bgObjectUrl);
      bgObjectUrl = url.startsWith("blob:") ? url : null;
      bgUrl = url;
    })();
    return () => {
      cancelled = true;
      if (bgObjectUrl?.startsWith("blob:")) {
        URL.revokeObjectURL(bgObjectUrl);
        bgObjectUrl = null;
        bgUrl = "";
      }
    };
  });

  onMount(async () => {
    try {
      await initSettingsStore();
      // 字号
      const savedFontSize = await getSetting<number>("font_size");
      if (savedFontSize !== undefined && savedFontSize >= 0.5 && savedFontSize <= 2.0) {
        fontSizeScale.set(savedFontSize);
      }
    } catch (e) {
      console.error("初始化布局设置失败:", e);
    } finally {
      // 设置就绪后，背景加载交由上面的 $effect 统一执行（单次）；
      // 即使读取失败也置就绪，避免背景因等待一个永不成功的初始化而永久不加载
      settingsReady = true;
    }
  });

  // 全局拦截外部链接：http(s) 锚点统一交由系统浏览器打开，
  // 避免 WebView 在应用内导航到空白外部页（CSP 不管 OS 级操作，走 IPC 不受 connect-src 限制）。
  onMount(() => {
    const onClick = (e: MouseEvent) => {
      if (e.defaultPrevented || e.button !== 0 || e.metaKey || e.ctrlKey) return;
      const anchor = (e.target as HTMLElement)?.closest?.("a");
      if (!anchor) return;
      const href = anchor.getAttribute("href");
      if (!href || !/^https?:\/\//i.test(href)) return;
      e.preventDefault();
      openUrl(href).catch((err) => console.error("打开外部链接失败:", err));
    };
    document.addEventListener("click", onClick);
    return () => document.removeEventListener("click", onClick);
  });
</script>

{#if !bgUrl}<Background />{/if}
<div class="app-bg" style:--bg-url={bgUrl ? `url("${bgUrl}")` : undefined}></div>
<div class="app-layout">
  {@render children?.()}
</div>

<style>
  :global(html), :global(body) {
    height: 100%;
    overflow: hidden;
    overscroll-behavior: none;
  }

  .app-bg {
    position: fixed;
    inset: 0;
    z-index: 0;
    background-image: linear-gradient(rgba(0,0,0,0.5), rgba(0,0,0,0.5)), var(--bg-url, none);
    background-position: center;
    background-size: cover;
    background-repeat: no-repeat;
    background-attachment: fixed;
  }

  .app-layout {
    position: relative;
    z-index: 1;
    box-sizing: border-box;
    width: 100%;
    height: 100dvh;
    overflow: hidden;
    color: var(--text-primary);
    padding-top: var(--safe-area-top);
    padding-bottom: var(--safe-area-bottom);
    padding-left: var(--safe-area-left);
    padding-right: var(--safe-area-right);
  }
</style>