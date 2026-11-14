<script lang="ts">
  let {
    size = 20,
    className = "",
  }: { size?: number; className?: string } = $props();
</script>

<!-- 原子旋转加载圈：纯圆环 + 旋转动画，颜色继承 currentColor -->
<span
  class="spinner {className}"
  style:--spinner-size={`${size}px`}
  aria-hidden="true"
>
  <svg width={size} height={size} viewBox="0 0 24 24">
    <circle
      cx="12"
      cy="12"
      r="10"
      stroke="currentColor"
      stroke-width="2"
      fill="none"
      stroke-dasharray="32"
    />
  </svg>
</span>

<style>
  .spinner {
    display: inline-flex;
    width: var(--spinner-size);
    height: var(--spinner-size);
    flex-shrink: 0;
  }
  .spinner svg {
    animation: spin 0.8s linear infinite;
  }
  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }
</style>
