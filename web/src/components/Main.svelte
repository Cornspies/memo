<script lang="ts">
  import {
    QUICK_FILTERS,
    app,
    applySaved,
    backend,
    clearFilter,
    errorText,
    filter,
    go,
    isSaved,
    listFilter,
    route,
    toast,
  } from "../lib/state.svelte";
  import type { Has, SavedFilter, TagCount } from "../lib/types";
  import Calendar from "./Calendar.svelte";
  import MemoList from "./MemoList.svelte";
  import MemoPane from "./MemoPane.svelte";
  import Modal from "./Modal.svelte";
  import Settings from "./Settings.svelte";
  import SyncStatus from "./SyncStatus.svelte";

  let tags = $state<TagCount[]>([]);
  let saved = $state<SavedFilter[]>([]);
  let menuOpen = $state(false);
  let saving = $state<string | null>(null);
  const detailOpen = $derived(route.memo !== null || route.settings);
  const filtered = $derived(
    filter.archived || filter.tag !== null || filter.has.length > 0 || filter.day !== null || filter.query.trim() !== "",
  );
  const describe = $derived(
    [
      filter.archived ? "Archive" : null,
      filter.query.trim() ? `“${filter.query.trim()}”` : null,
      filter.tag ? `#${filter.tag}` : null,
      filter.day !== null ? new Date(filter.day).toLocaleDateString([], { dateStyle: "medium" }) : null,
      ...QUICK_FILTERS.filter(([has]) => filter.has.includes(has)).map(([, name]) => name.toLowerCase()),
    ]
      .filter(Boolean)
      .join(" · "),
  );
  const savable = $derived(
    !filter.archived &&
      filter.day === null &&
      (filter.query.trim() !== "" || filter.tag !== null || filter.has.length > 0) &&
      !saved.some(isSaved),
  );

  $effect(() => {
    void app.version;
    backend.tags().then((t) => (tags = t));
    backend.savedFilters().then((f) => (saved = f));
  });

  async function newMemo() {
    try {
      const id = await backend.create("");
      filter.archived = false;
      newId = id;
      go(`/memo/${id}`);
    } catch (e) {
      toast("danger", errorText(e));
    }
  }

  /** The memo just created opens in the editor. */
  let newId = $state<string | null>(null);

  function show(archived: boolean, tag: string | null) {
    clearFilter(archived);
    filter.tag = tag;
    menuOpen = false;
    if (route.settings) go("/");
  }

  function pickSaved(item: SavedFilter) {
    applySaved(item);
    menuOpen = false;
    if (route.settings) go("/");
  }

  function toggle(has: Has) {
    filter.has = filter.has.includes(has) ? filter.has.filter((h) => h !== has) : [...filter.has, has];
  }

  function clear() {
    clearFilter();
    filter.query = "";
  }

  function startSave() {
    saving = filter.tag ? `#${filter.tag}` : filter.query.trim() || "";
  }

  async function save(event: SubmitEvent) {
    event.preventDefault();
    if (!saving?.trim()) return;
    try {
      await backend.saveFilter(saving.trim(), listFilter());
      saving = null;
    } catch (e) {
      toast("danger", errorText(e));
    }
  }

  async function removeSaved(item: SavedFilter) {
    if (!confirm(`Delete the saved filter "${item.name}"?`)) return;
    try {
      await backend.deleteFilter(item.id);
    } catch (e) {
      toast("danger", errorText(e));
    }
  }
</script>

<div class="layout" class:detail-open={detailOpen}>
  <aside class="sidebar" class:open={menuOpen}>
    <div class="brand">
      <img src="/favicon.svg" alt="" width="24" height="24" />
      <span>memo</span>
    </div>
    <button class="primary new" onclick={newMemo}>New memo</button>
    <nav>
      <button
        class="ghost nav"
        class:active={!filter.archived && !filter.tag && !filter.has.length && filter.day === null}
        onclick={() => show(false, null)}
      >
        Memos
      </button>
      <button class="ghost nav" class:active={filter.archived} onclick={() => show(true, null)}>Archive</button>
      <div class="heading">Calendar</div>
      <Calendar onpick={() => (menuOpen = false)} />
      {#if saved.length}
        <div class="heading">Saved</div>
        {#each saved as item (item.id)}
          <div class="saved">
            <button class="ghost nav" class:active={isSaved(item)} onclick={() => pickSaved(item)}>
              <span>{item.name}</span>
            </button>
            <button class="ghost small remove" aria-label={`Delete "${item.name}"`} onclick={() => removeSaved(item)}
              >✕</button
            >
          </div>
        {/each}
      {/if}
      {#if tags.length}
        <div class="heading">Tags</div>
        {#each tags as { tag, count } (tag)}
          <button class="ghost nav tag" class:active={filter.tag === tag} onclick={() => show(false, tag)}>
            <span>#{tag}</span><span class="muted">{count}</span>
          </button>
        {/each}
      {/if}
    </nav>
    <div class="bottom">
      <SyncStatus />
      <button class="ghost nav" class:active={route.settings} onclick={() => ((menuOpen = false), go("/settings"))}>
        Settings
      </button>
    </div>
  </aside>
  {#if menuOpen}
    <button class="scrim" aria-label="Close menu" onclick={() => (menuOpen = false)}></button>
  {/if}

  <section class="list">
    <header>
      <button class="ghost menu" aria-label="Menu" onclick={() => (menuOpen = true)}>☰</button>
      <input type="search" placeholder="Search" bind:value={filter.query} aria-label="Search memos" />
      <button class="primary compact-new" aria-label="New memo" onclick={newMemo}>+</button>
    </header>
    <div class="quick" role="group" aria-label="Quick filters">
      {#each QUICK_FILTERS as [has, name] (has)}
        <button class="chip" class:on={filter.has.includes(has)} aria-pressed={filter.has.includes(has)} onclick={() => toggle(has)}
          >{name}</button
        >
      {/each}
    </div>
    {#if filtered}
      <div class="filter row small-text">
        <span class="spacer">{describe}</span>
        {#if savable}<button class="ghost small" onclick={startSave}>Save</button>{/if}
        <button class="ghost small" onclick={clear}>Clear</button>
      </div>
    {/if}
    <MemoList selected={route.memo} />
  </section>

  <main class="detail">
    {#if route.settings}
      <Settings />
    {:else if route.memo}
      {#key route.memo}
        <MemoPane id={route.memo} startEditing={route.memo === newId} />
      {/key}
    {:else}
      <div class="center muted">Select a memo, or create one.</div>
    {/if}
  </main>
</div>

{#if saving !== null}
  <Modal title="Save filter" onclose={() => (saving = null)}>
    <form onsubmit={save}>
      <p class="muted small-text">Shows up in the sidebar, on all your devices. {describe}</p>
      <label class="field">
        <span>Name</span>
        <!-- svelte-ignore a11y_autofocus -->
        <input type="text" bind:value={saving} required autofocus maxlength="80" />
      </label>
      <button class="primary">Save</button>
    </form>
  </Modal>
{/if}

<style>
  .layout {
    display: grid;
    grid-template-columns: 220px 340px 1fr;
    height: 100%;
  }
  .sidebar {
    display: flex;
    flex-direction: column;
    gap: 12px;
    padding: 14px 10px;
    border-right: 1px solid var(--border);
    background: var(--bg);
    overflow-y: auto;
    min-height: 0;
  }
  .brand {
    display: flex;
    align-items: center;
    gap: 8px;
    font-weight: 700;
    font-size: 17px;
    padding: 0 6px;
  }
  .new {
    margin: 0 4px;
  }
  nav {
    display: flex;
    flex-direction: column;
    gap: 1px;
    flex: 1;
  }
  .nav {
    text-align: left;
    width: 100%;
    display: flex;
    justify-content: space-between;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .nav.active {
    background: var(--accent-soft);
    color: var(--accent);
    font-weight: 600;
  }
  .tag span:first-child {
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .saved {
    display: flex;
    align-items: center;
  }
  .saved .nav {
    flex: 1;
    min-width: 0;
  }
  .saved .remove {
    visibility: hidden;
    color: var(--muted);
  }
  .saved:hover .remove,
  .saved .remove:focus-visible {
    visibility: visible;
  }
  @media (hover: none) {
    .saved .remove {
      visibility: visible;
    }
  }
  .quick {
    display: flex;
    flex-wrap: wrap;
    gap: 4px;
    padding: 8px 12px;
    border-bottom: 1px solid var(--border);
  }
  .chip {
    font-size: 12px;
    padding: 2px 10px;
    border-radius: 99px;
  }
  .chip.on {
    background: var(--accent-soft);
    border-color: var(--accent);
    color: var(--accent);
    font-weight: 600;
  }
  .heading {
    font-size: 12px;
    font-weight: 700;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    color: var(--muted);
    margin: 14px 12px 4px;
  }
  .bottom {
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .list {
    display: flex;
    flex-direction: column;
    border-right: 1px solid var(--border);
    background: var(--surface);
    min-height: 0;
  }
  .list header {
    display: flex;
    gap: 6px;
    padding: 12px;
    border-bottom: 1px solid var(--border);
  }
  .menu,
  .compact-new,
  .scrim {
    display: none;
  }
  .filter {
    padding: 6px 14px;
    border-bottom: 1px solid var(--border);
    background: var(--surface-2);
    justify-content: space-between;
  }
  .detail {
    min-width: 0;
    min-height: 0;
    background: var(--surface);
  }

  @media (max-width: 1100px) {
    .layout {
      grid-template-columns: 300px 1fr;
    }
    .sidebar {
      position: fixed;
      inset: 0 auto 0 0;
      width: 250px;
      z-index: 10;
      translate: -100% 0;
      visibility: hidden;
      transition:
        translate 0.15s ease-out,
        visibility 0.15s;
    }
    .sidebar.open {
      translate: 0 0;
      visibility: visible;
      box-shadow: var(--shadow);
    }
    .scrim {
      display: block;
      position: fixed;
      inset: 0;
      z-index: 9;
      background: #0005;
      border: none;
      border-radius: 0;
    }
    .menu,
    .compact-new {
      display: block;
    }
  }

  @media (max-width: 700px) {
    .layout {
      grid-template-columns: 1fr;
    }
    .layout.detail-open .list,
    .layout:not(.detail-open) .detail {
      display: none;
    }
  }
</style>
