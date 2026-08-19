<script lang="ts">
  import type { Snippet } from "svelte";
  let {
    min = 200,
    max = 500,
    defaultSize = 300,
    size = $bindable(defaultSize),
    position = 'left' as 'left' | 'top' | 'bottom',
    mobileHidden = false,
    children,
  }: {
    min?: number;
    max?: number;
    defaultSize?: number;
    size?: number;
    position?: 'left' | 'top' | 'bottom';
    mobileHidden?: boolean;
    children?: Snippet;
  } = $props();

  let dragging = $state(false);

  function getClientXY(e: MouseEvent | TouchEvent) {
    if ('touches' in e) {
      return { x: e.touches[0].clientX, y: e.touches[0].clientY };
    }
    return { x: e.clientX, y: e.clientY };
  }

  function onDrag(e: MouseEvent | TouchEvent) {
    const { x, y } = getClientXY(e);
    const isHorizontal = position === 'left';
    const isBottom = position === 'bottom';
    const val = isHorizontal
      ? Math.max(min, Math.min(max, x))
      : Math.max(min, Math.min(max, isBottom ? window.innerHeight - y : y));
    size = val;
  }

  function onDown(e: MouseEvent | TouchEvent) {
    e.preventDefault();
    dragging = true;
    document.addEventListener('mousemove', onDrag);
    document.addEventListener('mouseup', onUp);
    document.addEventListener('touchmove', onDrag, { passive: false });
    document.addEventListener('touchend', onUp);
  }

  function onUp() {
    dragging = false;
    document.removeEventListener('mousemove', onDrag);
    document.removeEventListener('mouseup', onUp);
    document.removeEventListener('touchmove', onDrag);
    document.removeEventListener('touchend', onUp);
  }
</script>

{#if !mobileHidden}
  {#if position === 'left'}
    <div class="panel panel-h" style="--size: {size}px" class:dragging>
      {@render children?.()}
      <button
        class="resizer"
        style="right: calc(var(--resizer-size) / -2)"
        onmousedown={onDown}
        ontouchstart={onDown}
        aria-label="调整面板宽度"
        type="button"
      ></button>
    </div>
  {:else}
    <div class="panel panel-v" style="--size: {size}px" class:dragging>
      {@render children?.()}
      <button
        class="resizer"
        style="top: {position === 'bottom' ? '0' : 'calc(var(--size) - var(--resizer-size) / 2)'}"
        onmousedown={onDown}
        ontouchstart={onDown}
        aria-label="调整面板高度"
        type="button"
      ></button>
    </div>
  {/if}
{:else}
  {@render children?.()}
{/if}

<style>
  .panel {
    position: relative;
    flex-shrink: 0;
  }
  .panel-h {
    width: var(--size);
    max-width: 100%;
  }
  .panel-v {
    height: var(--size);
  }
  .resizer {
    position: absolute;
    top: 0;
    background: transparent;
    z-index: 10;
    transition: background 0.15s;
  }
  .panel-h .resizer {
    width: var(--resizer-size);
    height: 100%;
    cursor: col-resize;
  }
  .panel-v .resizer {
    height: var(--resizer-size);
    width: 100%;
    cursor: row-resize;
  }
  .resizer:hover,
  .dragging .resizer {
    background: #3b82f6;
  }
</style>