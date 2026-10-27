<script lang="ts">
  import type { Snippet } from "svelte";

  let { title, children }: { title: string; children: Snippet } = $props();
</script>

<div class="page">
  <div class="card">
    <div class="brand">
      <img src="/favicon.svg" alt="" width="28" height="28" />
      <span>memo</span>
    </div>
    <h1>{title}</h1>
    {@render children()}
  </div>
</div>

<style>
  .page {
    min-height: 100%;
    display: grid;
    place-items: center;
    padding: 24px 12px;
  }
  .card {
    width: min(420px, 100%);
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: 14px;
    padding: 28px;
  }
  .brand {
    display: flex;
    align-items: center;
    gap: 8px;
    font-weight: 700;
    font-size: 18px;
    margin-bottom: 18px;
  }
  h1 {
    font-size: 20px;
    margin: 0 0 18px;
  }
</style>
