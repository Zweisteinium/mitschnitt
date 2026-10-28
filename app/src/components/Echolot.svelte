<script lang="ts">
  import { clock, readable } from "../lib/api";
  import { echolot, resultText } from "../lib/echolot.svelte";
  import { session as listener } from "../lib/session.svelte";
  import Panel from "./Panel.svelte";

  const s = $derived(echolot.snapshot);
  const session = $derived(s.session);
  /** Pause and Stop take effect after the current song. */
  const busy = $derived(s.current !== null);
  /** Counts the time left down live. */
  const recorded = $derived(s.current && listener.status?.phase === "recording" ? listener.status.recorded_ms : 0);
  const doneMs = $derived(session ? Math.min(session.seconds_total * 1000, session.seconds_done * 1000 + recorded) : 0);
  const leftMs = $derived(session ? session.seconds_total * 1000 - doneMs : 0);
  /** By songs from an Echolot that doesn't send the time. */
  const progress = $derived(!session ? 0 : session.seconds_total > 0 ? doneMs / (session.seconds_total * 1000) : session.total > 0 ? session.done / session.total : 0);

  const remaining = (ms: number) => {
    const s = Math.ceil(ms / 1000);
    const h = Math.floor(s / 3600);
    return h > 0 ? `${h}:${String(Math.floor((s % 3600) / 60)).padStart(2, "0")}:${String(s % 60).padStart(2, "0")}` : clock(s * 1000);
  };

  const stateText: Record<string, string> = {
    idle: "Ready",
    busy: "Spotify is busy",
    recording: "Recording",
    setup: "Fix Setup",
  };
  const pauseText: Record<string, string> = {
    took_over: "You took over Spotify",
    by_user: "Paused",
    setup: "Paused – fix Setup",
  };
</script>

<Panel label="Echolot">
  <div class="who small" title={s.state === "busy" ? "Waiting while Spotify plays something else, here or on another device" : undefined}>
    <span class="dot" class:offline={s.connection === "offline"}></span>
    {s.connection === "offline" ? "Offline" : "Connected"}{#if s.state}<span class="muted"> · {stateText[s.state] ?? s.state}</span>{/if}
  </div>
  <div class="account mono muted" title="{s.user}@{s.host}">{s.user}@{s.host}</div>
  {#if s.state === "setup" && s.detail}<div class="hint warn small">{s.detail}</div>{/if}

  {#if session && echolot.active}
    <div class="session">
      <div class="title">{session.title}</div>
      <div class="bar"><span style:width="{progress * 100}%"></span></div>
      <div class="counts mono small">
        <span>{session.done}/{session.total}</span>
        <span>{session.filed} added</span>
        {#if session.not_better}<span>{session.not_better} not better</span>{/if}
        {#if session.skipped}<span>{session.skipped} skipped</span>{/if}
        {#if session.failed}<span class="bad">{session.failed} failed</span>{/if}
        {#if session.seconds_total > 0}<span>{remaining(leftMs)} left</span>{:else if session.minutes_left}<span>{session.minutes_left} min left</span>{/if}
      </div>
      {#if session.state !== "running" && busy}
        <div class="hint warn small">{session.state === "paused" ? "Pausing" : "Ending"} after this song…</div>
      {:else if session.state === "paused"}
        <div class="hint warn small">{pauseText[s.state === "setup" ? "setup" : session.reason] ?? "Paused"}</div>
      {/if}
      {#if s.current}<div class="current small">♪ {s.current.artist} – {s.current.title} <span class="muted mono">{clock(s.current.duration_ms)}</span></div>{/if}
      <div class="buttons">
        {#if session.state === "paused"}
          <button class="btn primary" onclick={() => echolot.session("resume")} disabled={echolot.busy || echolot.listing} title={echolot.listing ? "Wait until the list is done" : undefined}>Resume</button>
        {:else if session.state === "running"}
          <button class="btn" onclick={() => echolot.session("pause")} disabled={echolot.busy}>Pause</button>
        {/if}
        {#if s.current}
          <button class="btn" onclick={() => echolot.skip()} disabled={echolot.busy}>Skip</button>
        {/if}
        {#if session.state !== "stopped"}
          <button class="btn danger" onclick={() => echolot.session("stop")} disabled={echolot.busy}>End session</button>
        {/if}
      </div>
      {#if session.state === "running" && busy}<div class="muted small">Takes effect after this song.</div>{/if}
    </div>
  {:else if session}
    <div class="muted small last">Last session: {session.title} – {session.state}, {session.filed} added of {session.total}</div>
  {/if}
  {#if s.uploads > 0}<div class="muted small">Uploading {s.uploads}…</div>{/if}

  {#if s.lagging}<div class="hint small">Spotify reports what plays 6 s or more late. Consider a restart.</div>{/if}
  {#if echolot.error || s.error}<div class="error small">{echolot.error ?? s.error}</div>{/if}

  {#if echolot.results.length > 0}
    <ul class="results small">
      {#each echolot.results.slice(0, 8) as r (r.id)}
        <li title={readable(r.detail, true)}>
          <span class="tag mono" class:ok={r.result === "filed" || r.result === "recorded"} class:bad={r.result === "rejected" || r.result === "failed" || r.result === "stalled" || r.result === "crossfade"}>{resultText[r.result] ?? r.result}</span>
          <span class="song">{r.job.artist} – {r.job.title}</span>
        </li>
      {/each}
    </ul>
  {/if}
</Panel>

<style>
  .who {
    display: flex;
    align-items: center;
    gap: 8px;
    white-space: nowrap;
  }

  .account {
    margin: 2px 0 0 15px;
    font-size: 11px;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .last {
    margin-top: 10px;
  }

  .dot {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: var(--ok);
    box-shadow: 0 0 8px rgba(52, 211, 153, 0.6);
  }

  .dot.offline {
    background: var(--warn);
    box-shadow: none;
  }

  .small {
    font-size: 12px;
  }

  .session {
    display: grid;
    gap: 6px;
    margin-top: 14px;
  }

  .title {
    font-weight: 600;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .bar {
    height: 3px;
    background: var(--faint);
  }

  .bar span {
    display: block;
    height: 100%;
    background: var(--edge);
  }

  .counts {
    display: flex;
    flex-wrap: wrap;
    gap: 4px 10px;
    color: var(--muted);
  }

  .ok {
    color: var(--ok);
  }

  .bad {
    color: var(--bad);
  }

  .current {
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .buttons {
    display: grid;
    grid-auto-columns: 1fr;
    grid-auto-flow: column;
    gap: 8px;
    margin-top: 4px;
  }

  .hint {
    padding: 6px 9px;
    margin-top: 6px;
    border: 1px solid rgba(251, 191, 36, 0.4);
    background: rgba(251, 191, 36, 0.06);
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

  .tag.bad {
    color: var(--bad);
  }

  .song {
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
</style>
