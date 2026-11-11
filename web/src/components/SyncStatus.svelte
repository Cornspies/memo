<script lang="ts">
  import { when } from "../lib/format";
  import { app, backend } from "../lib/state.svelte";

  let tick = $state(Date.now());
  $effect(() => {
    const timer = setInterval(() => (tick = Date.now()), 30_000);
    return () => clearInterval(timer);
  });

  const text = $derived.by(() => {
    const s = app.sync;
    if (s.syncing) return "Syncing…";
    // The server refuses writes from a page opened at another address than its public_url.
    if (s.error === "origin not allowed") return "Not synced: open memo at the server's public address";
    if (s.error) return "Not synced: retrying";
    if (s.unsynced) return "Changes not synced yet";
    if (s.last) return `Synced ${when(s.last, tick)}`;
    return "Not synced yet";
  });
</script>

<div class="status small-text" class:error={app.sync.error} title={app.sync.error ?? ""}>
  <span class="dot" class:busy={app.sync.syncing} class:pending={app.sync.unsynced}></span>
  <span class="text">{text}</span>
  <button class="ghost small" disabled={app.sync.syncing} onclick={() => backend.sync()}>Sync</button>
</div>

<style>
  .status {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 4px 4px 4px 12px;
    color: var(--muted);
  }
  .status.error {
    color: var(--danger);
    background: none;
    margin: 0;
  }
  .text {
    flex: 1;
  }
  .dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--accent);
    flex: none;
  }
  .dot.pending {
    background: var(--warning);
  }
  .status.error .dot {
    background: var(--danger);
  }
  .dot.busy {
    animation: pulse 1s ease-in-out infinite alternate;
  }
  @keyframes pulse {
    from {
      opacity: 1;
    }
    to {
      opacity: 0.3;
    }
  }
</style>
