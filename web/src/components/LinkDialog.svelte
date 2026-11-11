<script lang="ts">
  import { backend } from "../lib/state.svelte";
  import type { MemoSummary } from "../lib/types";
  import Modal from "./Modal.svelte";

  let {
    exclude,
    onpick,
    onclose,
  }: { exclude: string; onpick: (memo: MemoSummary) => void; onclose: () => void } = $props();

  let query = $state("");
  let results = $state<MemoSummary[]>([]);

  $effect(() => {
    const current = query;
    Promise.all([backend.list({ query: current }), backend.list({ query: current, archived: true })]).then(
      ([memos, archived]) => {
        if (current === query) results = [...memos, ...archived].filter((m) => m.id !== exclude).slice(0, 50);
      },
    );
  });
</script>

<Modal title="Link to a memo" {onclose}>
  <!-- svelte-ignore a11y_autofocus -->
  <input type="search" placeholder="Search" bind:value={query} autofocus aria-label="Search memos" />
  <ul class="results">
    {#each results as memo (memo.id)}
      <li>
        <button class="ghost" onclick={() => onpick(memo)}>
          <span class:muted={!memo.title}>{memo.title || "Untitled"}</span>
          {#if memo.archived}<span class="badge">Archived</span>{/if}
        </button>
      </li>
    {:else}
      <li class="muted small-text">Nothing matches.</li>
    {/each}
  </ul>
</Modal>

<style>
  .results {
    list-style: none;
    margin: 10px 0 0;
    padding: 0;
    max-height: 50vh;
    overflow-y: auto;
    min-width: min(420px, 80vw);
  }
  .results button {
    width: 100%;
    text-align: left;
    display: flex;
    gap: 8px;
    justify-content: space-between;
    overflow: hidden;
    text-overflow: ellipsis;
  }
</style>
