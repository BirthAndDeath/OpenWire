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

  // 主题选项（使用 $derived 响应语言切换）
  let themes = $derived([
    { value: "dark", label: $_("theme_dark") },
    { value: "light", label: $_("theme_light") },
  ]);

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

  // ============================================================
  // 节点配置（bootstrap / relay）
  // ============================================================
  interface NodeEntry {
    peerId: string;
    multiaddr: string;
  }

  let relayNodes = $state<NodeEntry[]>([]);
  let bootstrapNodes = $state<NodeEntry[]>([]);
  let nodesLoaded = $state(false);
  let nodesChanged = $state(false);
  let nodesSaving = $state(false);
  let nodesMessage = $state("");

  // 新增节点输入
  let newRelayPeerId = $state("");
  let newRelayAddr = $state("");
  let newBootstrapPeerId = $state("");
  let newBootstrapAddr = $state("");

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

      // 加载节点配置
      await loadNodesConfig();
    }

    if (!isLoading) {
      loadSettings();
    }
  });

  // ============================================================
  // 节点配置相关函数
  // ============================================================

  /** 从后端加载节点配置 */
  async function loadNodesConfig() {
    try {
      const jsonStr = await invoke<string>("get_nodes_config");
      const data = JSON.parse(jsonStr);
      relayNodes = (data.relay_nodes || []).map((n: [string, string]) => ({
        peerId: n[0],
        multiaddr: n[1],
      }));
      bootstrapNodes = (data.bootstrap_nodes || []).map((n: [string, string]) => ({
        peerId: n[0],
        multiaddr: n[1],
      }));
      nodesLoaded = true;
      nodesChanged = false;
      console.log(`已加载节点配置: ${relayNodes.length} relay, ${bootstrapNodes.length} bootstrap`);
    } catch (e) {
      console.error("加载节点配置失败:", e);
      nodesLoaded = true;
    }
  }

  /** 添加 relay 节点 */
  function addRelayNode() {
    const peerId = newRelayPeerId.trim();
    const addr = newRelayAddr.trim();
    if (!peerId || !addr) return;
    // 检查重复
    if (relayNodes.some(n => n.peerId === peerId && n.multiaddr === addr)) return;
    relayNodes = [...relayNodes, { peerId, multiaddr: addr }];
    newRelayPeerId = "";
    newRelayAddr = "";
    nodesChanged = true;
    nodesMessage = "";
  }

  /** 删除 relay 节点 */
  function removeRelayNode(index: number) {
    relayNodes = relayNodes.filter((_, i) => i !== index);
    nodesChanged = true;
    nodesMessage = "";
  }

  /** 添加 bootstrap 节点 */
  function addBootstrapNode() {
    const peerId = newBootstrapPeerId.trim();
    const addr = newBootstrapAddr.trim();
    if (!peerId || !addr) return;
    if (bootstrapNodes.some(n => n.peerId === peerId && n.multiaddr === addr)) return;
    bootstrapNodes = [...bootstrapNodes, { peerId, multiaddr: addr }];
    newBootstrapPeerId = "";
    newBootstrapAddr = "";
    nodesChanged = true;
    nodesMessage = "";
  }

  /** 删除 bootstrap 节点 */
  function removeBootstrapNode(index: number) {
    bootstrapNodes = bootstrapNodes.filter((_, i) => i !== index);
    nodesChanged = true;
    nodesMessage = "";
  }

  /** 重置为默认节点配置（通过后端 API） */
  async function resetNodesToDefault() {
    nodesSaving = true;
    nodesMessage = "";
    try {
      const jsonStr = await invoke<string>("reset_nodes_config");
      const data = JSON.parse(jsonStr);
      relayNodes = (data.relay_nodes || []).map((n: [string, string]) => ({
        peerId: n[0],
        multiaddr: n[1],
      }));
      bootstrapNodes = (data.bootstrap_nodes || []).map((n: [string, string]) => ({
        peerId: n[0],
        multiaddr: n[1],
      }));
      nodesChanged = false;
      nodesMessage = $_("config_saved_restart");
      console.log("节点配置已重置为默认值");
    } catch (e) {
      console.error("重置节点配置失败:", e);
      nodesMessage = $_("config_save_failed") + `: ${e}`;
    } finally {
      nodesSaving = false;
    }
  }

  /** 保存节点配置到后端 */
  async function saveNodesConfig() {
    nodesSaving = true;
    nodesMessage = "";
    try {
      await invoke("save_nodes_config", {
        relayNodes: relayNodes.map(n => [n.peerId, n.multiaddr]),
        bootstrapNodes: bootstrapNodes.map(n => [n.peerId, n.multiaddr]),
      });
      nodesChanged = false;
      nodesMessage = $_("config_saved_restart");
      console.log("节点配置已保存");
    } catch (e) {
      console.error("保存节点配置失败:", e);
      nodesMessage = $_("config_save_failed") + `: ${e}`;
    } finally {
      nodesSaving = false;
    }
  }

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

      <!-- ============================================================ -->
      <!-- 节点配置：Bootstrap + Relay -->
      <!-- ============================================================ -->
      <section class="settings-section">
        <h2>🌐 {$_("node_settings")}</h2>
        <p class="section-desc">
          {$_("node_settings_desc")}
        </p>

        {#if !nodesLoaded}
          <p class="loading-hint">{$_("loading_nodes")}</p>
        {:else}
          <!-- Relay 节点列表 -->
          <div class="node-group">
            <h3>🔁 {$_("relay_nodes")}（{relayNodes.length}）</h3>
            <p class="node-desc">
              {$_("relay_nodes_desc")}
            </p>

            <div class="node-list">
              {#if relayNodes.length === 0}
                <p class="empty-hint">{$_("no_relay_nodes")}</p>
              {:else}
                {#each relayNodes as node, i}
                  <div class="node-item">
                    <div class="node-info">
                      <span class="node-peerid" title={node.peerId}>{node.peerId.slice(0, 20)}...</span>
                      <span class="node-addr" title={node.multiaddr}>{node.multiaddr}</span>
                    </div>
                    <button class="node-remove-btn" onclick={() => removeRelayNode(i)} title={$_("delete")}>✕</button>
                  </div>
                {/each}
              {/if}
            </div>

            <!-- 新增 relay 节点 -->
            <div class="node-add-form">
              <input
                type="text"
                placeholder={$_("peer_id_placeholder_relay")}
                bind:value={newRelayPeerId}
                class="node-input peerid-input"
              />
              <input
                type="text"
                placeholder={$_("multiaddr_placeholder_relay")}
                bind:value={newRelayAddr}
                class="node-input addr-input"
              />
              <button class="node-add-btn" onclick={addRelayNode} disabled={!newRelayPeerId.trim() || !newRelayAddr.trim()}>
                ➕ {$_("add")}
              </button>
            </div>
          </div>

          <!-- Bootstrap 节点列表 -->
          <div class="node-group">
            <h3>🌱 {$_("bootstrap_nodes")}（{bootstrapNodes.length}）</h3>
            <p class="node-desc">
              {$_("bootstrap_nodes_desc")}
            </p>

            <div class="node-list">
              {#if bootstrapNodes.length === 0}
                <p class="empty-hint">{$_("no_bootstrap_nodes")}</p>
              {:else}
                {#each bootstrapNodes as node, i}
                  <div class="node-item">
                    <div class="node-info">
                      <span class="node-peerid" title={node.peerId}>{node.peerId.slice(0, 20)}...</span>
                      <span class="node-addr" title={node.multiaddr}>{node.multiaddr}</span>
                    </div>
                    <button class="node-remove-btn" onclick={() => removeBootstrapNode(i)} title={$_("delete")}>✕</button>
                  </div>
                {/each}
              {/if}
            </div>

            <!-- 新增 bootstrap 节点 -->
            <div class="node-add-form">
              <input
                type="text"
                placeholder={$_("peer_id_placeholder_bootstrap")}
                bind:value={newBootstrapPeerId}
                class="node-input peerid-input"
              />
              <input
                type="text"
                placeholder={$_("multiaddr_placeholder_bootstrap")}
                bind:value={newBootstrapAddr}
                class="node-input addr-input"
              />
              <button class="node-add-btn" onclick={addBootstrapNode} disabled={!newBootstrapPeerId.trim() || !newBootstrapAddr.trim()}>
                ➕ {$_("add")}
              </button>
            </div>
          </div>

          <!-- 操作按钮 -->
          <div class="node-actions">
            <button
              class="save-btn"
              onclick={saveNodesConfig}
              disabled={!nodesChanged || nodesSaving}
            >
              {nodesSaving ? $_("saving") : "💾 " + $_("save_config")}
            </button>
            <button class="reset-btn" onclick={resetNodesToDefault}>
              🔄 {$_("reset_default")}
            </button>
          </div>

          <!-- 提示消息 -->
          {#if nodesMessage}
            <div class="node-message" class:success={nodesMessage.startsWith("✅")} class:error={nodesMessage.startsWith("❌")}>
              {nodesMessage}
            </div>
          {/if}

          <!-- 重启提示 -->
          {#if nodesChanged}
            <div class="restart-hint">
              {$_("restart_hint")}
            </div>
          {/if}
        {/if}
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

  /* ============================================================ */
  /* 节点配置样式 */
  /* ============================================================ */

  .node-group {
    margin-bottom: 24px;
    padding: 16px;
    background: var(--bg-secondary, #0a0a0a);
    border: 1px solid var(--border-color, #2a2a2a);
    border-radius: 8px;
  }

  .node-group h3 {
    margin: 0 0 4px 0;
    font-size: 15px;
    font-weight: 600;
    color: var(--text-primary, #fafafa);
  }

  .node-desc {
    margin: 0 0 12px 0;
    font-size: 12px;
    color: var(--text-secondary, #737373);
    line-height: 1.4;
  }

  .node-list {
    margin-bottom: 12px;
  }

  .node-item {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
    padding: 8px 10px;
    margin-bottom: 4px;
    background: var(--bg-tertiary, #1a1a1a);
    border: 1px solid var(--border-color, #2a2a2a);
    border-radius: 4px;
    font-size: 12px;
  }

  .node-info {
    display: flex;
    flex-direction: column;
    gap: 2px;
    flex: 1;
    min-width: 0;
  }

  .node-peerid {
    color: var(--text-primary, #fafafa);
    font-family: monospace;
    font-size: 11px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .node-addr {
    color: var(--text-secondary, #737373);
    font-family: monospace;
    font-size: 11px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .node-remove-btn {
    background: transparent;
    border: none;
    color: #ef4444;
    cursor: pointer;
    font-size: 14px;
    padding: 2px 6px;
    border-radius: 4px;
    flex-shrink: 0;
    transition: all 0.2s;
  }

  .node-remove-btn:hover {
    background: rgba(239, 68, 68, 0.15);
  }

  .node-add-form {
    display: flex;
    gap: 6px;
    flex-wrap: wrap;
  }

  .node-input {
    flex: 1;
    min-width: 120px;
    background: var(--bg-secondary, #0a0a0a);
    border: 1px solid var(--border-color, #2a2a2a);
    border-radius: 4px;
    padding: 6px 10px;
    color: var(--text-primary, #fafafa);
    font-size: 12px;
    font-family: monospace;
    transition: border-color 0.2s;
  }

  .node-input:focus {
    outline: none;
    border-color: #3b82f6;
  }

  .node-input::placeholder {
    color: var(--text-secondary, #555);
    font-family: system-ui;
  }

  .peerid-input {
    flex: 1.5;
  }

  .addr-input {
    flex: 2;
  }

  .node-add-btn {
    background: transparent;
    border: 1px solid #3b82f6;
    border-radius: 4px;
    padding: 6px 12px;
    color: #3b82f6;
    cursor: pointer;
    font-size: 12px;
    white-space: nowrap;
    transition: all 0.2s;
  }

  .node-add-btn:hover:not(:disabled) {
    background: rgba(59, 130, 246, 0.15);
  }

  .node-add-btn:disabled {
    opacity: 0.4;
    cursor: not-allowed;
  }

  .empty-hint {
    color: var(--text-secondary, #555);
    font-size: 12px;
    font-style: italic;
    padding: 8px 0;
  }

  .loading-hint {
    color: var(--text-secondary, #737373);
    font-size: 13px;
    text-align: center;
    padding: 20px;
  }

  .node-actions {
    display: flex;
    gap: 10px;
    margin-top: 16px;
  }

  .save-btn {
    background: #3b82f6;
    border: none;
    border-radius: 6px;
    padding: 10px 20px;
    color: #fff;
    cursor: pointer;
    font-size: 14px;
    font-weight: 500;
    transition: all 0.2s;
  }

  .save-btn:hover:not(:disabled) {
    background: #2563eb;
  }

  .save-btn:disabled {
    opacity: 0.4;
    cursor: not-allowed;
  }

  .reset-btn {
    background: transparent;
    border: 1px solid var(--border-color, #2a2a2a);
    border-radius: 6px;
    padding: 10px 20px;
    color: var(--text-primary, #fafafa);
    cursor: pointer;
    font-size: 14px;
    transition: all 0.2s;
  }

  .reset-btn:hover {
    border-color: #f59e0b;
    color: #f59e0b;
  }

  .node-message {
    margin-top: 12px;
    padding: 10px 14px;
    border-radius: 6px;
    font-size: 13px;
    line-height: 1.4;
  }

  .node-message.success {
    background: rgba(16, 185, 129, 0.1);
    border: 1px solid rgba(16, 185, 129, 0.2);
    color: #10b981;
  }

  .node-message.error {
    background: rgba(239, 68, 68, 0.1);
    border: 1px solid rgba(239, 68, 68, 0.2);
    color: #ef4444;
  }

  .restart-hint {
    margin-top: 12px;
    padding: 10px 14px;
    border-radius: 6px;
    font-size: 13px;
    line-height: 1.4;
    background: rgba(245, 158, 11, 0.1);
    border: 1px solid rgba(245, 158, 11, 0.2);
    color: #f59e0b;
  }
</style>
