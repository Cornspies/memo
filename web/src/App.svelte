<script lang="ts">
  import Auth from "./components/Auth.svelte";
  import Main from "./components/Main.svelte";
  import RecoveryKey from "./components/RecoveryKey.svelte";
  import Toasts from "./components/Toasts.svelte";
  import Unlock from "./components/Unlock.svelte";
  import { app, backend, loadSession } from "./lib/state.svelte";

  /** Set after register or recover, until the user confirms they saved it. */
  let recoveryKey = $state<string | null>(null);

  loadSession();

  async function confirmed() {
    await backend.finishOnboarding();
    recoveryKey = null;
  }
</script>

{#if recoveryKey}
  <RecoveryKey key={recoveryKey} onconfirm={confirmed} />
{:else if !app.session}
  <div class="center muted">Loading…</div>
{:else if app.session.state === "signed_out"}
  <Auth onrecoverykey={(key) => (recoveryKey = key)} />
{:else if app.session.state === "locked"}
  <Unlock username={app.session.username} />
{:else if app.session.state === "offline"}
  <div class="center">
    <div>
      <p>The server cannot be reached.</p>
      <button class="primary" onclick={loadSession}>Try again</button>
    </div>
  </div>
{:else}
  <Main />
{/if}
<Toasts />
