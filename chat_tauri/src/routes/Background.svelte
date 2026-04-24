<script>
  import { onMount, onDestroy } from "svelte";
  import { _ } from "svelte-i18n";

  // 鼠标位置状态
  let mouseX = 0;
  let mouseY = 0;

  // 容器引用，用于计算相对坐标
  /**
   * @type {HTMLDivElement}
   */
  let containerRef;
  let rect = { width: 0, height: 0, left: 0, top: 0 };

  // 处理鼠标移动
  /**
   * @param {{ clientX: number; clientY: number; }} e
   */
  function handleMouseMove(e) {
    if (!containerRef) return;

    // 更新矩形信息（防止窗口resize导致偏差）
    rect = containerRef.getBoundingClientRect();

    // 计算鼠标在容器内的相对位置 (-0.5 到 0.5)
    const x = (e.clientX - rect.left) / rect.width - 0.5;
    const y = (e.clientY - rect.top) / rect.height - 0.5;

    mouseX = x;
    mouseY = y;
  }

  onMount(() => {
    window.addEventListener("mousemove", handleMouseMove);
  });

  onDestroy(() => {
    window.removeEventListener("mousemove", handleMouseMove);
  });

  // 计算瞳孔偏移量 (限制最大移动距离)

  $: maxOffset = Math.min(rect.width, rect.height) * 0.045; // 留出一些边距
  $: pupilX = Math.max(-maxOffset, Math.min(maxOffset, mouseX * maxOffset));
  $: pupilY = Math.max(-maxOffset, Math.min(maxOffset, mouseY * maxOffset));
</script>

<div class="bg-canvas"></div>

<!-- 眼睛容器 -->
<div class="eye-container" bind:this={containerRef}>
  <!-- 
    Material Symbols by Google
    License: Apache 2.0
    作为静止的眼眶/背景
  -->
  <svg
    class="eye-icon"
    xmlns="http://www.w3.org/2000/svg"
    width="24"
    height="24"
    viewBox="0 0 24 24"
  >
    <path
      fill="currentColor"
      d="M3 23q-.825 0-1.412-.587T1 21v-2q0-.425.288-.712T2 18t.713.288T3 19v2h2q.425 0 .713.288T6 22t-.288.712T5 23zm18 0h-2q-.425 0-.712-.288T18 22t.288-.712T19 21h2v-2q0-.425.288-.712T22 18t.713.288T23 19v2q0 .825-.587 1.413T21 23m-9-4.5q-2.65 0-4.9-1.4t-3.525-3.825q-.15-.3-.225-.612t-.075-.638q0-.35.075-.675t.225-.625Q4.85 8.3 7.1 6.9T12 5.5t4.9 1.4t3.525 3.825q.15.3.225.613t.075.662t-.075.663t-.225.612Q19.15 15.7 16.9 17.1T12 18.5m0-3q1.45 0 2.475-1.025T15.5 12t-1.025-2.475T12 8.5T9.525 9.525T8.5 12t1.025 2.475T12 15.5M23 3v2q0 .425-.288.713T22 6t-.712-.288T21 5V3h-2q-.425 0-.712-.288T18 2t.288-.712T19 1h2q.825 0 1.413.588T23 3M3 1h2q.425 0 .713.288T6 2t-.288.713T5 3H3v2q0 .425-.288.713T2 6t-.712-.288T1 5V3q0-.825.588-1.412T3 1"
    />
  </svg>

  <!-- 动态瞳孔层 -->
  <div class="pupil" style="--tx: {pupilX}px; --ty: {pupilY}px;"></div>
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
    /* 确保图标居中且静止 */
    position: relative;
    z-index: 1;
  }

  /* 新增：瞳孔样式 */
  .pupil {
    position: absolute;
    width: 20%; /* 瞳孔大小，相对于容器 */
    height: 20%;
    background-color: currentColor; /* 跟随主题色 */
    border-radius: 50%;
    z-index: 2;
    /* 初始居中 */
    top: 50%;
    left: 50%;
    /* 应用偏移量，使用 translate3d 开启硬件加速 */
    transform: translate(calc(-50% + var(--tx)), calc(-50% + var(--ty)));
    /* 更平滑的过渡，增加过渡时间 */
    transition: transform 0.3s cubic-bezier(0.4, 0, 0.2, 1);
    box-shadow:
      0 0 15px currentColor,
      inset 0 0 5px rgba(255, 255, 255, 0.5);
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
