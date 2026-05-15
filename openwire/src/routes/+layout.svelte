<script lang="ts">
  import "../lib/i18n";
  import { initTheme } from "../lib/theme";
  import { initLanguage } from "../lib/language";
  import { initSettingsStore, getSetting } from "../lib/settings";
  import { invoke } from "@tauri-apps/api/core";
  import {
    downloadDir as getSystemDownloadDir,
    documentDir as getSystemDocumentDir,
  } from "@tauri-apps/api/path";
  import Background from "./Background.svelte";
  import { _ } from "svelte-i18n";

  let { children }: { children?: () => any } = $props();

  // 在布局层初始化主题、语言和下载目录（只加载一次）
  $effect(() => {
    async function initializeSettings() {
      // 并行初始化主题和语言
      await Promise.all([initTheme(), initLanguage()]);

      // 初始化下载目录：如果未设置，fallback 到系统下载文件夹并同步到后端
      try {
        await initSettingsStore();
        const savedDownload = await getSetting<string>("download_dir");
        if (!savedDownload) {
          const systemDir = await getSystemDownloadDir();
          // 同步到后端核心，确保文件下载使用系统下载文件夹
          await invoke("set_download_dir", { path: systemDir });
        }
      } catch (e) {
        console.error("初始化下载目录失败:", e);
      }

      // 初始化上传目录：如果未设置，fallback 到系统文档文件夹
      try {
        const savedUpload = await getSetting<string>("upload_dir");
        if (!savedUpload) {
          const systemDir = await getSystemDocumentDir();
          // 持久化到前端设置存储
          const { setSetting } = await import("../lib/settings");
          await setSetting("upload_dir", systemDir);
        }
      } catch (e) {
        console.error("初始化上传目录失败:", e);
      }
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
