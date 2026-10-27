<script lang="ts">
  import { download } from "../lib/format";
  import { errorText } from "../lib/state.svelte";
  import Card from "./Card.svelte";

  let { key, onconfirm }: { key: string; onconfirm: () => Promise<void> } = $props();
  let typed = $state("");
  let copied = $state(false);
  let busy = $state(false);
  let error = $state<string | null>(null);

  const groups = $derived(key.split("-"));
  const last = $derived(groups[groups.length - 1]);
  const matches = $derived(typed.trim().toUpperCase() === last);

  async function copy() {
    await navigator.clipboard.writeText(key);
    copied = true;
  }

  async function submit(event: SubmitEvent) {
    event.preventDefault();
    busy = true;
    try {
      await onconfirm();
    } catch (e) {
      error = errorText(e);
      busy = false;
    }
  }
</script>

<Card title="Save your recovery key">
  <p>
    If you forget your password, this key is the <strong>only</strong> way back into your account. Nobody
    else can reset it, not even the server's admin. Write it down or store it in a password manager.
  </p>
  <div class="key mono">
    {#each groups as group}<span>{group}</span>{/each}
  </div>
  <div class="row">
    <button type="button" onclick={copy}>{copied ? "Copied" : "Copy"}</button>
    <button type="button" onclick={() => download(key + "\n", "memo-recovery-key.txt", "text/plain")}>
      Download
    </button>
  </div>
  <form onsubmit={submit}>
    {#if error}<p class="error">{error}</p>{/if}
    <label class="field confirm">
      <span>To confirm, type the last group ({groups.length}th)</span>
      <input type="text" class="mono" bind:value={typed} autocomplete="off" spellcheck="false" />
    </label>
    <button class="primary" disabled={!matches || busy}>{busy ? "Starting…" : "I saved it, continue"}</button>
  </form>
</Card>

<style>
  .key {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(56px, 1fr));
    gap: 6px;
    background: var(--surface-2);
    border-radius: var(--radius);
    padding: 12px;
    margin: 16px 0 10px;
    font-size: 15px;
    letter-spacing: 0.04em;
    user-select: all;
  }
  .confirm {
    margin-top: 20px;
  }
</style>
