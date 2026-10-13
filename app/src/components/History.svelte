<script lang="ts">
  import { api, clock, megabytes, type HistoryRecord, type Master, type Severity } from "../lib/api";
  import { resultText } from "../lib/echolot.svelte";
  import { session, type Entry } from "../lib/session.svelte";
  import Panel from "./Panel.svelte";

  type Tone = "ok" | "fine" | "warn" | "bad" | "muted";

  const time = (d: Date) => d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
  const full = (d: Date) => d.toLocaleString([], { dateStyle: "medium", timeStyle: "short" });
  const when = (d: Date) => (d.toDateString() === new Date().toDateString() ? time(d) : d.toLocaleDateString([], { day: "numeric", month: "short" }));

  /** OK: fine, but not proven to the last sample (the player's fade-in, or no proof). */
  function quality(verdict: Severity, master: Master, exact: boolean, faded: boolean): { tone: Tone; tag: string; text: string } {
    if (verdict === "corrupt") return { tone: "bad", tag: "bad", text: "Corrupt" };
    if (faded) return { tone: "fine", tag: "ok", text: "Player fade-in at the start · rest bit-exact" };
    if (verdict === "altered") return { tone: "warn", tag: "alt", text: "Not fully original" };
    const bits = master === "bit16_exact" ? "16-bit master" : "24-bit master";
    return exact ? { tone: "ok", tag: "exact", text: `Bit-exact · ${bits}` } : { tone: "fine", tag: "ok", text: `Clean · ${bits}` };
  }

  function summary(e: Entry): { tone: Tone; tag: string; text: string } {
    if (e.outcome === "exists") return { tone: "muted", tag: "have", text: "Already recorded" };
    if (e.outcome !== "saved") return e.outcome === "dropped" ? { tone: "muted", tag: "skip", text: e.message } : { tone: "bad", tag: "fail", text: e.message };
    const q = quality(e.verdict, e.master, e.exact, e.faded);
    return e.file === null ? { ...q, text: `${q.text} · removed` } : q;
  }

  const facts = (durationMs: number, size: number) => `${clock(durationMs)} · ${megabytes(size)}`;

  function origin(r: HistoryRecord): string {
    if (!r.echolot) return "Manual";
    if (r.result) return `Echolot · ${resultText[r.result] ?? r.result}`;
    return r.verdict === "info" ? "Echolot · not uploaded" : "Echolot · not uploaded (damaged)";
  }

  const PAGE = 50;
  let open = $state(false);
  let records = $state<HistoryRecord[] | null>(null);
  let shown = $state(PAGE);

  /** This run's files are in the list above. */
  async function load() {
    try {
      records = (await api.history()).filter((r) => r.at < session.startedAt);
    } catch (e) {
      session.error = String(e);
    }
  }

  $effect(() => {
    if (open && records === null) void load();
  });
</script>

<Panel label="Session" index="03">
  {#snippet actions()}
    <button class="btn" onclick={() => session.settings && api.open(session.settings.out_dir)}>Open folder</button>
  {/snippet}

  {#if session.history.length === 0}
    <p class="muted empty">Recorded and skipped tracks appear here.</p>
  {:else}
    <ul class="rows">
      {#each session.history as e (e.id)}
        {@const s = summary(e)}
        <li>
          <span class="tag mono {s.tone}">{s.tag}</span>
          <div class="main">
            <div class="name">{e.track.artist} – {e.track.title}</div>
            <div class="detail {s.tone}">
              {s.text}{#if e.outcome === "saved" && e.echolot}{" · for Echolot"}{/if}
              {#if e.outcome === "saved" && e.file}<span class="facts mono">{facts(e.track.duration_ms, e.size)}</span>{/if}
            </div>
            {#if e.outcome === "saved" && e.issues.length}
              <ul class="issues">
                {#each e.issues as issue}<li>{issue}</li>{/each}
              </ul>
            {/if}
          </div>
          <span class="when mono muted" title={full(e.at)}>{time(e.at)}</span>
          {#if (e.outcome === "saved" || e.outcome === "exists") && e.file}
            <button class="link mono" onclick={() => api.open(e.file!)} title={e.file}>show</button>
          {:else}
            <span></span>
          {/if}
        </li>
      {/each}
    </ul>
  {/if}

  <details class="history" bind:open>
    <summary class="mono">History{#if records} · {records.length}{/if}</summary>
    {#if records === null}
      <p class="muted empty">Loading…</p>
    {:else if records.length === 0}
      <p class="muted empty">Files from earlier runs are listed here.</p>
    {:else}
      <ul class="rows">
        {#each records.slice(0, shown) as r (r.at + r.file)}
          {@const q = quality(r.verdict, r.master, r.exact, r.faded)}
          {@const at = new Date(r.at)}
          <li>
            <span class="tag mono {q.tone}" title={q.text}>{q.tag}</span>
            <div class="main">
              <div class="name">{r.track.artist} – {r.track.title}</div>
              <div class="detail muted">
                {origin(r)}<span class="facts mono">{facts(r.track.duration_ms, r.size)}</span>
              </div>
            </div>
            <span class="when mono muted" title={full(at)}>{when(at)}</span>
            {#if r.exists}
              <button class="link mono" onclick={() => api.open(r.file)} title={r.file}>show</button>
            {:else}
              <span class="gone mono muted" title="Moved or deleted: {r.file}">gone</span>
            {/if}
          </li>
        {/each}
      </ul>
      {#if records.length > shown}
        <button class="btn more" onclick={() => (shown += PAGE)}>Show more</button>
      {/if}
    {/if}
  </details>
</Panel>

<style>
  .empty {
    margin: 0;
  }

  ul {
    list-style: none;
    margin: 0;
    padding: 0;
  }

  .rows > li {
    display: grid;
    grid-template-columns: 46px 1fr auto 34px;
    gap: 12px;
    align-items: start;
    padding: 10px 0;
  }

  .rows > li + li {
    border-top: 1px solid var(--faint);
  }

  .tag {
    margin-top: 2px;
    padding: 1px 0;
    text-align: center;
    font-size: 10px;
    letter-spacing: 0.1em;
    text-transform: uppercase;
    border: 1px solid currentColor;
  }

  .fine {
    color: color-mix(in srgb, var(--ok) 55%, var(--muted));
  }

  .main {
    min-width: 0;
  }

  .name {
    font-weight: 600;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .detail {
    font-size: 12px;
  }

  .facts {
    margin-left: 10px;
    font-size: 11px;
    color: var(--muted);
    white-space: nowrap;
  }

  .issues li {
    font-size: 12px;
    color: var(--muted);
  }

  .when {
    margin-top: 2px;
    font-size: 11px;
  }

  .link,
  .gone {
    margin-top: 2px;
    font-size: 11px;
    text-transform: uppercase;
    letter-spacing: 0.1em;
    text-align: right;
  }

  .link {
    border: none;
    background: none;
    padding: 0;
    color: var(--cyan);
  }

  .history {
    margin-top: 12px;
    padding-top: 10px;
    border-top: 1px solid var(--line);
  }

  .history summary {
    cursor: pointer;
    font-size: 11px;
    letter-spacing: 0.1em;
    text-transform: uppercase;
    color: var(--muted);
  }

  .history[open] summary {
    margin-bottom: 6px;
  }

  .more {
    width: 100%;
    margin-top: 8px;
  }
</style>
