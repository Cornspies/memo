<script lang="ts">
  import { app, backend, errorText, filter, go, route, toast } from "../lib/state.svelte";
  import type { TagCount } from "../lib/types";
  import MemoList from "./MemoList.svelte";
  import MemoPane from "./MemoPane.svelte";
  import Settings from "./Settings.svelte";
  import SyncStatus from "./SyncStatus.svelte";

  let tags = $state<TagCount[]>([]);
  let menuOpen = $state(false);
  const detailOpen = $derived(route.memo !== null || route.settings);

  $effect(() => {
    void app.version;
    backend.tags().then((t) => (tags = t));
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
    filter.archived = archived;
    filter.tag = tag;
    menuOpen = false;
    if (route.settings) go("/");
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
      <button class="ghost nav" class:active={!filter.archived && !filter.tag} onclick={() => show(false, null)}>
        Memos
      </button>
      <button class="ghost nav" class:active={filter.archived} onclick={() => show(true, null)}>Archive</button>
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
    {#if filter.tag || filter.archived}
      <div class="filter row small-text">
        <span>{filter.archived ? "Archive" : `#${filter.tag}`}</span>
        <button class="ghost small" onclick={() => show(false, null)}>Clear</button>
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
