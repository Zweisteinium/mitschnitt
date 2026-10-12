<script lang="ts">
  import { clock } from "../lib/api";
  import { echolot } from "../lib/echolot.svelte";
  import Panel from "./Panel.svelte";

  const s = $derived(echolot.snapshot);
  const session = $derived(s.session);
  /** A song is being worked on: Pause and Stop take effect after it. */
  const busy = $derived(s.current !== null);
  const progress = $derived(session && session.total > 0 ? session.done / session.total : s.total > 0 ? s.done / s.total : 0);

  const stateText: Record<string, string> = {
    idle: "Ready",
    busy: "Waiting - you are using Spotify",
    recording: "Recording for Echolot",
    setup: "Waiting - fix Setup",
  };
  const pauseText: Record<string, string> = {
    took_over: "You took over Spotify",
    by_user: "Paused",
    setup: "Paused - fix Setup",
  };
  const resultText: Record<string, string> = {
    filed: "filed",
    not_better: "not better",
    rejected: "rejected",
    recorded: "recorded",
    lossy_only: "no lossless",
    wrong_song: "wrong song",
    took_over: "taken over",
    stalled: "stalled",
    failed: "failed",
  };
</script>

<Panel label="Echolot" index="04">
  {#snippet actions()}
    {#if echolot.connected}
      <button class="btn" onclick={() => echolot.disconnect()} disabled={echolot.busy || busy} title={busy ? "Wait until the current song is done" : undefined}>Disconnect</button>
    {/if}
  {/snippet}

  {#if echolot.connected}
    <div class="who mono">
      <span class="dot" class:offline={s.connection === "offline"}></span>
      {s.connection === "offline" ? "Offline" : "Connected"} · {s.user}@{s.host}
    </div>
    <div class="muted small">{s.device}{#if s.state} · {stateText[s.state] ?? s.state}{/if}</div>
    {#if s.state === "setup" && s.detail}<div class="hint warn small">{s.detail}</div>{/if}

    {#if session && (session.state === "running" || session.state === "paused" || busy)}
      <div class="session">
        <div class="title">{session.title}</div>
        <div class="bar"><span style:width="{progress * 100}%"></span></div>
        <div class="counts mono small">
          <span>{session.done}/{session.total}</span>
          <span class="ok">{session.filed} filed</span>
          {#if session.not_better}<span>{session.not_better} not better</span>{/if}
          {#if session.skipped}<span>{session.skipped} skipped</span>{/if}
          {#if session.failed}<span class="bad">{session.failed} failed</span>{/if}
          {#if session.minutes_left}<span>{session.minutes_left} min left</span>{/if}
        </div>
        {#if session.state !== "running" && busy}
          <div class="hint warn small">{session.state === "paused" ? "Pausing" : "Stopping"} after this song…</div>
        {:else if session.state === "paused"}
          <div class="hint warn small">{pauseText[session.reason] ?? "Paused"}</div>
        {/if}
        {#if s.current}<div class="current small">♪ {s.current.artist} – {s.current.title} <span class="muted mono">{clock(s.current.duration_ms)}</span></div>{/if}
        <div class="buttons">
          {#if session.state === "paused"}
            <button class="btn primary" onclick={() => echolot.session("resume")} disabled={echolot.busy}>Resume</button>
          {:else if session.state === "running"}
            <button class="btn" onclick={() => echolot.session("pause")} disabled={echolot.busy}>{busy ? "Pause after this song" : "Pause"}</button>
          {/if}
          {#if session.state !== "stopped"}
            <button class="btn danger" onclick={() => echolot.session("stop")} disabled={echolot.busy}>{busy ? "Stop after this song" : "Stop"}</button>
          {/if}
        </div>
      </div>
    {:else if session}
      <div class="muted small">Last session: {session.title} - {session.state}, {session.filed} filed of {session.total}</div>
    {:else}
      <div class="muted small">Choose songs to record in Echolot; they start here while Mitschnitt is open.</div>
    {/if}
    {#if s.uploads > 0}<div class="muted small">Uploading {s.uploads}…</div>{/if}
  {:else if echolot.running}
    <div class="title">{s.detail || "Exported list"}</div>
    <div class="bar"><span style:width="{progress * 100}%"></span></div>
    <div class="counts mono small"><span>{s.done}/{s.total}</span></div>
    {#if s.current}<div class="current small">♪ {s.current.artist} – {s.current.title}</div>{/if}
    <div class="buttons"><button class="btn danger" onclick={() => echolot.stopQueue()}>Stop</button></div>
  {:else if echolot.pairing}
    <p class="small">Approve the connection in the browser. If Mitschnitt doesn't open by itself, paste the code Echolot shows:</p>
    <form
      class="code"
      onsubmit={(e) => {
        e.preventDefault();
        echolot.submitCode();
      }}
    >
      <input class="mono" bind:value={echolot.code} placeholder="Code" aria-label="Code from Echolot" />
      <button class="btn primary" type="submit" disabled={echolot.busy || !echolot.code.trim()}>OK</button>
    </form>
    <button class="btn wide" onclick={() => echolot.cancel()}>Cancel</button>
  {:else}
    <label class="small muted" for="echolot-url">Address</label>
    <input id="echolot-url" class="mono url" bind:value={echolot.url} placeholder="https://echolot.example.org" />
    <p class="small note">Echolot will be able to start Spotify on this PC and put recordings into your library.</p>
    {#if echolot.confirming}<p class="small">Connect to this Echolot?</p>{/if}
    <button class="btn primary wide" onclick={() => echolot.connect()} disabled={echolot.busy || !echolot.url.trim()}>Connect Echolot</button>
    <button class="btn wide import" onclick={() => echolot.importQueue()} disabled={echolot.busy}>Import exported list…</button>
  {/if}

  {#if echolot.error || s.error}<div class="error small">{echolot.error ?? s.error}</div>{/if}

  {#if echolot.results.length > 0}
    <ul class="results small">
      {#each echolot.results.slice(0, 8) as r (r.id)}
        <li title={r.detail}>
          <span class="tag mono" class:ok={r.result === "filed" || r.result === "recorded"} class:bad={r.result === "rejected" || r.result === "failed"}>{resultText[r.result] ?? r.result}</span>
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
    font-size: 12px;
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
    display: flex;
    gap: 8px;
    margin-top: 4px;
  }

  .hint {
    padding: 6px 9px;
    margin-top: 6px;
    border: 1px solid rgba(251, 191, 36, 0.4);
    background: rgba(251, 191, 36, 0.06);
  }

  .url,
  .code input {
    width: 100%;
    margin: 4px 0 8px;
    padding: 7px 9px;
    border: 1px solid var(--faint);
    background: var(--panel-2);
    color: var(--text);
  }

  .code {
    display: grid;
    grid-template-columns: 1fr auto;
    gap: 8px;
    align-items: center;
  }

  .code input {
    margin: 0;
  }

  .note {
    color: var(--muted);
    margin: 0 0 10px;
  }

  .wide {
    width: 100%;
  }

  .import {
    margin-top: 8px;
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

  .song {
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
</style>
