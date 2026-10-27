<script lang="ts">
  import { dateTime, size } from "../lib/format";
  import { backend, errorText } from "../lib/state.svelte";
  import type { MemoDetail, RevisionInfo } from "../lib/types";
  import Modal from "./Modal.svelte";
  import Rendered from "./Rendered.svelte";

  let { memo, onclose }: { memo: MemoDetail; onclose: () => void } = $props();
  let revisions = $state<RevisionInfo[] | null>(null);
  let selected = $state<RevisionInfo | null>(null);
  let body = $state<string | null>(null);
  let error = $state<string | null>(null);
  let busy = $state(false);

  $effect(() => {
    backend.revisions(memo.id).then(
      (list) => (revisions = list),
      (e) => (error = errorText(e)),
    );
  });

  function who(userId: string | null) {
    const member = memo.members.find((m) => m.user_id === userId);
    return member ? (member.trust === "me" ? "you" : member.username) : "a former member";
  }

  async function open(revision: RevisionInfo) {
    selected = revision;
    body = null;
    error = null;
    try {
      body = await backend.revision(memo.id, revision.version);
    } catch (e) {
      error = errorText(e, { decrypt: "This version cannot be decrypted on this device." });
    }
  }

  async function restore() {
    if (!selected) return;
    busy = true;
    try {
      await backend.restoreRevision(memo.id, selected.version);
      onclose();
    } catch (e) {
      error = errorText(e);
    } finally {
      busy = false;
    }
  }
</script>

<Modal title="History" {onclose}>
  {#if error}<p class="error">{error}</p>{/if}
  {#if selected}
    <div class="row head">
      <button class="ghost small" onclick={() => (selected = null)}>← All versions</button>
      <span class="muted small-text">Version {selected.version} · {dateTime(selected.updated_at)}</span>
    </div>
    {#if body !== null}
      <div class="version">
        <Rendered memoId={memo.id} {body} attachments={memo.attachments} />
      </div>
      {#if memo.writable && !memo.conflict && selected.version !== memo.server_version}
        <button class="primary" disabled={busy} onclick={restore}>Restore this version</button>
      {/if}
    {:else if !error}
      <p class="muted">Decrypting…</p>
    {/if}
  {:else if revisions === null}
    <p class="muted">Loading…</p>
  {:else}
    <ul>
      {#each revisions as revision (revision.version)}
        <li>
          <button class="ghost" onclick={() => open(revision)}>
            <span>{dateTime(revision.updated_at)}</span>
            <span class="muted small-text">
              by {who(revision.updated_by)} · {size(revision.size)}
              {revision.version === memo.server_version ? "· current" : ""}
            </span>
          </button>
        </li>
      {/each}
    </ul>
    <p class="muted small-text">Older versions are removed after a while, depending on the server's settings.</p>
  {/if}
</Modal>

<style>
  ul {
    list-style: none;
    padding: 0;
    margin: 0;
    max-height: 60vh;
    overflow-y: auto;
  }
  li button {
    width: 100%;
    display: flex;
    justify-content: space-between;
    gap: 8px;
    text-align: left;
    flex-wrap: wrap;
  }
  .head {
    margin-bottom: 10px;
  }
  .version {
    border: 1px solid var(--border);
    border-radius: var(--radius);
    padding: 12px;
    max-height: 55vh;
    overflow-y: auto;
    margin-bottom: 12px;
  }
</style>
