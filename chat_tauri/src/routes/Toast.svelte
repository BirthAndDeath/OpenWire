<script lang="ts">
  import { fly } from "svelte/transition";

  interface Props {
    message?: string;
    duration?: number;
  }

  let { message = "", duration = 5000 }: Props = $props();

  let visible = $state(false);
  let timeout: ReturnType<typeof setTimeout> | null = null;

  // 监听 message 变化来显示/隐藏 toast
  $effect(() => {
    if (message) {
      visible = true;
      if (timeout) {
        clearTimeout(timeout);
      }
      timeout = setTimeout(() => {
        visible = false;
        timeout = null;
      }, duration);
    } else {
      visible = false;
    }

    return () => {
      if (timeout) {
        clearTimeout(timeout);
        timeout = null;
      }
    };
  });
</script>

{#if visible}
  <div
    class="toast"
    transition:fly={{
      y: -20,
      duration: 300,
      easing: (t) => Math.sin((t * Math.PI) / 2),
    }}
  >
    {message}
  </div>
{/if}

<style>
  .toast {
    position: fixed;
    top: 50%;
    left: 50%;
    transform: translate(-50%, -50%);
    background: rgba(255, 68, 68, 0.95);
    color: white;
    padding: 12px 20px;
    border-radius: 40px;
    box-shadow: 0 4px 12px rgba(0, 0, 0, 0.15);
    z-index: 1000;
    font-weight: 500;
    font-size: 14px;
    text-align: center;
    max-width: 80vw;
    word-break: break-word;
    backdrop-filter: blur(4px);
    animation: fadeOut 4s ease forwards;
  }

  @keyframes fadeOut {
    0% {
      opacity: 0;
      transform: translate(-50%, -50%) scale(0.9);
    }
    15% {
      opacity: 1;
      transform: translate(-50%, -50%) scale(1);
    }
    85% {
      opacity: 1;
      transform: translate(-50%, -50%) scale(1);
    }
    100% {
      opacity: 0;
      transform: translate(-50%, -50%) scale(0.9);
      visibility: hidden;
    }
  }
</style>
