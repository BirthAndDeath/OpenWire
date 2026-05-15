<script lang="ts">
  import "../../lib/i18n";
  import { _, locale } from "svelte-i18n";
  import { goto } from "$app/navigation";
  import { theme, setTheme } from "../../lib/theme";
  import { language, setLanguage } from "../../lib/language";
  import {
    getSetting,
    setSetting,
    initSettingsStore,
    screenshotProtectionStore,
  } from "../../lib/settings";
  import { open } from "@tauri-apps/plugin-dialog";
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import { invoke } from "@tauri-apps/api/core";
  import {
    downloadDir as getSystemDownloadDir,
    documentDir as getSystemDocumentDir,
  } from "@tauri-apps/api/path";
  import PasswordInput from "../../lib/PasswordInput.svelte";

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

  // 下载目录
  let downloadDir = $state("");

  // 上传文件搜寻目录
  let uploadDir = $state("");

  // 截屏保护
  let screenshotProtection = $state(false);

  // Keyring 可用性（隔离层检查）
  let keyringAvailable = $state(false);
  let keyringCheckDone = $state(false);

  // 等待全局状态初始化完成
  $effect(() => {
    // 订阅 theme store，当它有值时说明已初始化
    const unsubscribeTheme = theme.subscribe((val) => {
      if (val) isLoading = false;
    });
    const unsubscribeLang = language.subscribe(() => {});

    return () => {
      unsubscribeTheme();
      unsubscribeLang();
    };
  });

  // 加载设置
  $effect(() => {
    async function loadSettings() {
      // 确保 settings store 已初始化，否则 getSetting 会返回 undefined
      await initSettingsStore();

      // 检查 Keyring 可用性（隔离层）
      try {
        keyringAvailable = await invoke<boolean>("is_keyring_available");
        keyringCheckDone = true;
        console.log("Keyring available:", keyringAvailable);
      } catch (e) {
        console.error("检查 Keyring 可用性失败:", e);
        keyringCheckDone = true;
      }

      // 获取下载目录（前端持久化存储，未设置时 fallback 到系统下载文件夹）
      try {
        const saved = await getSetting<string>("download_dir");
        if (saved) {
          downloadDir = saved;
        } else {
          // 未设置时 fallback 到系统下载文件夹
          downloadDir = await getSystemDownloadDir();
          // 同步到后端核心，确保首次使用时下载目录正确
          try {
            await invoke("set_download_dir", { path: downloadDir });
          } catch (e) {
            console.error("同步默认下载目录到后端失败:", e);
          }
        }
      } catch (e) {
        console.error("获取下载目录失败:", e);
        // 极端 fallback
        try {
          downloadDir = await getSystemDownloadDir();
        } catch {}
      }

      // 获取上传文件搜寻目录（前端持久化存储，未设置时 fallback 到系统文档文件夹）
      try {
        const saved = await getSetting<string>("upload_dir");
        if (saved) {
          uploadDir = saved;
        } else {
          // 未设置时 fallback 到系统文档文件夹
          uploadDir = await getSystemDocumentDir();
        }
      } catch (e) {
        console.error("获取上传目录失败:", e);
        // 极端 fallback
        try {
          uploadDir = await getSystemDocumentDir();
        } catch {}
      }

      // 获取截屏保护设置
      try {
        const saved = await getSetting<boolean>("screenshot_protection");
        if (saved !== undefined) {
          screenshotProtection = saved;
          // 应用截屏保护状态到窗口
          const appWindow = getCurrentWindow();
          await appWindow.setContentProtected(screenshotProtection);
        }
      } catch (e) {
        console.error("获取截屏保护设置失败:", e);
      }
    }

    if (!isLoading) {
      loadSettings();
    }
  });

  // 切换语言
  async function changeLanguage(lang: string) {
    await setLanguage(lang);
  }

  // 返回首页
  function goBack() {
    goto("/");
  }

  // 选择下载目录
  async function selectDownloadDir() {
    try {
      const selected = await open({
        directory: true,
        multiple: false,
        title: "选择下载目录",
      });
      if (selected) {
        // 持久化到前端设置存储
        await setSetting("download_dir", selected);
        downloadDir = selected;
        // 同步到后端核心，确保文件下载使用正确的目录
        try {
          await invoke("set_download_dir", { path: selected });
        } catch (e) {
          console.error("同步下载目录到后端失败:", e);
        }
      }
    } catch (e) {
      console.error("选择下载目录失败:", e);
    }
  }

  // 选择上传文件搜寻目录
  async function selectUploadDir() {
    try {
      const selected = await open({
        directory: true,
        multiple: false,
        title: "选择上传文件搜寻目录",
      });
      if (selected) {
        uploadDir = selected;
        await setSetting("upload_dir", selected);
      }
    } catch (e) {
      console.error("选择上传目录失败:", e);
    }
  }

  // 切换截屏保护
  async function toggleScreenshotProtection() {
    screenshotProtection = !screenshotProtection;
    await setSetting("screenshot_protection", screenshotProtection);
    // 更新共享 store，通知 Background.svelte
    screenshotProtectionStore.set(screenshotProtection);
    try {
      const appWindow = getCurrentWindow();
      await appWindow.setContentProtected(screenshotProtection);
    } catch (e) {
      console.error("设置截屏保护失败:", e);
    }
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
            onchange={(e) =>
              setTheme(
                (e.target as HTMLSelectElement).value as "dark" | "light",
              )}
          >
            {#each themes as t}
              <option value={t.value}>{t.label}</option>
            {/each}
          </select>
        </div>
      </section>

      <!-- 截屏保护 -->
      <section class="settings-section">
        <h2>{$_("screenshot_protection")}</h2>
        <div class="toggle-setting">
          <label class="toggle-label" for="screenshot-toggle">
            <span class="toggle-desc">{$_("screenshot_protection_desc")}</span>
            <span class="toggle-note">Linux 上此功能可能不生效</span>
          </label>
          <button
            id="screenshot-toggle"
            class="toggle-button"
            class:active={screenshotProtection}
            onclick={toggleScreenshotProtection}
            role="switch"
            aria-checked={screenshotProtection}
            aria-label={$_("screenshot_protection")}
          >
            <span class="toggle-knob"></span>
          </button>
        </div>
      </section>

      <!-- 文件设置：下载目录 + 上传文件搜寻目录 -->
      <section class="settings-section">
        <h2>{$_("file_settings")}</h2>

        <!-- 下载目录 -->
        <div class="dir-setting">
          <div class="dir-setting-header">
            <span class="dir-setting-label">{$_("download_settings")}</span>
          </div>
          <div class="dir-path">
            <span class="dir-path-label">{$_("current_download_dir")}:</span>
            <span class="dir-path-value">{downloadDir || $_("not_set")}</span>
          </div>
          <button class="select-dir-button" onclick={selectDownloadDir}>
            📁 {$_("select_download_dir")}
          </button>
        </div>

        <!-- 上传文件搜寻目录 -->
        <div class="dir-setting">
          <div class="dir-setting-header">
            <span class="dir-setting-label">{$_("upload_settings")}</span>
          </div>
          <div class="dir-path">
            <span class="dir-path-label">{$_("current_upload_dir")}:</span>
            <span class="dir-path-value">{uploadDir || $_("not_set")}</span>
          </div>
          <button class="select-dir-button" onclick={selectUploadDir}>
            📁 {$_("select_upload_dir")}
          </button>
        </div>
      </section>

      <!-- 密码设置（仅 Keyring 不可用时显示） -->
      {#if keyringCheckDone && !keyringAvailable}
        <section class="settings-section">
          <h2>{$_("password_settings")}</h2>
          <p class="section-desc">
            {$_("password_settings_desc")}
          </p>
          <PasswordInput />
        </section>
      {:else if keyringCheckDone && keyringAvailable}
        <!-- Keyring 可用时显示提示信息 -->
        <section class="settings-section">
          <h2>{$_("password_settings")}</h2>
          <p class="section-desc">
            {$_("password_settings_desc")}
          </p>
          <div class="keyring-available-note">
            <span class="keyring-icon">🔑</span>
            <span>{$_("keyring_available_note")}</span>
          </div>
        </section>
      {/if}
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

  .section-desc {
    font-size: 13px;
    color: var(--text-secondary, #737373);
    margin: -8px 0 16px 0;
    line-height: 1.5;
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

  /* 目录设置样式 */
  .dir-setting {
    margin-bottom: 20px;
    padding: 12px;
    background: var(--bg-secondary, #0a0a0a);
    border: 1px solid var(--border-color, #2a2a2a);
    border-radius: 6px;
  }

  .dir-setting:last-child {
    margin-bottom: 0;
  }

  .dir-setting-header {
    margin-bottom: 8px;
  }

  .dir-path {
    display: flex;
    align-items: flex-start;
    gap: 8px;
    margin-bottom: 12px;
    font-size: 13px;
    word-break: break-all;
  }

  .dir-path-label {
    color: var(--text-secondary, #737373);
    white-space: nowrap;
    flex-shrink: 0;
  }

  .dir-path-value {
    color: var(--text-primary, #fafafa);
    font-family: monospace;
    background: var(--bg-tertiary, #1a1a1a);
    padding: 4px 8px;
    border-radius: 4px;
    font-size: 12px;
  }

  .select-dir-button {
    background: transparent;
    border: 1px solid var(--border-color, #2a2a2a);
    border-radius: 6px;
    padding: 8px 16px;
    color: var(--text-primary, #fafafa);
    cursor: pointer;
    font-size: 13px;
    transition: all 0.2s;
    display: inline-flex;
    align-items: center;
    gap: 6px;
  }

  .select-dir-button:hover {
    background: var(--bg-tertiary, #1a1a1a);
    border-color: #3b82f6;
    color: #3b82f6;
  }

  /* 开关样式 */
  .toggle-setting {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: 16px;
  }

  .toggle-label {
    flex: 1;
    cursor: pointer;
  }

  .toggle-desc {
    font-size: 13px;
    color: var(--text-secondary, #737373);
    line-height: 1.5;
  }

  .toggle-note {
    display: block;
    margin-top: 6px;
    font-size: 11px;
    color: var(--text-secondary, #737373);
    opacity: 0.6;
    font-style: italic;
  }

  .toggle-button {
    flex-shrink: 0;
    position: relative;
    width: 48px;
    height: 26px;
    background: var(--bg-secondary, #0a0a0a);
    border: 1px solid var(--border-color, #2a2a2a);
    border-radius: 13px;
    cursor: pointer;
    transition: all 0.2s;
    padding: 0;
    margin-top: 2px;
  }

  .toggle-button.active {
    background: #3b82f6;
    border-color: #3b82f6;
  }

  .toggle-knob {
    position: absolute;
    top: 2px;
    left: 2px;
    width: 20px;
    height: 20px;
    background: #fafafa;
    border-radius: 50%;
    transition: transform 0.2s;
  }

  .toggle-button.active .toggle-knob {
    transform: translateX(22px);
  }

  /* Keyring 可用提示 */
  .keyring-available-note {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 12px 16px;
    background: rgba(16, 185, 129, 0.1);
    border: 1px solid rgba(16, 185, 129, 0.2);
    border-radius: 8px;
    font-size: 13px;
    color: #10b981;
    line-height: 1.5;
  }

  .keyring-icon {
    font-size: 20px;
    flex-shrink: 0;
  }
</style>
