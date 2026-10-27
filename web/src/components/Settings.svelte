<script lang="ts">
  import { dateTime, download, size, when } from "../lib/format";
  import { app, backend, errorText, go, toast } from "../lib/state.svelte";
  import type { Device, Me } from "../lib/types";

  let me = $state<Me | null>(null);
  let devices = $state<Device[] | null>(null);
  let error = $state<string | null>(null);

  let current = $state("");
  let next = $state("");
  let repeat = $state("");
  let revokeOthers = $state(true);
  let passwordBusy = $state(false);
  let passwordError = $state<string | null>(null);

  let deletePassword = $state("");
  let deleteName = $state("");
  let deleteBusy = $state(false);
  let deleteError = $state<string | null>(null);
  let exporting = $state(false);

  const fingerprint = $derived(app.session?.state === "unlocked" ? app.session.fingerprint : "");

  async function load() {
    try {
      [me, devices] = await Promise.all([backend.me(), backend.devices()]);
    } catch (e) {
      error = errorText(e);
    }
  }
  load();

  async function removeDevice(device: Device) {
    if (!confirm(`Sign out "${device.name}"? Unsynced changes on it are lost.`)) return;
    try {
      await backend.removeDevice(device.id);
      await load();
    } catch (e) {
      toast("danger", errorText(e));
    }
  }

  async function changePassword(event: SubmitEvent) {
    event.preventDefault();
    passwordError = null;
    const problem = await backend.passwordProblem(next);
    if (problem) return (passwordError = problem);
    if (next !== repeat) return (passwordError = "The new passwords do not match.");
    passwordBusy = true;
    try {
      await backend.changePassword(current, next, revokeOthers);
      current = next = repeat = "";
      toast("info", "Password changed.");
      await load();
    } catch (e) {
      passwordError = errorText(e, { invalid_credentials: "The current password is wrong." });
    } finally {
      passwordBusy = false;
    }
  }

  async function exportAll() {
    exporting = true;
    try {
      const zip = await backend.exportAll();
      download(zip, `memo-export-${new Date().toISOString().slice(0, 10)}.zip`, "application/zip");
    } catch (e) {
      toast("danger", errorText(e));
    } finally {
      exporting = false;
    }
  }

  async function deleteAccount(event: SubmitEvent) {
    event.preventDefault();
    deleteError = null;
    if (deleteName.trim().toLowerCase() !== me?.username) return (deleteError = "Type your username to confirm.");
    deleteBusy = true;
    try {
      await backend.deleteAccount(deletePassword);
      go("/");
    } catch (e) {
      deleteError = errorText(e, { invalid_credentials: "Wrong password." });
      deleteBusy = false;
    }
  }

  const kinds = { web: "Browser", desktop: "Desktop", android: "Android" };
</script>

<div class="settings">
  <header class="row">
    <button class="ghost back" aria-label="Back" onclick={() => go("/")}>←</button>
    <h2>Settings</h2>
  </header>
  {#if error}<p class="error">{error}</p>{/if}

  {#if me}
    <section>
      <h3>Account</h3>
      <p>
        Signed in as <strong>{me.username}</strong>{me.role === "admin" ? " (admin)" : ""}, since
        {dateTime(me.created_at)}.
      </p>
      <div class="usage">
        <div class="bar"><div class="fill" style:width="{Math.min(100, (me.used_bytes / me.quota_bytes) * 100)}%"></div></div>
        <span class="small-text muted">{size(me.used_bytes)} of {size(me.quota_bytes)} used</span>
      </div>
    </section>
  {/if}

  <section>
    <h3>Your key fingerprint</h3>
    <p class="muted small-text">
      People you share with can compare this with what their app shows for you, to be sure nobody is in
      between.
    </p>
    <p class="fingerprint mono">{fingerprint}</p>
  </section>

  <section>
    <h3>Devices</h3>
    {#if devices}
      <ul class="devices">
        {#each devices as device (device.id)}
          <li>
            <div>
              <strong>{device.name}</strong>
              <span class="muted small-text">· {kinds[device.kind]}</span>
              {#if device.current}<span class="badge accent">This device</span>{/if}
              <div class="muted small-text">Last active {when(device.last_seen_at)}</div>
            </div>
            {#if !device.current}
              <button class="small danger" onclick={() => removeDevice(device)}>Sign out</button>
            {/if}
          </li>
        {/each}
      </ul>
    {:else}
      <p class="muted">Loading…</p>
    {/if}
  </section>

  <section>
    <h3>Change password</h3>
    <form onsubmit={changePassword}>
      {#if passwordError}<p class="error">{passwordError}</p>{/if}
      <input type="text" hidden value={me?.username ?? ""} autocomplete="username" readonly />
      <label class="field">
        <span>Current password</span>
        <input type="password" bind:value={current} required autocomplete="current-password" />
      </label>
      <label class="field">
        <span>New password</span>
        <input type="password" bind:value={next} required autocomplete="new-password" />
      </label>
      <label class="field">
        <span>Repeat the new password</span>
        <input type="password" bind:value={repeat} required autocomplete="new-password" />
      </label>
      <label class="check field">
        <input type="checkbox" bind:checked={revokeOthers} />
        Sign out all other devices
      </label>
      <button class="primary" disabled={passwordBusy}>{passwordBusy ? "Changing…" : "Change password"}</button>
    </form>
  </section>

  <section>
    <h3>Export</h3>
    <p class="muted small-text">All your memos as Markdown files with their images, in a ZIP file.</p>
    <button onclick={exportAll} disabled={exporting}>{exporting ? "Exporting…" : "Export memos"}</button>
  </section>

  <section>
    <h3>Session</h3>
    <div class="row">
      <button onclick={() => backend.lock()}>Lock</button>
      <button onclick={() => backend.logout()}>Sign out</button>
    </div>
    {#if app.sync.unsynced}
      <p class="notice warning small-text">Some changes are not synced yet. They are lost if you lock or sign out now.</p>
    {/if}
  </section>

  <section class="danger-zone">
    <h3>Delete account</h3>
    <p class="small-text">
      Deletes your account, every memo you own (also for the people you shared them with) and all your
      images. This cannot be undone.
    </p>
    <form onsubmit={deleteAccount}>
      {#if deleteError}<p class="error">{deleteError}</p>{/if}
      <label class="field">
        <span>Your username</span>
        <input type="text" bind:value={deleteName} autocomplete="off" />
      </label>
      <label class="field">
        <span>Password</span>
        <input type="password" bind:value={deletePassword} required autocomplete="current-password" />
      </label>
      <button class="danger solid" disabled={deleteBusy}>{deleteBusy ? "Deleting…" : "Delete my account"}</button>
    </form>
  </section>
</div>

<style>
  .settings {
    height: 100%;
    overflow-y: auto;
    padding: 16px clamp(16px, 4vw, 48px) 48px;
  }
  .settings > * {
    max-width: 620px;
  }
  header h2 {
    margin: 0;
    font-size: 20px;
  }
  .back {
    display: none;
  }
  section {
    padding: 18px 0;
    border-bottom: 1px solid var(--border);
  }
  h3 {
    font-size: 15px;
    margin: 0 0 10px;
  }
  .usage {
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .bar {
    height: 6px;
    background: var(--surface-2);
    border-radius: 3px;
    overflow: hidden;
  }
  .fill {
    height: 100%;
    background: var(--accent);
  }
  .fingerprint {
    font-size: 17px;
    background: var(--surface-2);
    padding: 10px 14px;
    border-radius: var(--radius);
    display: inline-block;
  }
  .devices {
    list-style: none;
    padding: 0;
    margin: 0;
  }
  .devices li {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
    padding: 8px 0;
  }
  .check {
    display: flex;
    gap: 8px;
    align-items: center;
  }
  .danger-zone {
    border-bottom: none;
  }
  @media (max-width: 700px) {
    .back {
      display: block;
    }
  }
</style>
