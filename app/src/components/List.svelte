<script lang="ts">
  import { clock, readable } from "../lib/api";
  import { echolot, resultText } from "../lib/echolot.svelte";
  import { session } from "../lib/session.svelte";
  import Panel from "./Panel.svelte";

  const l = $derived(echolot.list!);
  const folder = $derived(session.settings ? `${session.settings.out_dir}/Echolot` : undefined);
</script>

<Panel label="List">
  <div class="title">{l.detail}</div>
  <div class="bar"><span style:width="{l.total > 0 ? (l.done / l.total) * 100 : 0}%"></span></div>
  <div class="counts mono small">
    <span>{l.done}/{l.total}</span>
    <span>{!echolot.listing ? "done" : l.paused ? (l.current ? "pausing after this song" : "paused") : "recording"}</span>
  </div>
  {#if l.current}<div class="current small">♪ {l.current.artist} – {l.current.title} <span class="muted mono">{clock(l.current.duration_ms)}</span></div>{/if}
  {#if echolot.listing}
    <div class="buttons">
      {#if l.paused}
        <button class="btn primary" onclick={() => echolot.listControl("resume")} disabled={echolot.busy}>Resume</button>
      {:else}
        <button class="btn" onclick={() => echolot.listControl("pause")} disabled={echolot.busy}>Pause</button>
      {/if}
      {#if l.current}<button class="btn" onclick={() => echolot.listControl("skip")} disabled={echolot.busy}>Skip</button>{/if}
      <button class="btn danger" onclick={() => echolot.listControl("stop")} disabled={echolot.busy}>Stop list</button>
    </div>
  {/if}
  <p class="muted small" title={folder}>Recordings wait in the Echolot folder for Echolot's upload.</p>

  {#if l.lagging}<p class="warn small">Spotify reports what plays 6 s or more late. Consider a restart.</p>{/if}
  {#if l.error}<div class="error small">{l.error}</div>{/if}

  {#if echolot.listResults.length > 0}
    <ul class="results small">
      {#each echolot.listResults.slice(0, 8) as r (r.id)}
        <li title={readable(r.detail, true)}>
          <span class="tag mono" class:ok={r.result === "recorded"} class:bad={r.result === "failed" || r.result === "stalled" || r.result === "crossfade"}>{resultText[r.result] ?? r.result}</span>
          <span class="song">{r.job.artist} – {r.job.title}</span>
        </li>
      {/each}
    </ul>
  {/if}
</Panel>

<style>
  .title {
    font-weight: 600;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .bar {
    height: 3px;
    margin-top: 6px;
    background: var(--faint);
  }

  .bar span {
    display: block;
    height: 100%;
    background: var(--edge);
  }

  .counts {
    display: flex;
    gap: 10px;
    margin-top: 6px;
    color: var(--muted);
  }

  .small {
    font-size: 12px;
  }

  .current {
    margin-top: 6px;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .buttons {
    display: grid;
    grid-auto-columns: 1fr;
    grid-auto-flow: column;
    gap: 8px;
    margin-top: 10px;
  }

  p {
    margin: 10px 0 0;
  }

  .error {
    margin-top: 10px;
    color: var(--bad);
  }

  .results {
    list-style: none;
    margin: 14px 0 0;
    padding: 10px 0 0;
    border-top: 1px solid var(--faint);
    display: grid;
    gap: 4px;
  }

  .results li {
    display: grid;
    grid-template-columns: 82px minmax(0, 1fr);
    gap: 8px;
  }

  .tag {
    font-size: 10px;
    letter-spacing: 0.06em;
    text-transform: uppercase;
    color: var(--muted);
  }

  .tag.ok {
    color: var(--ok);
  }

  .tag.bad {
    color: var(--bad);
  }

  .song {
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
</style>
