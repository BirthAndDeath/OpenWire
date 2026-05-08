<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import { _ } from "svelte-i18n";
  import {
    getSetting,
    initSettingsStore,
    screenshotProtectionStore,
  } from "../lib/settings";
  import { getCurrentWindow } from "@tauri-apps/api/window";

  // ===== 鼠标位置状态 =====
  let mouseX = 0;
  let mouseY = 0;

  // ===== 容器引用 =====
  let containerRef: HTMLDivElement;
  let rect = { width: 0, height: 0, left: 0, top: 0 };

  // ===== 动画帧相关 =====
  let animFrameId: number | null = null;

  // ===== 目标值（鼠标映射后的最终目标瞳孔位置） =====
  let targetPupilX = 0;
  let targetPupilY = 0;

  // ===== 当前值（实际渲染值，通过 lerp 逼近目标） =====
  let currentPupilX = 0;
  let currentPupilY = 0;

  // ===== 鼠标活动状态 =====
  let mouseActive = false;

  // ===== 截屏保护 =====
  let screenshotProtection = false;
  let unsubScreenshot: (() => void) | null = null;

  // ===== 缓动函数 =====
  function lerp(from: number, to: number, t: number): number {
    return from + (to - from) * t;
  }

  // ===== 处理鼠标移动 =====
  function handleMouseMove(e: MouseEvent) {
    if (!containerRef) return;

    // 更新矩形信息（防止窗口 resize 导致偏差）
    rect = containerRef.getBoundingClientRect();

    // 计算鼠标在容器内的相对位置 (-0.5 到 0.5)
    const x = (e.clientX - rect.left) / rect.width - 0.5;
    const y = (e.clientY - rect.top) / rect.height - 0.5;

    mouseX = x;
    mouseY = y;
    mouseActive = true;

    // 计算目标瞳孔偏移量（限制最大移动距离）
    const maxOffset = Math.min(rect.width, rect.height) * 0.045;
    targetPupilX = Math.max(-maxOffset, Math.min(maxOffset, x * maxOffset));
    targetPupilY = Math.max(-maxOffset, Math.min(maxOffset, y * maxOffset));
  }

  function handleMouseLeave() {
    mouseActive = false;
    // 鼠标离开时，目标回到中心
    targetPupilX = 0;
    targetPupilY = 0;
  }

  // ===== 主循环：平滑缓动跟随 =====
  function animationLoop() {
    // 鼠标活动时快速跟随，离开时缓慢回中
    const speed = mouseActive ? 0.18 : 0.04;
    currentPupilX = lerp(currentPupilX, targetPupilX, speed);
    currentPupilY = lerp(currentPupilY, targetPupilY, speed);

    animFrameId = requestAnimationFrame(animationLoop);
  }

  // ===== 生命周期 =====
  onMount(async () => {
    // 加载截屏保护设置
    await initSettingsStore();
    const saved = await getSetting<boolean>("screenshot_protection");
    if (saved !== undefined) {
      screenshotProtection = saved;
      // 同步共享 store，确保订阅时初始值一致
      screenshotProtectionStore.set(saved);
    }

    // 订阅共享 store，实时响应设置页面的变更
    unsubScreenshot = screenshotProtectionStore.subscribe((value) => {
      screenshotProtection = value;
      // 切换时重置瞳孔偏移到中心
      targetPupilX = 0;
      targetPupilY = 0;
      currentPupilX = 0;
      currentPupilY = 0;
      // 同步更新窗口截屏保护状态
      try {
        const appWindow = getCurrentWindow();
        appWindow.setContentProtected(value);
      } catch (e) {
        console.error("设置截屏保护失败:", e);
      }
    });

    window.addEventListener("mousemove", handleMouseMove);
    window.addEventListener("mouseleave", handleMouseLeave);
    animFrameId = requestAnimationFrame(animationLoop);
  });

  onDestroy(() => {
    window.removeEventListener("mousemove", handleMouseMove);
    window.removeEventListener("mouseleave", handleMouseLeave);
    if (animFrameId !== null) {
      cancelAnimationFrame(animFrameId);
    }
    if (unsubScreenshot) {
      unsubScreenshot();
    }
  });

  // 暴露给模板的响应式值
  $: pupilX = currentPupilX;
  $: pupilY = currentPupilY;
</script>

<div class="bg-canvas"></div>

<!-- 眼睛容器 -->
<div class="eye-container" bind:this={containerRef}>
  {#if screenshotProtection}
    <!--
      Google Material Icons
      by Material Design Authors
      License: Apache 2.0
    -->
    <svg
      class="eye-icon"
      xmlns="http://www.w3.org/2000/svg"
      width="24"
      height="24"
      viewBox="0 0 24 24"
    >
      <rect width="24" height="24" fill="none" />
      <path
        fill="currentColor"
        d="M11.83 9L15 12.16V12a3 3 0 0 0-3-3zm-4.3.8l1.55 1.55c-.05.21-.08.42-.08.65a3 3 0 0 0 3 3c.22 0 .44-.03.65-.08l1.55 1.55c-.67.33-1.41.53-2.2.53a5 5 0 0 1-5-5c0-.79.2-1.53.53-2.2M2 4.27l2.28 2.28l.45.45C3.08 8.3 1.78 10 1 12c1.73 4.39 6 7.5 11 7.5c1.55 0 3.03-.3 4.38-.84l.43.42L19.73 22L21 20.73L3.27 3M12 7a5 5 0 0 1 5 5c0 .64-.13 1.26-.36 1.82l2.93 2.93c1.5-1.25 2.7-2.89 3.43-4.75c-1.73-4.39-6-7.5-11-7.5c-1.4 0-2.74.25-4 .7l2.17 2.15C10.74 7.13 11.35 7 12 7"
      />
    </svg>
  {:else}
    <!--
      Google Material Icons
      by Material Design Authors
      License: Apache 2.0
    -->
    <svg
      class="eye-icon"
      xmlns="http://www.w3.org/2000/svg"
      width="24"
      height="24"
      viewBox="0 0 24 24"
    >
      <rect width="24" height="24" fill="none" />
      <!--
        眼眶填充 + 中间圆形镂空（evenodd 使中间瞳孔区域透明）
        外部 div 瞳孔从镂空圆孔处透出
      -->
      <path
        fill="currentColor"
        fill-rule="evenodd"
        d="M12 4.5C7 4.5 2.73 7.61 1 12c1.73 4.39 6 7.5 11 7.5s9.27-3.11 11-7.5c-1.73-4.39-6-7.5-11-7.5M12 17c-2.76 0-5-2.24-5-5s2.24-5 5-5s5 2.24 5 5s-2.24 5-5 5"
      />
    </svg>

    <!-- 动态瞳孔层（外部 div，跟随鼠标偏移） -->
    <div class="pupil" style="--tx: {pupilX}px; --ty: {pupilY}px;"></div>
  {/if}
</div>

<!-- 谨言慎行文字 -->
<div class="caution-text">
  {$_("caution_words")}
</div>

<style>
  /* 引入必要的 CSS 变量和样式，确保眼睛显示正常 */
  :global(:root) {
    --theme-color: #3a7ca5;
    --theme-r: 58;
    --theme-g: 124;
    --theme-b: 165;
    --card-dx: 0;
    --card-dy: 0;
    /* 定义眼睛的基础颜色，随主题变化 */
    --eye-base-color: #ffffff;
    --eye-shadow-color: rgba(
      var(--theme-r),
      var(--theme-g),
      var(--theme-b),
      0.4
    );
  }

  .eye-container {
    aspect-ratio: 1/1; /* 调整为正方形以适应图标 */
    z-index: 0; /* 降低层级，作为背景 */
    --eye-scale: 1;
    width: min(300px, 50%); /* 增大尺寸 */
    height: auto;
    transition:
      transform 0.5s cubic-bezier(0.2, 0.8, 0.2, 1),
      filter 0.5s,
      opacity 0.5s,
      color 0.5s,
      background-color 0.5s,
      box-shadow 0.5s;
    position: fixed; /* 固定在视口中心 */
    top: 50%;
    left: 50%;
    transform: translate(-50%, -50%);
    pointer-events: none; /* 不阻挡鼠标事件 */

    /* 移除背景效果 */
    opacity: 0.8;
    color: var(--theme-color); /* 使用主题色 */
    background-color: transparent;
    backdrop-filter: none;
    -webkit-backdrop-filter: none;
    border-radius: 0;
    border: none;
    box-shadow: none;

    /* 确保子元素定位基准 */
    display: flex;
    justify-content: center;
    align-items: center;
  }

  .eye-icon {
    width: 100%;
    height: 100%;
    display: block;
    position: relative;
    z-index: 1;
    overflow: visible;
  }

  /* 平面瞳孔样式 */
  .pupil {
    position: absolute;
    width: 25%; /* 瞳孔大小，相对于容器 */
    height: 25%;
    background-color: currentColor; /* 纯色平面，跟随主题色 */
    border-radius: 50%;
    z-index: 2;
    /* 初始居中 */
    top: 50%;
    left: 50%;
    /* 应用偏移量，使用 translate3d 开启硬件加速 */
    transform: translate(calc(-50% + var(--tx)), calc(-50% + var(--ty)));
    /* 由 requestAnimationFrame 驱动缓动，移除 CSS transition 避免冲突 */
    transition: none;
    opacity: 1;
  }

  /* 响应式调整 */
  @media (max-width: 768px) {
    .eye-container {
      width: min(150px, 40vw); /* 移动端稍微缩小 */
      transform: translate(-50%, -50%) scale(0.8);
    }

    .pupil {
      width: 25%; /* 移动端瞳孔稍大 */
      height: 25%;
    }
  }

  /* 谨言慎行文字样式 */
  .caution-text {
    position: fixed;
    top: calc(50% + min(300px, 50%) / 2 + 20px);
    left: 50%;
    transform: translateX(-50%);
    font-size: 18px;
    font-weight: 500;
    color: var(--theme-color);
    opacity: 0.7;
    letter-spacing: 4px;
    text-shadow: 0 2px 8px
      rgba(var(--theme-r), var(--theme-g), var(--theme-b), 0.3);
    pointer-events: none;
    z-index: 0;
    transition: all 0.5s ease;
  }

  /* 移动端响应式调整 */
  @media (max-width: 768px) {
    .caution-text {
      top: calc(50% + min(150px, 40vw) / 2 * 0.8 + 15px);
      font-size: 14px;
      letter-spacing: 2px;
    }
  }
</style>
