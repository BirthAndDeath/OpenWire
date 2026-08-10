<script lang="ts">
  import "../lib/i18n";
  import "../lib/styles/variables.css";
  import { onMount } from "svelte";
  import { initTheme } from "../lib/theme";
  import { initLanguage } from "../lib/language";
  import { initSettingsStore, getSetting, setSetting, chatBackgroundStore, chatBackgroundVersion, fontSizeScale } from "../lib/settings";
  import { get } from "svelte/store";
  import { convertFileSrc, invoke } from "@tauri-apps/api/core";
  import { join, appDataDir } from "@tauri-apps/api/path";
  import Background from "./Background.svelte";

  let { children }: { children?: () => any } = $props();

  let bgUrl = $state("");

  async function migrateBackground(path: string, appData: string): Promise<string | null> {
    const srcName = path.split(/[/\\]/).pop()!;
    const ext = srcName.lastIndexOf(".") >= 0 ? srcName.slice(srcName.lastIndexOf(".")) : "";
    const dest = await join(appData, "backgrounds", `background-${Date.now()}${ext}`);
    try {
      await invoke("copy_file", { src: path, dst: dest });
      await setSetting("chat_background", dest);
      return dest;
    } catch (e) {
      console.error("背景图迁移失败:", e);
      await setSetting("chat_background", "");
      return null;
    }
  }

// 在布局层初始化主题和语言
  $effect(() => {
    async function init() {
      await Promise.all([initTheme(), initLanguage()]);
    }
    init().catch(console.error);
  });

  // 订阅背景图路径和版本号变化，bgUrl 加上版本号参数以破坏缓存
  $effect(() => {
    function updateBg() {
      const path = get(chatBackgroundStore);
      const ver = get(chatBackgroundVersion);
      bgUrl = path ? `${convertFileSrc(path)}?v=${ver}` : "";
    }
    const unsubPath = chatBackgroundStore.subscribe(updateBg);
    const unsubVer = chatBackgroundVersion.subscribe(updateBg);
    return () => {
      unsubPath();
      unsubVer();
    };
  });

  // 首次加载背景设置（$effect 异步时序不可靠，改用 onMount 确保订阅已就绪）
  onMount(async () => {
    await initSettingsStore();
    const saved = await getSetting<string>("chat_background");
    if (saved) {
      const appData = await appDataDir();
      const bgPrefix = await join(appData, "backgrounds");
      const migrated = saved.startsWith(bgPrefix) ? saved : await migrateBackground(saved, appData);
      if (migrated) {
        chatBackgroundStore.set(migrated);
      }
    }
    const savedFontSize = await getSetting<number>("font_size");
    if (savedFontSize !== undefined && savedFontSize >= 0.5 && savedFontSize <= 2.0) {
      fontSizeScale.set(savedFontSize);
    }
  });
</script>

{#if !bgUrl}<Background />{/if}
<div class="app-layout" style={bgUrl ? `--bg-url: url("${bgUrl}")` : ""}>
  {@render children?.()}
</div>

<style>
  .app-layout {
    min-height: 100dvh;
    max-width: var(--layout-max-width, 1600px);
    margin: 0 auto;
    background-image: linear-gradient(rgba(0,0,0,0.5), rgba(0,0,0,0.5)), var(--bg-url, none);
    background-position: center;
    background-size: cover;
    background-repeat: no-repeat;
    background-attachment: fixed;
    color: var(--text-primary);
    padding-top: var(--safe-area-top);
    padding-bottom: var(--safe-area-bottom);
    padding-left: var(--safe-area-left);
    padding-right: var(--safe-area-right);
  }
</style>
