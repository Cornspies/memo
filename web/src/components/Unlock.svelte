<script lang="ts">
  import { backend, errorText } from "../lib/state.svelte";
  import Card from "./Card.svelte";

  let { username }: { username: string } = $props();
  let password = $state("");
  let busy = $state(false);
  let error = $state<string | null>(null);

  async function submit(event: SubmitEvent) {
    event.preventDefault();
    busy = true;
    error = null;
    try {
      await backend.unlock(password);
    } catch (e) {
      error = errorText(e, { invalid_credentials: "Wrong password." });
    } finally {
      busy = false;
    }
  }
</script>

<Card title="Unlock">
  <p class="muted">
    Signed in as <strong>{username}</strong>. Your keys are kept in memory only, so enter your password
    to decrypt your memos.
  </p>
  <form onsubmit={submit}>
    {#if error}<p class="error">{error}</p>{/if}
    <input type="text" hidden value={username} autocomplete="username" readonly />
    <label class="field">
      <span>Password</span>
      <!-- svelte-ignore a11y_autofocus -->
      <input type="password" bind:value={password} required autocomplete="current-password" autofocus />
    </label>
    <div class="row">
      <button class="primary" disabled={busy}>{busy ? "Unlocking…" : "Unlock"}</button>
      <span class="spacer"></span>
      <button type="button" class="ghost" onclick={() => backend.logout()}>Sign out</button>
    </div>
  </form>
</Card>
