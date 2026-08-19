<script lang="ts">
  import "../lib/i18n";
  import "../lib/styles/variables.css";
  import { onMount, type Snippet } from "svelte";
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
      const url = await resolveBackgroundUrl();
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
    await initSettingsStore();
    // 设置就绪后，背景加载交由上面的 $effect 统一执行（单次），不再重复显式加载
    settingsReady = true;
    // 字号
    const savedFontSize = await getSetting<number>("font_size");
    if (savedFontSize !== undefined && savedFontSize >= 0.5 && savedFontSize <= 2.0) {
      fontSizeScale.set(savedFontSize);
    }
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