<script lang="ts">
  import { dayLabel, dayStart, when } from "../lib/format";
  import { app, backend, filter, go, listFilter } from "../lib/state.svelte";
  import type { MemoSummary } from "../lib/types";

  let { selected }: { selected: string | null } = $props();
  let memos = $state<MemoSummary[] | null>(null);

  $effect(() => {
    void app.version;
    const current = listFilter();
    backend.list(current).then((list) => (memos = list));
  });

  /** The timeline: pinned memos, then one group per day of the last change. */
  const groups = $derived.by(() => {
    const groups: { label: string; memos: MemoSummary[] }[] = [];
    let key: number | "pinned" | null = null;
    for (const memo of memos ?? []) {
      const next = memo.pinned ? "pinned" : dayStart(memo.updated_at);
      if (next !== key) {
        groups.push({ label: next === "pinned" ? "Pinned" : dayLabel(memo.updated_at), memos: [] });
        key = next;
      }
      groups[groups.length - 1].memos.push(memo);
    }
    return groups;
  });

  function problem(memo: MemoSummary): [string, string] | null {
    if (memo.conflict) return ["warning", "Conflict"];
    if (memo.undecoded === "newer_format") return ["warning", "Newer format"];
    if (memo.undecoded) return ["danger", "Cannot decrypt"];
    if (memo.push_error) return ["danger", "Not saved"];
    return null;
  }
</script>

<div class="items">
  {#if memos === null}
    <p class="empty muted">Loading…</p>
  {:else if memos.length === 0}
    <p class="empty muted">
      {filter.query || filter.tag || filter.has.length || filter.day !== null
        ? "Nothing matches."
        : filter.archived
          ? "The archive is empty."
          : "No memos yet."}
    </p>
  {:else}
    {#each groups as group (group.label)}
      <h3 class="group">{group.label}</h3>
      {#each group.memos as memo (memo.id)}
        {@const issue = problem(memo)}
        <button class="item" class:selected={memo.id === selected} onclick={() => go(`/memo/${memo.id}`)}>
          <div class="top">
            <span class="title" class:untitled={!memo.title}>{memo.title || "Untitled"}</span>
            <span class="date muted">{when(memo.updated_at)}</span>
          </div>
          {#if memo.preview}<div class="preview muted">{memo.preview}</div>{/if}
          <div class="meta">
            {#if memo.pinned}<span class="badge accent">Pinned</span>{/if}
            {#if memo.shared}<span class="badge">Shared</span>{/if}
            {#if !memo.writable && !memo.undecoded}<span class="badge">Read-only</span>{/if}
            {#if memo.images}<span class="badge">{memo.images} {memo.images === 1 ? "image" : "images"}</span>{/if}
            {#if issue}<span class="badge {issue[0]}">{issue[1]}</span>{/if}
            {#if memo.dirty && !issue}<span class="badge warning" title="Not synced yet">●</span>{/if}
          </div>
        </button>
      {/each}
    {/each}
  {/if}
</div>

<style>
  .items {
    overflow-y: auto;
    flex: 1;
    min-height: 0;
  }
  .empty {
    padding: 24px;
    text-align: center;
  }
  .group {
    position: sticky;
    top: 0;
    z-index: 1;
    margin: 0;
    padding: 6px 14px 4px;
    font-size: 12px;
    font-weight: 700;
    color: var(--muted);
    background: var(--surface-2);
    border-bottom: 1px solid var(--border);
  }
  .item {
    display: block;
    width: 100%;
    text-align: left;
    border: none;
    border-bottom: 1px solid var(--border);
    border-radius: 0;
    padding: 10px 14px;
    background: var(--surface);
    white-space: normal;
  }
  .item.selected {
    background: var(--accent-soft);
  }
  .top {
    display: flex;
    gap: 8px;
    align-items: baseline;
  }
  .title {
    flex: 1;
    font-weight: 600;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .untitled {
    color: var(--muted);
    font-style: italic;
  }
  .date {
    font-size: 12px;
    flex: none;
  }
  .preview {
    font-size: 13px;
    display: -webkit-box;
    -webkit-line-clamp: 2;
    line-clamp: 2;
    -webkit-box-orient: vertical;
    overflow: hidden;
  }
  .meta {
    display: flex;
    gap: 4px;
    flex-wrap: wrap;
    margin-top: 4px;
  }
  .meta:empty {
    display: none;
  }
</style>
