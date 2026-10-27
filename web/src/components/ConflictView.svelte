<script lang="ts">
  import { when } from "../lib/format";
  import { backend, errorText, toast } from "../lib/state.svelte";
  import type { MemoDetail } from "../lib/types";
  import Rendered from "./Rendered.svelte";

  let { memo }: { memo: MemoDetail } = $props();
  let merging = $state(false);
  let merged = $state("");
  let busy = $state(false);

  const theirs = $derived(memo.conflict_with!);

  function startMerge() {
    merged = `${memo.body.trimEnd()}\n\n---\n\n${theirs.body.trimStart()}`;
    merging = true;
  }

  async function resolve(body: string) {
    busy = true;
    try {
      await backend.resolve(memo.id, body);
    } catch (e) {
      toast("danger", errorText(e));
    } finally {
      busy = false;
    }
  }
</script>

<div class="conflict">
  <p class="notice warning">
    This memo was changed on this device and elsewhere at the same time, in the same place. Nothing is
    lost: choose which version to keep, or combine them.
  </p>
  {#if merging}
    <textarea class="merge mono" bind:value={merged}></textarea>
    <div class="row">
      <button class="primary" disabled={busy} onclick={() => resolve(merged)}>Save combined version</button>
      <button onclick={() => (merging = false)}>Back</button>
    </div>
  {:else}
    <div class="versions">
      <section>
        <h3>This device</h3>
        <div class="version"><Rendered memoId={memo.id} body={memo.body} attachments={memo.attachments} /></div>
        <button class="primary" disabled={busy} onclick={() => resolve(memo.body)}>Keep this version</button>
      </section>
      <section>
        <h3>Other version <span class="muted small-text">· {when(theirs.edited_at)}</span></h3>
        <div class="version"><Rendered memoId={memo.id} body={theirs.body} attachments={memo.attachments} /></div>
        <button class="primary" disabled={busy} onclick={() => resolve(theirs.body)}>Keep this version</button>
      </section>
    </div>
    <button onclick={startMerge}>Combine both by hand</button>
  {/if}
</div>

<style>
  .conflict {
    display: flex;
    flex-direction: column;
    gap: 12px;
  }
  .versions {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(260px, 1fr));
    gap: 16px;
  }
  section {
    display: flex;
    flex-direction: column;
    gap: 8px;
    align-items: flex-start;
  }
  h3 {
    font-size: 14px;
    margin: 0;
  }
  .version {
    border: 1px solid var(--border);
    border-radius: var(--radius);
    padding: 12px;
    width: 100%;
    max-height: 50vh;
    overflow-y: auto;
  }
  .merge {
    min-height: 320px;
    font-size: 14px;
  }
  .conflict > button {
    align-self: flex-start;
  }
</style>
