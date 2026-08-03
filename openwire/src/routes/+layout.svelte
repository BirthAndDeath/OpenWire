<script lang="ts">
  import "../lib/i18n";
  import "../lib/styles/variables.css";
  import { initTheme } from "../lib/theme";
  import { initLanguage } from "../lib/language";
  import Background from "./Background.svelte";
  import { _ } from "svelte-i18n";

  let { children }: { children?: () => any } = $props();

  // 在布局层初始化主题和语言
  $effect(() => {
    async function initializeSettings() {
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
    min-height: 100dvh;
    max-width: var(--layout-max-width, 1600px);
    margin: 0 auto;
    background: transparent;
    color: var(--text-primary);
    padding-top: var(--safe-area-top);
    padding-bottom: var(--safe-area-bottom);
    padding-left: var(--safe-area-left);
    padding-right: var(--safe-area-right);
  }
</style>
