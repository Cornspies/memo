<script lang="ts">
  import { app, dismiss, go } from "../lib/state.svelte";
</script>

<div class="toasts" aria-live="polite">
  {#each app.toasts as toast (toast.id)}
    <div class="toast {toast.kind}">
      <span>{toast.text}</span>
      {#if toast.memoId}
        <button
          class="small"
          onclick={() => {
            go(`/memo/${toast.memoId}`);
            dismiss(toast.id);
          }}>Open</button
        >
      {/if}
      <button class="ghost small" aria-label="Dismiss" onclick={() => dismiss(toast.id)}>✕</button>
    </div>
  {/each}
</div>

<style>
  .toasts {
    position: fixed;
    right: 16px;
    bottom: 16px;
    display: flex;
    flex-direction: column;
    gap: 8px;
    z-index: 20;
    max-width: min(420px, calc(100vw - 32px));
  }
  .toast {
    display: flex;
    align-items: center;
    gap: 8px;
    background: var(--surface);
    border: 1px solid var(--border);
    border-left: 4px solid var(--accent);
    border-radius: var(--radius);
    padding: 10px 10px 10px 14px;
    box-shadow: var(--shadow);
  }
  .toast span {
    flex: 1;
  }
  .toast.warning {
    border-left-color: var(--warning);
  }
  .toast.danger {
    border-left-color: var(--danger);
  }
</style>
