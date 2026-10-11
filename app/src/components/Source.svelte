<script lang="ts">
  import { session } from "../lib/session.svelte";
  import Panel from "./Panel.svelte";
</script>

<Panel label="Source" index="01">
  {#snippet actions()}
    {#if !session.listening}<button class="btn" onclick={() => session.refresh()}>Refresh</button>{/if}
  {/snippet}

  {#if session.listening && session.source}
    <div class="active">
      <div class="name">{session.source.app}</div>
      <div class="muted">{session.source.device}</div>
      <div class="tags mono">
        <span class:ok={session.source.rate === 44100} class:warn={session.source.rate !== 44100}>{(session.source.rate / 1000).toFixed(1)} kHz</span>
        {#if session.platform}<span>gain {session.platform.gain * 100} %</span>{/if}
        {#if session.platform?.monitor}<span>{session.settings?.monitor ? "monitoring" : "silent"}</span>{/if}
      </div>
    </div>
    <div class="stops">
      <button class="btn wide" onclick={() => session.stopAfterTrack()} disabled={session.busy || session.stopping || session.status?.phase !== "recording"}>
        {session.stopping ? "Stopping after track…" : "Stop after track"}
      </button>
      <button class="btn danger wide" onclick={() => session.stop()} disabled={session.busy}>Stop</button>
    </div>
  {:else}
    <div class="list" role="radiogroup">
      {#each session.sources as s (s.id)}
        <button class="option" role="radio" aria-checked={session.selected === s.id} disabled={!s.running} onclick={() => (session.selected = s.id)}>
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
    <button class="btn primary wide" onclick={() => session.start()} disabled={!session.sources.find((s) => s.id === session.selected)?.running || session.busy}>
      {session.busy ? "Starting…" : "Start listening"}
    </button>
  {/if}
</Panel>

<style>
  .list {
    display: grid;
    gap: 6px;
    margin-bottom: 14px;
  }

  .option {
    display: grid;
    grid-template-columns: minmax(0, 1fr) auto;
    text-align: left;
    padding: 9px 12px;
    border: 1px solid var(--faint);
    background: transparent;
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

  .name,
  .what {
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .name {
    font-weight: 600;
  }

  .what {
    grid-column: 1;
    font-size: 12px;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .live {
    grid-row: 1 / span 2;
    grid-column: 2;
    align-self: center;
    font-size: 10px;
    letter-spacing: 0.12em;
    text-transform: uppercase;
    color: var(--ok);
  }

  .active {
    display: grid;
    gap: 2px;
    margin-bottom: 16px;
  }

  .active .name {
    font-size: 18px;
  }

  .tags {
    display: flex;
    gap: 6px;
    margin-top: 8px;
    font-size: 11px;
  }

  .tags span {
    padding: 1px 7px;
    border: 1px solid var(--faint);
    color: var(--muted);
  }

  .tags .ok {
    border-color: rgba(52, 211, 153, 0.5);
    color: var(--ok);
  }

  .tags .warn {
    border-color: rgba(251, 191, 36, 0.5);
    color: var(--warn);
  }

  .hint {
    display: block;
    width: 100%;
    margin-bottom: 12px;
    padding: 8px 10px;
    text-align: left;
    font-size: 12px;
    border: 1px solid rgba(251, 191, 36, 0.4);
    background: rgba(251, 191, 36, 0.06);
  }

  .wide {
    width: 100%;
  }

  .stops {
    display: grid;
    grid-template-columns: 1fr auto;
    gap: 8px;
  }
</style>
