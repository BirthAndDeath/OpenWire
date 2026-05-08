<script lang="ts">
  import "../lib/i18n";
  import { initTheme } from "../lib/theme";
  import { initLanguage } from "../lib/language";
  import Background from "./Background.svelte";
  import { _ } from "svelte-i18n";

  let { children }: { children?: () => any } = $props();

  // 在布局层初始化主题和语言（只加载一次）
  $effect(() => {
    async function initializeSettings() {
      // 并行初始化主题和语言
      await Promise.all([initTheme(), initLanguage()]);
    }

    initializeSettings().catch(console.error);
  });
</script>

<Background />
<div class="app-layout">
  {@render children?.()}
</div>

<style>
  .app-layout {
    min-height: 100vh;
    background: transparent;
    backdrop-filter: none;
    color: var(--text-primary);
  }
</style>
