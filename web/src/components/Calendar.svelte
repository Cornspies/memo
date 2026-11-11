<script lang="ts">
  import { dayStart } from "../lib/format";
  import { app, backend, filter, go, route } from "../lib/state.svelte";

  let { onpick }: { onpick?: () => void } = $props();

  /** Memos created per local day (`dayStart`). */
  let counts = $state(new Map<number, number>());
  const today = dayStart(Date.now());
  let month = $state(new Date(new Date(today).getFullYear(), new Date(today).getMonth(), 1));

  $effect(() => {
    void app.version;
    backend.activity().then((times) => {
      const next = new Map<number, number>();
      for (const time of times) {
        const day = dayStart(time);
        next.set(day, (next.get(day) ?? 0) + 1);
      }
      counts = next;
    });
  });

  /** Weeks of the shown month, Monday first; `null` pads the first and last week. */
  const weeks = $derived.by(() => {
    const first = month.getDay();
    const days: (number | null)[] = Array((first + 6) % 7).fill(null);
    const length = new Date(month.getFullYear(), month.getMonth() + 1, 0).getDate();
    for (let d = 1; d <= length; d++) days.push(new Date(month.getFullYear(), month.getMonth(), d).getTime());
    while (days.length % 7) days.push(null);
    return Array.from({ length: days.length / 7 }, (_, i) => days.slice(i * 7, i * 7 + 7));
  });
  const label = $derived(month.toLocaleDateString([], { month: "long", year: "numeric" }));
  const weekdays = Array.from({ length: 7 }, (_, i) =>
    new Date(2024, 0, 1 + i).toLocaleDateString([], { weekday: "narrow" }),
  );

  function shift(by: number) {
    month = new Date(month.getFullYear(), month.getMonth() + by, 1);
  }

  function pick(day: number) {
    filter.day = filter.day === day ? null : day;
    filter.archived = false;
    if (route.settings) go("/");
    onpick?.();
  }

  function level(count: number) {
    return count === 0 ? 0 : count === 1 ? 1 : count <= 3 ? 2 : 3;
  }
</script>

<div class="calendar">
  <div class="head">
    <button class="ghost small" aria-label="Previous month" onclick={() => shift(-1)}>‹</button>
    <span>{label}</span>
    <button class="ghost small" aria-label="Next month" onclick={() => shift(1)}>›</button>
  </div>
  <div class="grid" role="grid" aria-label={label}>
    {#each weekdays as weekday, i (i)}<span class="weekday muted">{weekday}</span>{/each}
    {#each weeks as week, w (w)}
      {#each week as day, d (d)}
        {#if day === null}
          <span></span>
        {:else}
          {@const count = counts.get(day) ?? 0}
          <button
            class="day level-{level(count)}"
            class:today={day === today}
            class:selected={filter.day === day}
            disabled={count === 0 && filter.day !== day}
            title={`${new Date(day).toLocaleDateString([], { dateStyle: "medium" })}: ${count} ${count === 1 ? "memo" : "memos"}`}
            onclick={() => pick(day)}>{new Date(day).getDate()}</button
          >
        {/if}
      {/each}
    {/each}
  </div>
</div>

<style>
  .calendar {
    padding: 0 6px;
  }
  .head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    font-size: 13px;
    font-weight: 600;
    margin-bottom: 4px;
  }
  .grid {
    display: grid;
    grid-template-columns: repeat(7, 1fr);
    gap: 2px;
    font-size: 11px;
    text-align: center;
  }
  .weekday {
    padding: 2px 0;
  }
  .day {
    padding: 3px 0;
    border: 1px solid transparent;
    border-radius: 5px;
    background: transparent;
    font-size: 11px;
  }
  .day:disabled {
    opacity: 0.45;
    cursor: default;
  }
  .level-1 {
    background: color-mix(in srgb, var(--accent) 22%, transparent);
  }
  .level-2 {
    background: color-mix(in srgb, var(--accent) 45%, transparent);
  }
  .level-3 {
    background: color-mix(in srgb, var(--accent) 70%, transparent);
    color: var(--accent-text);
  }
  .day.today {
    border-color: var(--accent);
  }
  .day.selected {
    background: var(--accent);
    color: var(--accent-text);
    font-weight: 700;
  }
</style>
