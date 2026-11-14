<script lang="ts">
  import { onMount } from "svelte";

  let {
    children,
  }: { children: import("svelte").Snippet } = $props();

  let ref: HTMLDivElement | undefined = $state();

  // 将子树物理移动到 <body>：脱离侧栏等可能带 transform/filter 的祖先，
  // 使内部 position: fixed 元素按视口正确定位（与根级弹窗一致）。
  onMount(() => {
    if (ref) document.body.appendChild(ref);
    return () => ref?.remove();
  });
</script>

<div bind:this={ref} style="display: contents">
  {@render children()}
</div>
