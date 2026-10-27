<script lang="ts">
  import type { Snippet } from "svelte";

  let { title, onclose, children }: { title: string; onclose: () => void; children: Snippet } = $props();
  let dialog: HTMLDialogElement;

  $effect(() => {
    dialog.showModal();
  });
</script>

<dialog bind:this={dialog} {onclose}>
  <header>
    <h2>{title}</h2>
    <button class="ghost" aria-label="Close" onclick={() => dialog.close()}>✕</button>
  </header>
  <div class="body">{@render children()}</div>
</dialog>

<style>
  header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 14px 16px 6px 20px;
  }
  h2 {
    font-size: 17px;
    margin: 0;
  }
  .body {
    padding: 8px 20px 20px;
  }
</style>
