<script lang="ts">
  import { onMount } from "svelte";
  import Panel from "../components/Panel.svelte";
  import TrackRow from "../components/TrackRow.svelte";
  import { api, type HistoryRecord } from "../lib/api";
  import { resultText } from "../lib/echolot.svelte";
  import { facts, fileCard, quality, when, type Quality } from "../lib/quality";
  import { session } from "../lib/session.svelte";

  type Row = { r: HistoryRecord; q: Quality; at: Date; origin: string; text: string };

  const PAGE = 50;
  const filters = [
    { tag: "", label: "All" },
    { tag: "exact", label: "Exact" },
    { tag: "fine", label: "Fine" },
    { tag: "lossy", label: "Lossy" },
    { tag: "alt", label: "Altered" },
    { tag: "bad", label: "Damaged" },
  ];

  let rows = $state<Row[] | null>(null);
  let search = $state("");
  let filter = $state("");
  let shown = $state(PAGE);

  function origin(r: HistoryRecord): string {
    if (!r.echolot) return "Manual";
    if (r.result) return `Echolot · ${resultText[r.result] ?? r.result}`;
    return r.verdict === "info" ? "Echolot · not uploaded" : "Echolot · damaged, not uploaded";
  }

  // Read again each time the tab opens: this run's songs are in it too.
  onMount(() => {
    void api
      .history()
      .then((records) => {
        rows = records.map((r) => ({ r, q: quality(r.verdict, r.master, r.exact, r.faded, r.lossy, r.file, r.issues ?? []), at: new Date(r.at), origin: origin(r), text: `${r.track.artist} ${r.track.title} ${r.track.album}`.toLowerCase() }));
      })
      .catch((e) => (session.error = String(e)));
  });

  const words = $derived(search.toLowerCase().split(/\s+/).filter(Boolean));
  const found = $derived((rows ?? []).filter((row) => words.every((w) => row.text.includes(w))));
  const matching = $derived(filter ? found.filter((row) => row.q.tag === filter) : found);
  const counts = $derived(Object.fromEntries(filters.map((f) => [f.tag, f.tag ? found.filter((row) => row.q.tag === f.tag).length : found.length])));
</script>

<Panel label="Library">
  {#snippet actions()}
    <button class="btn" onclick={() => session.settings && api.open(session.settings.out_dir)}>Open folder</button>
  {/snippet}

  <div class="tools">
    <input class="search" type="search" placeholder="Search artist, title or album" bind:value={search} oninput={() => (shown = PAGE)} aria-label="Search" />
    <div class="filters mono" role="radiogroup" aria-label="Quality">
      {#each filters as f}
        <button role="radio" aria-checked={filter === f.tag} class:on={filter === f.tag} disabled={f.tag !== "" && !counts[f.tag]} onclick={() => ((filter = f.tag), (shown = PAGE))}>
          {f.label}<span class="n">{counts[f.tag] ?? 0}</span>
        </button>
      {/each}
    </div>
  </div>

  {#if rows === null}
    <p class="muted empty">Loading…</p>
  {:else if rows.length === 0}
    <p class="muted empty">Every song Mitschnitt saves is listed here, with how it arrived.</p>
  {:else if matching.length === 0}
    <p class="muted empty">Nothing matches.</p>
  {:else}
    <ul>
      {#each matching.slice(0, shown) as { r, q, at, origin } (r.at + r.file)}
        <TrackRow track={r.track} {q} {at} whenText={when(at)} card={fileCard({ ...r, issues: r.issues ?? [] }, at, origin)} file={r.file} gone={!r.exists} note={origin} facts={facts(r.track.duration_ms, r.size)} />
      {/each}
    </ul>
    {#if matching.length > shown}
      <button class="btn more" onclick={() => (shown += PAGE)}>Show more · {matching.length - shown} left</button>
    {/if}
  {/if}
</Panel>

<style>
  .tools {
    display: flex;
    flex-wrap: wrap;
    gap: 10px 16px;
    align-items: center;
    margin-bottom: 12px;
    padding-bottom: 12px;
    border-bottom: 1px solid var(--faint);
  }

  .search {
    flex: 1 1 260px;
    padding: 7px 10px;
    border: 1px solid var(--line);
    background: var(--panel-2);
    outline: none;
    user-select: text;
  }

  .search:focus {
    border-color: var(--line-strong);
  }

  .filters {
    display: flex;
    border: 1px solid var(--line);
  }

  .filters button {
    padding: 6px 11px;
    border: none;
    background: transparent;
    font-size: 11px;
    letter-spacing: 0.1em;
    text-transform: uppercase;
    color: var(--muted);
  }

  .filters button + button {
    border-left: 1px solid var(--line);
  }

  .filters button.on {
    color: var(--text);
    background: rgba(34, 211, 238, 0.08);
    box-shadow: inset 0 -2px 0 var(--cyan);
  }

  .filters button:disabled {
    opacity: 0.4;
    cursor: default;
  }

  .n {
    margin-left: 6px;
    color: var(--muted);
  }

  .empty {
    margin: 0;
  }

  ul {
    list-style: none;
    margin: 0;
    padding: 0;
  }

  .more {
    width: 100%;
    margin-top: 8px;
  }
</style>
