<script lang="ts">
  import { api, clock, megabytes, type HistoryRecord, type Lossy, type Master, type Severity } from "../lib/api";
  import { resultText } from "../lib/echolot.svelte";
  import { session, type Entry } from "../lib/session.svelte";
  import Panel from "./Panel.svelte";

  type Tone = "ok" | "lossy" | "warn" | "bad" | "muted";

  const time = (d: Date) => d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
  const full = (d: Date) => d.toLocaleString([], { dateStyle: "medium", timeStyle: "short" });
  const when = (d: Date) => (d.toDateString() === new Date().toDateString() ? time(d) : d.toLocaleDateString([], { day: "numeric", month: "short" }));

  /** A finding's gist for the row: up to its details ("end not verified: no pause …" -> "End not verified"). */
  function headline(issue: string | undefined): string {
    const gist = (issue ?? "").split(/: | \(/)[0];
    return gist.charAt(0).toUpperCase() + gist.slice(1);
  }

  const bitsOf = (master: Master) => (master === "bit16_exact" ? "16-bit master" : "24-bit master");

  /** OK: fine, but not proven to the last sample (the player's fade-in, or no proof). */
  function quality(verdict: Severity, master: Master, exact: boolean, faded: boolean, lossy: Lossy | null, file: string | null, issues: string[]): { tone: Tone; tag: string; text: string } {
    if (verdict === "corrupt") return { tone: "bad", tag: "bad", text: issues.length ? `Damaged · ${headline(issues[0])}` : "Damaged" };
    if (lossy) {
      const text = lossy === "stream" ? "Lossy stream" : "Lossy master";
      return { tone: "lossy", tag: "lossy", text: file?.endsWith(".mp3") ? `${text} · MP3` : text };
    }
    if (faded) return { tone: "ok", tag: "fine", text: "Fade-in, rest bit-exact" };
    if (verdict === "altered") return { tone: "warn", tag: "alt", text: issues.length ? headline(issues[0]) : "Not fully original" };
    return exact ? { tone: "ok", tag: "exact", text: `Bit-exact · ${bitsOf(master)}` } : { tone: "ok", tag: "fine", text: `Clean · ${bitsOf(master)}` };
  }

  function summary(e: Entry): { tone: Tone; tag: string; text: string } {
    if (e.outcome === "exists") return { tone: "muted", tag: "have", text: "Already have it" };
    if (e.outcome !== "saved") return e.outcome === "dropped" ? { tone: "muted", tag: "skip", text: headline(e.message) } : { tone: "bad", tag: "fail", text: headline(e.message) };
    const q = quality(e.verdict, e.master, e.exact, e.faded, e.lossy, e.file, e.issues);
    return e.file === null ? { ...q, text: `${q.text} · removed` } : q;
  }

  const facts = (durationMs: number, size: number) => `${clock(durationMs)} · ${megabytes(size)}`;

  /** Everything about a row, for its info card. */
  function details(e: Entry): string[] {
    if (e.outcome === "exists") return [e.file];
    if (e.outcome !== "saved") return [e.message];
    return [...e.issues, bitsOf(e.master), ...(e.file ? [e.file] : [])];
  }

  function origin(r: HistoryRecord): string {
    if (!r.echolot) return "Manual";
    if (r.result) return `Echolot · ${resultText[r.result] ?? r.result}`;
    return r.verdict === "info" ? "Echolot · not uploaded" : "Echolot · damaged, not uploaded";
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

{#snippet info(lines: string[])}
  {#if lines.length}
    <span class="info" tabindex="0" role="button" aria-label="Details">
      i
      <span class="card" role="tooltip">
        {#each lines as line}<span class="line">{line}</span>{/each}
      </span>
    </span>
  {/if}
{/snippet}

{#snippet fileActions(file: string)}
  <span class="actions">
    <button class="icon" onclick={() => api.open(file)} title="Show in folder: {file}" aria-label="Show in folder">
      <svg viewBox="0 0 16 16"><path d="M1.5 3.5h4.5l1.5 1.5h7v8h-13z" /></svg>
    </button>
    <button class="icon" onclick={() => api.play(file)} title="Play" aria-label="Play">
      <svg viewBox="0 0 16 16"><path d="M4.5 2.5v11l9-5.5z" /></svg>
    </button>
  </span>
{/snippet}

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
              {@render info(details(e))}
            </div>
          </div>
          <span class="when mono muted" title={full(e.at)}>{time(e.at)}</span>
          {#if (e.outcome === "saved" || e.outcome === "exists") && e.file}
            {@render fileActions(e.file)}
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
          {@const q = quality(r.verdict, r.master, r.exact, r.faded, r.lossy, r.file, r.issues ?? [])}
          {@const at = new Date(r.at)}
          <li>
            <span class="tag mono {q.tone}" title={q.text}>{q.tag}</span>
            <div class="main">
              <div class="name">{r.track.artist} – {r.track.title}</div>
              <div class="detail muted">
                {origin(r)}<span class="facts mono">{facts(r.track.duration_ms, r.size)}</span>
                {@render info([...(r.issues ?? []), bitsOf(r.master), r.file])}
              </div>
            </div>
            <span class="when mono muted" title={full(at)}>{when(at)}</span>
            {#if r.exists}
              {@render fileActions(r.file)}
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
    grid-template-columns: 46px 1fr auto 44px;
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
    display: flex;
    flex-wrap: wrap;
    column-gap: 10px;
    font-size: 12px;
  }

  .facts {
    font-size: 11px;
    color: var(--muted);
    white-space: nowrap;
  }

  .info {
    position: relative;
    display: inline-grid;
    place-items: center;
    width: 14px;
    height: 14px;
    border: 1px solid var(--muted);
    border-radius: 50%;
    color: var(--muted);
    font: italic 600 9px/1 serif;
    cursor: help;
  }

  .info:hover,
  .info:focus-visible {
    color: var(--text);
    border-color: var(--text);
    outline: none;
  }

  .card {
    display: none;
    position: absolute;
    z-index: 10;
    top: 18px;
    left: -8px;
    width: max-content;
    max-width: min(420px, 70vw);
    padding: 8px 10px;
    background: var(--panel);
    border: 1px solid var(--line-strong);
    box-shadow: 0 4px 16px rgb(0 0 0 / 0.25);
    color: var(--text);
    font: 12px/1.45 var(--font);
    font-style: normal;
    font-weight: 400;
    cursor: text;
    user-select: text;
  }

  .info:hover .card,
  .info:focus-within .card {
    display: grid;
    gap: 2px;
  }

  .line {
    overflow-wrap: anywhere;
  }

  .when {
    margin-top: 2px;
    font-size: 11px;
  }

  .gone {
    margin-top: 2px;
    font-size: 11px;
    text-transform: uppercase;
    letter-spacing: 0.1em;
    text-align: right;
  }

  .actions {
    display: flex;
    justify-content: flex-end;
    gap: 4px;
  }

  .icon {
    display: grid;
    place-items: center;
    width: 20px;
    height: 20px;
    padding: 0;
    border: none;
    background: none;
    color: var(--cyan);
    opacity: 0.75;
  }

  .icon:hover {
    opacity: 1;
  }

  .icon svg {
    width: 14px;
    height: 14px;
    fill: none;
    stroke: currentColor;
    stroke-width: 1.4;
    stroke-linejoin: round;
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
