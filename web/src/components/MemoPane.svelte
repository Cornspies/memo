<script lang="ts">
  import { untrack } from "svelte";
  import { dateTime, when } from "../lib/format";
  import { app, backend, errorCode, errorText, filter, go, toast } from "../lib/state.svelte";
  import type { AttachmentView, MemoDetail } from "../lib/types";
  import ConflictView from "./ConflictView.svelte";
  import HistoryDialog from "./HistoryDialog.svelte";
  import Lightbox from "./Lightbox.svelte";
  import Rendered from "./Rendered.svelte";
  import ShareDialog from "./ShareDialog.svelte";

  let { id, startEditing = false }: { id: string; startEditing?: boolean } = $props();

  const MAX_IMAGE = 25 * 1024 * 1024;
  const SAVE_DELAY = 400;

  let memo = $state<MemoDetail | null>(null);
  let missing = $state(false);
  // svelte-ignore state_referenced_locally
  let editing = $state(startEditing);
  /** The editor's text, and the body last saved (or loaded). */
  let text = $state("");
  let saved = "";
  let saveTimer: ReturnType<typeof setTimeout> | undefined;
  let textarea = $state<HTMLTextAreaElement | null>(null);
  let dialog = $state<"share" | "history" | null>(null);
  let viewing = $state<AttachmentView | null>(null);
  let attaching = $state(0);
  let fileInput = $state<HTMLInputElement>();

  const writable = $derived(memo !== null && memo.writable && !memo.conflict);
  const author = $derived.by(() => {
    if (!memo?.updated_by || memo.members.length < 2) return null;
    const member = memo.members.find((m) => m.user_id === memo!.updated_by);
    return member ? (member.trust === "me" ? "you" : member.username) : null;
  });
  const owner = $derived(memo?.members.find((m) => m.user_id === memo!.owner_id)?.username ?? null);

  $effect(() => {
    void app.version;
    untrack(() => void refresh());
  });

  // Leaving the memo: save, drop images the text no longer uses, and discard it if empty.
  $effect(() => () => {
    if (editing) void finish(false);
  });

  async function refresh() {
    if (editing) await save();
    let next: MemoDetail;
    try {
      next = await backend.memo(id);
    } catch (e) {
      if (errorCode(e) === "not_found") missing = true;
      else toast("danger", errorText(e));
      return;
    }
    // A sync may have merged another device's edit into the text being edited.
    if (!editing || next.body !== saved) adopt(next.body);
    memo = next;
    missing = false;
    if (next.conflict || !next.writable) editing = false;
  }

  function adopt(body: string) {
    const cursor = textarea?.selectionStart ?? null;
    text = saved = body;
    if (cursor !== null && textarea) {
      queueMicrotask(() => textarea?.setSelectionRange(cursor, cursor));
    }
  }

  function changed() {
    clearTimeout(saveTimer);
    saveTimer = setTimeout(() => void save(), SAVE_DELAY);
  }

  async function save(prune = false) {
    clearTimeout(saveTimer);
    if (text === saved && !prune) return;
    const body = text;
    try {
      await backend.edit(id, body, prune);
      saved = body;
    } catch (e) {
      if (errorCode(e) === "in_conflict") {
        editing = false;
        toast("warning", "This memo was changed elsewhere at the same time. Choose what to keep.");
      } else {
        toast("danger", errorText(e));
      }
    }
  }

  async function finish(stay = true) {
    editing = false;
    await save(true);
    if (memo?.own && text.trim() === "" && memo.server_version === 0 && memo.attachments.length === 0) {
      await backend.remove(id).catch(() => {});
      if (stay) go("/");
      return;
    }
    if (stay) await refresh();
  }

  function startEdit() {
    editing = true;
    queueMicrotask(() => textarea?.focus());
  }

  $effect(() => {
    if (editing && textarea && document.activeElement !== textarea && startEditing) textarea.focus();
  });

  function keydown(event: KeyboardEvent) {
    if ((event.ctrlKey || event.metaKey) && event.key === "s") {
      event.preventDefault();
      void save();
    } else if (event.key === "Escape") {
      void finish();
    }
  }

  function insert(snippet: string) {
    const area = textarea;
    const start = area?.selectionStart ?? text.length;
    const end = area?.selectionEnd ?? text.length;
    const before = text.slice(0, start);
    const prefix = before && !before.endsWith("\n") ? "\n" : "";
    text = before + prefix + snippet + text.slice(end);
    const cursor = start + prefix.length + snippet.length;
    queueMicrotask(() => area?.setSelectionRange(cursor, cursor));
  }

  async function attach(files: Iterable<File>) {
    for (const file of files) {
      if (!file.type.startsWith("image/")) continue;
      if (file.size > MAX_IMAGE) {
        toast("danger", `${file.name} is larger than 25 MB.`);
        continue;
      }
      attaching++;
      try {
        await save();
        const image = await backend.attachImage(id, file, null);
        insert(`![](attachment:${image.id})\n`);
        await save();
        memo = await backend.memo(id);
      } catch (e) {
        toast("danger", `${file.name}: ${errorText(e)}`);
      } finally {
        attaching--;
      }
    }
  }

  function paste(event: ClipboardEvent) {
    const files = [...(event.clipboardData?.files ?? [])].filter((f) => f.type.startsWith("image/"));
    if (files.length) {
      event.preventDefault();
      void attach(files);
    }
  }

  function drop(event: DragEvent) {
    const files = [...(event.dataTransfer?.files ?? [])];
    if (files.length) {
      event.preventDefault();
      void attach(files);
    }
  }

  async function act(action: () => Promise<unknown>) {
    try {
      await action();
    } catch (e) {
      toast("danger", errorText(e));
    }
  }

  async function remove() {
    if (!memo) return;
    const shared = memo.members.length > 1;
    const question = shared
      ? "Delete this memo for everyone it is shared with?"
      : "Delete this memo? It cannot be restored.";
    if (!confirm(question)) return;
    await act(async () => {
      await backend.remove(id);
      go("/");
    });
  }

  async function leave() {
    if (!confirm("Leave this shared memo? You will lose access to it.")) return;
    await act(async () => {
      await backend.leave(id);
      go("/");
    });
  }

  async function copy() {
    await act(async () => go(`/memo/${await backend.saveAsCopy(id)}`));
  }

  function showTag(tag: string) {
    filter.tag = tag;
    filter.archived = false;
    go("/");
  }
</script>

{#if missing}
  <div class="center muted">
    <div>
      <p>This memo does not exist (any more).</p>
      <button onclick={() => go("/")}>Back to the list</button>
    </div>
  </div>
{:else if memo}
  <div class="pane">
    <header class="toolbar">
      <button class="ghost back" aria-label="Back" onclick={() => go("/")}>←</button>
      {#if editing}
        <button class="primary" onclick={() => finish()}>Done</button>
        <button onclick={() => fileInput?.click()} disabled={attaching > 0}>
          {attaching ? "Adding image…" : "Add image"}
        </button>
      {:else if writable}
        <button class="primary" onclick={startEdit}>Edit</button>
      {/if}
      <span class="spacer"></span>
      {#if !editing}
        <button
          class="ghost"
          onclick={() => act(() => backend.setState(id, !memo!.pinned, memo!.archived))}
          title={memo.pinned ? "Unpin" : "Pin to the top"}>{memo.pinned ? "Unpin" : "Pin"}</button
        >
        <button
          class="ghost"
          onclick={() => act(() => backend.setState(id, false, !memo!.archived))}
          title={memo.archived ? "Move back to memos" : "Move to the archive"}
          >{memo.archived ? "Unarchive" : "Archive"}</button
        >
      {/if}
      <button class="ghost" onclick={() => (dialog = "share")}>{memo.own ? "Share" : "Members"}</button>
      <button class="ghost" onclick={() => (dialog = "history")} disabled={memo.server_version === 0}>
        History
      </button>
      {#if memo.own}
        <button class="ghost danger" onclick={remove}>Delete</button>
      {:else}
        <button class="ghost danger" onclick={leave}>Leave</button>
      {/if}
    </header>

    <div class="content">
      {#if memo.undecoded === "missing_key"}
        <p class="notice warning">The key for this memo has not arrived yet. It will open after the next sync.</p>
      {:else if memo.undecoded === "undecryptable"}
        <p class="notice danger">
          This memo cannot be decrypted. Its key may not come from its owner, or the data was damaged.
        </p>
      {:else if memo.undecoded === "newer_format"}
        <p class="notice warning">This memo was saved by a newer version of memo. It is read-only here.</p>
      {/if}
      {#if memo.push_error === "forbidden"}
        <div class="notice danger row">
          <span class="spacer">You can no longer edit this memo. Your changes are kept on this device.</span>
          <button onclick={copy}>Save as a new memo</button>
        </div>
      {:else if memo.push_error === "quota_exceeded"}
        <p class="notice danger">Your storage quota is full. This memo's changes are not on the server yet.</p>
      {:else if memo.push_error === "invalid"}
        <p class="notice danger">The server refused the last change. Edit the memo to try again.</p>
      {/if}

      {#if memo.conflict && memo.conflict_with}
        <ConflictView {memo} />
      {:else if editing}
        <textarea
          class="editor"
          bind:this={textarea}
          bind:value={text}
          oninput={changed}
          onkeydown={keydown}
          onpaste={paste}
          ondrop={drop}
          ondragover={(e) => e.preventDefault()}
          placeholder="Write in Markdown. #tags, - [ ] tasks, images by paste or drop."
          spellcheck="true"
        ></textarea>
      {:else}
        {#if memo.body.trim()}
          <Rendered
            memoId={id}
            body={memo.body}
            attachments={memo.attachments}
            writable={writable && !memo.undecoded}
            ontask={(offset) => act(() => backend.toggleTask(id, offset))}
            ontag={showTag}
            onimage={(a) => (viewing = a)}
          />
        {:else if !memo.undecoded}
          <p class="muted">Empty memo.</p>
        {/if}
      {/if}
    </div>

    <footer class="small-text muted">
      {#if memo.server_version === 0}
        Not on the server yet.
      {:else}
        <span title={dateTime(memo.updated_at)}>Changed {when(memo.updated_at)}{author ? ` by ${author}` : ""}</span>
      {/if}
      {#if !memo.own && owner}· Shared by {owner}{/if}
      {#if !memo.writable && !memo.own}· Read-only{/if}
      {#if memo.tags.length}·
        {#each memo.tags as tag}<button class="ghost small tag" onclick={() => showTag(tag)}>#{tag}</button>{/each}
      {/if}
    </footer>
  </div>

  <input
    type="file"
    accept="image/jpeg,image/png,image/webp,image/gif"
    multiple
    hidden
    bind:this={fileInput}
    onchange={(e) => {
      const input = e.currentTarget;
      void attach([...(input.files ?? [])]).then(() => (input.value = ""));
    }}
  />

  {#if dialog === "share"}
    <ShareDialog {memo} onclose={() => (dialog = null)} />
  {:else if dialog === "history"}
    <HistoryDialog {memo} onclose={() => (dialog = null)} />
  {/if}
  {#if viewing}
    <Lightbox memoId={id} attachment={viewing} onclose={() => (viewing = null)} />
  {/if}
{:else}
  <div class="center muted">Loading…</div>
{/if}

<style>
  .pane {
    display: flex;
    flex-direction: column;
    height: 100%;
  }
  .toolbar {
    display: flex;
    align-items: center;
    gap: 4px;
    padding: 10px 14px;
    border-bottom: 1px solid var(--border);
    flex-wrap: wrap;
  }
  .back {
    display: none;
  }
  .content {
    flex: 1;
    overflow-y: auto;
    padding: 24px clamp(16px, 4vw, 48px);
    display: flex;
    flex-direction: column;
    min-height: 0;
  }
  .content > :global(.markdown) {
    max-width: 760px;
  }
  .editor {
    flex: 1;
    min-height: 300px;
    resize: none;
    border: none;
    padding: 0;
    background: transparent;
    font-family: var(--mono);
    font-size: 14px;
    line-height: 1.6;
    max-width: 760px;
  }
  .editor:focus-visible {
    outline: none;
  }
  footer {
    padding: 8px 18px;
    border-top: 1px solid var(--border);
    display: flex;
    gap: 6px;
    flex-wrap: wrap;
    align-items: center;
  }
  .tag {
    color: var(--accent);
    padding: 0 4px;
  }
  @media (max-width: 700px) {
    .back {
      display: block;
    }
  }
</style>
