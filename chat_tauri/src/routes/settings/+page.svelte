<script lang="ts">
  import "../../lib/i18n";
  import { _, locale } from "svelte-i18n";
  import { goto } from "$app/navigation";
  import { theme, setTheme } from '../../lib/theme';
  import { language, setLanguage } from '../../lib/language';

  // 语言选项
  const languages = [
    { code: "en", name: "English" },
    { code: "zh", name: "中文" },
    { code: "fr", name: "Français" },
    { code: "es", name: "Español" },
    { code: "de", name: "Deutsch" },
    { code: "ja", name: "日本語" },
  ];

  // 主题选项
  const themes = [
    { value: "dark", label: "暗色主题" },
    { value: "light", label: "亮色主题" },
  ];

  // 加载状态
  let isLoading = $state(true);

  // 等待全局状态初始化完成
  $effect(() => {
    // 订阅 theme 和 language store，当它们有值时说明已初始化
    const unsubscribeTheme = theme.subscribe(() => {});
    const unsubscribeLang = language.subscribe(() => {});
    
    // 简单延迟以确保 Store 已加载（实际初始化在 layout 中完成）
    setTimeout(() => {
      isLoading = false;
    }, 100);
    
    return () => {
      unsubscribeTheme();
      unsubscribeLang();
    };
  });

  // 切换语言
  async function changeLanguage(lang: string) {
    await setLanguage(lang);
  }

  // 返回首页
  function goBack() {
    goto("/");
  }
</script>

{#if isLoading}
  <div class="loading-state">
    <p>{$_("loading")}...</p>
  </div>
{:else}
  <div class="settings-container">
    <header class="settings-header">
      <button class="back-button" onclick={goBack} aria-label="返回主页">
        ← {$_("back")}
      </button>
      <h1>{$_("settings")}</h1>
    </header>

    <main class="settings-content">
      <section class="settings-section">
        <h2>{$_("language_settings")}</h2>
        <div class="language-selector">
          <label for="lang-select">{$_("select_language")}:</label>
          <select
            id="lang-select"
            value={$language}
            onchange={(e) =>
              changeLanguage((e.target as HTMLSelectElement).value)}
          >
            {#each languages as lang}
              <option value={lang.code}>{lang.name}</option>
            {/each}
          </select>
        </div>
      </section>

      <section class="settings-section">
        <h2>{$_("appearance_settings")}</h2>
        <div class="theme-selector">
          <label for="theme-select">{$_("select_theme")}:</label>
          <select
            id="theme-select"
            value={$theme}
            onchange={(e) => setTheme((e.target as HTMLSelectElement).value as 'dark' | 'light')}
          >
            {#each themes as t}
              <option value={t.value}>{t.label}</option>
            {/each}
          </select>
        </div>
      </section>

      <!-- 可以在此添加更多设置项 -->
      <section class="settings-section">
        <h2>{$_("general_settings")}</h2>
        <p class="placeholder-text">{$_("more_settings_coming_soon")}</p>
      </section>
    </main>
  </div>
{/if}

<style>
  .settings-container {
    display: flex;
    flex-direction: column;
    height: 100vh;
    background: transparent;
    color: var(--text-primary, #f6f6f6);
    font-family: system-ui;
    position: relative;
    z-index: 1;
  }

  .settings-header {
    display: flex;
    align-items: center;
    gap: 16px;
    padding: 16px 24px;
    border-bottom: 1px solid var(--border-color, #2a2a2a);
    background: var(--bg-secondary, #0a0a0a);
  }

  .back-button {
    background: transparent;
    border: 1px solid var(--border-color, #2a2a2a);
    border-radius: 6px;
    padding: 8px 16px;
    color: var(--text-primary, #fafafa);
    cursor: pointer;
    font-size: 14px;
    transition: all 0.2s;
  }

  .back-button:hover {
    background: var(--bg-tertiary, #1a1a1a);
    border-color: #3b82f6;
    color: #3b82f6;
  }

  .settings-header h1 {
    margin: 0;
    font-size: 24px;
    font-weight: 600;
    color: var(--text-primary, #fafafa);
  }

  .settings-content {
    flex: 1;
    overflow-y: auto;
    padding: 24px;
  }

  .settings-section {
    margin-bottom: 32px;
    background: var(--bg-tertiary, #1a1a1a);
    border: 1px solid var(--border-color, #2a2a2a);
    border-radius: 8px;
    padding: 20px;
  }

  .settings-section h2 {
    margin: 0 0 16px 0;
    font-size: 18px;
    font-weight: 600;
    color: var(--text-primary, #fafafa);
    border-bottom: 1px solid var(--border-color, #2a2a2a);
    padding-bottom: 8px;
  }

  .language-selector,
  .theme-selector {
    display: flex;
    align-items: center;
    gap: 12px;
  }

  .language-selector label,
  .theme-selector label {
    font-size: 14px;
    color: var(--text-primary, #fafafa);
    white-space: nowrap;
  }

  .language-selector select,
  .theme-selector select {
    flex: 1;
    background: var(--bg-secondary, #0a0a0a);
    border: 1px solid var(--border-color, #2a2a2a);
    border-radius: 6px;
    padding: 8px 12px;
    color: var(--text-primary, #fafafa);
    font-size: 14px;
    cursor: pointer;
    transition: all 0.2s;
    -webkit-appearance: none;
    -moz-appearance: none;
    appearance: none;
    background-image: url("data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' width='12' height='12' viewBox='0 0 12 12'%3E%3Cpath fill='%23737373' d='M6 9L1 4h10z'/%3E%3C/svg%3E");
    background-repeat: no-repeat;
    background-position: right 12px center;
    padding-right: 36px;
  }

  .language-selector select:hover,
  .theme-selector select:hover {
    border-color: #3b82f6;
    background-color: var(--bg-tertiary, #1a1a1a);
  }

  .language-selector select:focus,
  .theme-selector select:focus {
    outline: none;
    border-color: #3b82f6;
    box-shadow: 0 0 0 2px rgba(59, 130, 246, 0.2);
  }

  .language-selector select option,
  .theme-selector select option {
    background: var(--bg-secondary, #0a0a0a);
    color: var(--text-primary, #fafafa);
    padding: 8px;
  }

  .placeholder-text {
    color: var(--text-secondary, #737373);
    font-style: italic;
    text-align: center;
    padding: 20px;
  }
</style>
