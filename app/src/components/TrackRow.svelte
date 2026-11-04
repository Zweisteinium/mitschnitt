<script lang="ts">
  import { api, type Track } from "../lib/api";
  import { full, type Card, type Quality } from "../lib/quality";
  import Cover from "./Cover.svelte";

  /** `file`: actions for it; `gone`: it was there once. `note`: after the verdict, in grey. */
  let { track, q, at, whenText, card, cover = null, file = null, gone = false, note = null, facts = null }: { track: Track; q: Quality; at: Date; whenText: string; card: Card; cover?: string | null; file?: string | null; gone?: boolean; note?: string | null; facts?: string | null } = $props();
</script>

<li>
  <Cover src={cover} file={gone ? null : file} size={40} />
  <span class="tag mono {q.tone}" title={q.text}>{q.tag}</span>
  <div class="main">
    <div class="name">{track.artist} – {track.title}</div>
    <div class="detail">
      <span class={q.tone}>{q.text}</span>
      {#if note}<span class="muted">{note}</span>{/if}
      {#if facts}<span class="facts mono">{facts}</span>{/if}
      <span class="info" tabindex="0" role="button" aria-label="Details">
        i
        <span class="card" role="tooltip">
          {#if card.findings.length}
            <span class="findings">{#each card.findings as line}<span>{line}</span>{/each}</span>
          {/if}
          <span class="facts-list">
            {#each card.facts as [label, value]}<span class="label mono">{label}</span><span>{value}</span>{/each}
          </span>
        </span>
      </span>
    </div>
  </div>
  <span class="when mono muted" title={full(at)}>{whenText}</span>
  {#if gone}
    <span class="gone mono muted" title="Moved or deleted: {file}">gone</span>
  {:else if file}
    <span class="actions">
      <button class="icon" onclick={() => api.open(file)} title="Show in folder: {file}" aria-label="Show in folder">
        <svg viewBox="0 0 16 16"><path d="M1.5 3.5h4.5l1.5 1.5h7v8h-13z" /></svg>
      </button>
      <button class="icon" onclick={() => api.play(file)} title="Play" aria-label="Play">
        <svg viewBox="0 0 16 16"><path d="M4.5 2.5v11l9-5.5z" /></svg>
      </button>
    </span>
  {:else}
    <span></span>
  {/if}
</li>

<style>
  li {
    display: grid;
    grid-template-columns: 40px 46px minmax(0, 1fr) auto 44px;
    gap: 12px;
    align-items: center;
    padding: 9px 0;
  }

  li + :global(li) {
    border-top: 1px solid var(--faint);
  }

  .tag {
    padding: 1px 0;
    text-align: center;
    font-size: 10px;
    letter-spacing: 0.1em;
    text-transform: uppercase;
    border: 1px solid currentColor;
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
    align-items: center;
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
    right: -8px;
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
    gap: 8px;
  }

  .findings {
    display: grid;
    gap: 2px;
    padding-bottom: 8px;
    border-bottom: 1px solid var(--faint);
  }

  .facts-list {
    display: grid;
    grid-template-columns: auto minmax(0, 1fr);
    gap: 2px 12px;
    overflow-wrap: anywhere;
  }

  .label {
    font-size: 10px;
    line-height: 17px;
    letter-spacing: 0.08em;
    text-transform: uppercase;
    color: var(--muted);
  }

  .when {
    font-size: 11px;
  }

  .gone {
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
</style>
