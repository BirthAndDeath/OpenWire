<script lang="ts">
  import "../lib/i18n";
  import { initTheme } from '../lib/theme';
  import { initLanguage } from '../lib/language';

  let { children }: { children?: () => any } = $props();

  // 在布局层初始化主题和语言（只加载一次）
  $effect(() => {
    async function initializeSettings() {
      // 并行初始化主题和语言
      await Promise.all([
        initTheme(),
        initLanguage()
      ]);
    }
    
    initializeSettings();
  });
</script>

{@render children?.()}
