<script lang="ts">
  import { num } from "../lib/api";
  import { session } from "../lib/session.svelte";
</script>

<!-- What records: the app and its route while listening, the choice of apps before. -->
<div class="bar">
  {#if session.source}
    <span class="app">{session.source.app}</span>
    <span class="device muted" title={session.source.device}>{session.source.device}</span>
    <span class="tags mono">
      <span class:ok={session.source.rate === 44100} class:warn={session.source.rate !== 44100}>{num(session.source.rate / 1000)} kHz</span>
      {#if session.platform}<span>gain {session.platform.gain * 100} %</span>{/if}
      {#if session.platform?.monitor}<span>{session.settings?.monitor ? "monitoring" : "silent"}</span>{/if}
    </span>
  {:else}
    <span class="label mono muted">Source</span>
    <div class="options" role="radiogroup" aria-label="Source">
      {#each session.sources as s (s.id)}
        <button class="option" role="radio" aria-checked={session.selected === s.id} disabled={!s.running} onclick={() => (session.selected = s.id)} title={!s.running ? "not running" : s.title ? `${s.artist} – ${s.title}` : "ready"}>
          <span class="name">{s.name}</span>
          <span class="what muted">{!s.running ? "not running" : s.title ? `${s.artist} – ${s.title}` : "ready"}</span>
          {#if s.playing}<span class="live mono">live</span>{/if}
        </button>
      {/each}
    </div>
    {#if session.problems > 0}
      <button class="hint warn" onclick={() => (session.tab = "setup")}>
        {session.problems} setup issue{session.problems > 1 ? "s" : ""} – recordings won't be bit-exact. Open setup →
      </button>
    {/if}
  {/if}
</div>

<style>
  .bar {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 8px 14px;
    min-height: 44px;
    margin-bottom: 16px;
    padding: 6px 14px;
    border: 1px solid var(--line);
    background: var(--panel);
  }

  .app {
    font-weight: 600;
  }

  .device {
    min-width: 0;
    flex: 0 1 auto;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .tags {
    display: flex;
    gap: 6px;
    margin-left: auto;
    font-size: 11px;
  }

  .tags span {
    padding: 2px 8px;
    border: 1px solid var(--faint);
    color: var(--muted);
    white-space: nowrap;
  }

  .tags .ok {
    border-color: rgba(52, 211, 153, 0.5);
    color: var(--ok);
  }

  .tags .warn {
    border-color: rgba(251, 191, 36, 0.5);
    color: var(--warn);
  }

  .label {
    font-size: 11px;
    letter-spacing: 0.16em;
    text-transform: uppercase;
  }

  .options {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    min-width: 0;
  }

  .option {
    display: flex;
    align-items: baseline;
    gap: 10px;
    max-width: 420px;
    padding: 5px 12px;
    border: 1px solid var(--faint);
    background: transparent;
    text-align: left;
  }

  .option:disabled {
    opacity: 0.5;
    cursor: default;
  }

  .option[aria-checked="true"]:not(:disabled) {
    border-color: var(--line-strong);
    background: rgba(34, 211, 238, 0.06);
    box-shadow: inset 2px 0 0 var(--cyan);
  }

  .name {
    font-weight: 600;
  }

  .what {
    min-width: 0;
    font-size: 12px;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .live {
    font-size: 10px;
    letter-spacing: 0.12em;
    text-transform: uppercase;
    color: var(--ok);
  }

  .hint {
    margin-left: auto;
    padding: 5px 10px;
    font-size: 12px;
    border: 1px solid rgba(251, 191, 36, 0.4);
    background: rgba(251, 191, 36, 0.06);
  }
</style>
