<script lang="ts">
  import { app, backend, errorText, toast } from "../lib/state.svelte";
  import type { MemberView, MemoDetail, Permission, Trust as TrustLevel, UserInfo } from "../lib/types";
  import Modal from "./Modal.svelte";
  import Trust from "./Trust.svelte";

  let { memo, onclose }: { memo: MemoDetail; onclose: () => void } = $props();

  let username = $state("");
  let found = $state<(UserInfo & { trust: TrustLevel; fingerprint: string }) | null>(null);
  let permission = $state<Permission>("read");
  let busy = $state(false);
  let error = $state<string | null>(null);
  let verifying = $state<MemberView | null>(null);

  const myFingerprint = $derived(app.session?.state === "unlocked" ? app.session.fingerprint : "");
  const isMember = $derived(found !== null && memo.members.some((m) => m.user_id === found!.user_id));

  async function run(action: () => Promise<void>) {
    busy = true;
    error = null;
    try {
      await action();
    } catch (e) {
      error = errorText(e);
    } finally {
      busy = false;
    }
  }

  async function find(event: SubmitEvent) {
    event.preventDefault();
    found = null;
    busy = true;
    error = null;
    try {
      found = await backend.lookupUser(username);
    } catch (e) {
      error = errorText(e, { not_found: "There is no user with this name." });
    } finally {
      busy = false;
    }
  }

  const share = () =>
    run(async () => {
      const user = found!;
      await backend.share(memo.id, { user_id: user.user_id, username: user.username, public_key: user.public_key }, permission);
      toast("info", `Shared with ${user.username}.`);
      found = null;
      username = "";
    });

  const revoke = (member: MemberView) => {
    if (!confirm(`Remove ${member.username}? They keep what they already saw, but not later changes.`)) return;
    return run(() => backend.revoke(memo.id, [member.user_id]));
  };
</script>

<Modal title={memo.own ? "Share" : "Members"} {onclose}>
  {#if error}<p class="error">{error}</p>{/if}

  {#if verifying}
    <p>
      Ask <strong>{verifying.username}</strong> to open <em>Settings</em> and read you their key fingerprint,
      in person or over a channel you trust. It must match:
    </p>
    <p class="fingerprint mono">{verifying.fingerprint}</p>
    <div class="row">
      <button
        class="primary"
        disabled={busy}
        onclick={() =>
          run(async () => {
            await backend.verifyContact(verifying!.user_id);
            verifying = null;
          })}>It matches</button
      >
      <button onclick={() => (verifying = null)}>Cancel</button>
    </div>
  {:else}
    <ul class="members">
      {#each memo.members as member (member.user_id)}
        <li>
          <div class="who">
            <strong>{member.username}</strong>
            {#if member.user_id === memo.owner_id}<span class="badge">Owner</span>{/if}
            <Trust trust={member.trust} />
          </div>
          <div class="actions">
            {#if member.trust === "pinned" || member.trust === "not_pinned"}
              <button class="ghost small" onclick={() => (verifying = member)}>Verify</button>
            {/if}
            {#if member.user_id === memo.owner_id}
              <span class="muted small-text">can edit</span>
            {:else if memo.own}
              <select
                value={member.permission}
                disabled={busy}
                onchange={(e) => {
                  const next = e.currentTarget.value as Permission;
                  run(() => backend.setPermission(memo.id, member.user_id, next));
                }}
              >
                <option value="read">can read</option>
                <option value="write">can edit</option>
              </select>
              <button class="ghost small danger" disabled={busy} onclick={() => revoke(member)}>Remove</button>
            {:else}
              <span class="muted small-text">{member.permission === "write" ? "can edit" : "can read"}</span>
            {/if}
          </div>
          {#if member.trust === "changed"}
            <p class="notice danger small-text">
              The server now presents a different key for {member.username} than the one you saw before. This
              can mean an attack. Memos are only encrypted to the key you saw first. Ask them whether they
              reset their account.
            </p>
          {/if}
        </li>
      {/each}
    </ul>

    {#if memo.own}
      <form class="add" onsubmit={find}>
        <label>
          <span>Share with</span>
          <div class="row">
            <input type="text" bind:value={username} placeholder="Username" required autocapitalize="none" spellcheck="false" />
            <button disabled={busy}>Find</button>
          </div>
        </label>
      </form>
      {#if found}
        <div class="found">
          <div class="who">
            <strong>{found.username}</strong>
            <Trust trust={found.trust} />
          </div>
          {#if found.trust === "me"}
            <p class="muted small-text">That is you.</p>
          {:else if isMember}
            <p class="muted small-text">Already a member.</p>
          {:else if found.trust === "changed"}
            <p class="notice danger small-text">
              The server presents a different key for {found.username} than the one you saw before. Sharing is
              blocked until you check with them.
            </p>
          {:else}
            <p class="small-text">
              Key fingerprint: <span class="mono">{found.fingerprint}</span>
              {#if found.trust === "not_pinned"}
                <br /><span class="muted">
                  First time you share with them: this key will be remembered. Compare the fingerprint with
                  them to be sure it is theirs.
                </span>
              {/if}
            </p>
            <div class="row">
              <select bind:value={permission}>
                <option value="read">can read</option>
                <option value="write">can edit</option>
              </select>
              <button class="primary" disabled={busy} onclick={share}>Share</button>
            </div>
          {/if}
        </div>
      {/if}
    {:else}
      <p class="muted small-text">Only the owner can add or remove members.</p>
    {/if}
    <p class="muted small-text mine">Your key fingerprint: <span class="mono">{myFingerprint}</span></p>
  {/if}
</Modal>

<style>
  .members {
    list-style: none;
    padding: 0;
    margin: 0 0 16px;
  }
  .members li {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    justify-content: space-between;
    gap: 6px;
    padding: 8px 0;
    border-bottom: 1px solid var(--border);
  }
  .members li .notice {
    flex-basis: 100%;
    margin: 4px 0 0;
  }
  .who,
  .actions {
    display: flex;
    align-items: center;
    gap: 6px;
  }
  .actions select {
    width: auto;
    padding: 3px 6px;
  }
  .found {
    margin-top: 12px;
    padding: 12px;
    border: 1px solid var(--border);
    border-radius: var(--radius);
  }
  .found select {
    width: auto;
  }
  .add input {
    flex: 1;
    width: auto;
  }
  .fingerprint {
    font-size: 18px;
    background: var(--surface-2);
    padding: 12px;
    border-radius: var(--radius);
    text-align: center;
  }
  .mine {
    margin: 16px 0 0;
  }
</style>
