<script lang="ts">
  import { clock, type Track } from "../lib/api";
  import { echolot } from "../lib/echolot.svelte";
  import { session } from "../lib/session.svelte";
  import Cover from "./Cover.svelte";
  import Meter from "./Meter.svelte";
  import Panel from "./Panel.svelte";

  const status = $derived(session.status);
  const player = $derived(session.player);
  const phase = $derived(session.listening ? (status?.phase ?? "idle") : "off");
  /** The song being recorded, else what the player shows (outside a run: what the source list says). */
  const source = $derived(session.sources.find((s) => s.id === session.selected));
  const track = $derived<Track | null>(
    status?.track ?? player?.track ?? (!session.listening && source?.title ? { title: source.title, artist: source.artist, album: "", duration_ms: 0 } : null),
  );
  const recorded = $derived(!!status?.track);
  const elapsed = $derived(recorded ? (status?.recorded_ms ?? 0) : (player?.position_ms ?? 0));
  const progress = $derived(track?.duration_ms ? Math.min(1, elapsed / track.duration_ms) : 0);
  const playing = $derived(recorded || (session.listening ? !!player?.playing : !!source?.playing));

  const labels = {
    off: "Not recording",
    idle: "Ready – waiting for a song from its start",
    waiting: "Armed – waiting for sound",
    recording: "Recording",
  };

  // The mixer level is linear amplitude; a cubic audio taper makes the whole slider audibly useful.
  const position = $derived(Math.cbrt(session.monitorVolume ?? 1));
</script>

<Panel label="Now">
  <div class="hero">
    <Cover src={track ? session.coverOf(track) : null} size={148} />
    <div class="body">
      <div class="state {phase}">
        <span class="dot"></span>
        <span class="mono">{labels[phase]}</span>
        {#if track && !recorded}<span class="mono muted">· {playing ? "playing" : "paused"}, not recorded</span>{/if}
        {#if session.listening && player?.track && !recorded && !echolot.active && !echolot.listing}
          <button class="btn again" onclick={() => session.recordFromStart()} title="Pauses, jumps to 0:00 and plays it again: a song started inside an album (gapless) or mid-song has no clean start to record from.">Record from start</button>
        {/if}
        {#if session.listening && session.keyBlockedAt}
          <span class="chip mono" title="The recording continues. To pause, use Spotify's own pause button.">Media key blocked</span>
        {/if}
      </div>

      {#if track}
        <div class="track">
          <div class="title">{track.title}</div>
          <div class="muted">{track.artist}{track.album ? ` · ${track.album}` : ""}</div>
        </div>
        {#if track.duration_ms}
          <div class="bar" class:rec={phase === "recording"}><div class="fill" style:width="{progress * 100}%"></div></div>
          <div class="times mono muted">
            <span>{clock(elapsed)}</span>
            <span>{clock(track.duration_ms)}</span>
          </div>
        {/if}
      {:else}
        <p class="muted hint">
          {session.listening
            ? "Play a track from its start. It is kept whole: pausing, skipping or seeking discards it."
            : "Pick a source above and start recording."}
        </p>
      {/if}
    </div>
  </div>

  {#if session.listening}
    {#if session.monitorMuted}
      <div class="notice warn mono">
        <span>Monitor muted – the output device changed.</span>
        <button class="link mono" onclick={() => session.unmuteMonitor()}>Unmute here</button>
      </div>
    {/if}
    <div class="signal">
      {#if status?.grid}
        {@const exact = status.grid.bit24 === 1}
        {@const volume = status.grid.volume}
        <div
          class="proof mono"
          class:ok={exact}
          class:warn={!exact}
          title={exact
            ? "Every sample arrives exactly as Spotify's lossless stream carries it."
            : volume
              ? "The lossless stream arrives exactly, only scaled: Spotify's own volume slider isn't at maximum. Set it to full – the monitor keeps the level you hear."
              : "Samples arrive changed: a lossy stream, or something in between – Spotify's volume below 100 %, normalization, the equalizer, crossfade."}>
          <span class="dot"></span>
          {exact ? "Lossless · arriving bit-exact" : volume ? `Spotify's volume at ${Math.round(volume * 1000) / 10} % · set it to maximum` : "Not lossless · lossy or altered"}
        </div>
      {/if}
      <Meter rms={status?.rms ?? [0, 0]} peak={status?.level ?? [0, 0]} />
      {#if session.platform?.monitor && session.settings?.monitor && session.monitorVolume !== null}
        <label class="volume" title="Mitschnitt in the {session.platform.os === 'windows' ? 'Windows volume mixer' : 'system mixer'}, on a loudness scale">
          <span class="mono muted">Monitor</span>
          <input type="range" min="0" max="1" step="0.01" value={position} oninput={(e) => session.setMonitorVolume(Math.pow(+e.currentTarget.value, 3))} />
          <span class="mono">{Math.round(position * 100)}</span>
        </label>
      {/if}
    </div>
  {/if}
</Panel>

<style>
  .hero {
    display: grid;
    grid-template-columns: auto minmax(0, 1fr);
    gap: 20px;
    align-items: center;
  }

  .body {
    min-width: 0;
  }

  .signal {
    margin-top: 6px;
  }

  .state {
    display: flex;
    align-items: center;
    gap: 10px;
    margin-bottom: 14px;
    font-size: 12px;
    letter-spacing: 0.06em;
    text-transform: uppercase;
  }

  .dot {
    width: 8px;
    height: 8px;
    background: var(--faint);
  }

  .idle .dot,
  .waiting .dot {
    background: var(--cyan);
    box-shadow: 0 0 10px var(--cyan);
  }

  .recording .dot {
    background: var(--rec);
    box-shadow: 0 0 12px var(--rec);
    animation: pulse 1.2s ease-in-out infinite;
  }

  .recording {
    color: var(--rec);
  }

  @keyframes pulse {
    50% {
      opacity: 0.3;
    }
  }

  .title {
    font-size: 26px;
    font-weight: 650;
    letter-spacing: -0.01em;
    line-height: 1.2;
  }

  .track {
    margin-bottom: 14px;
  }

  .bar {
    height: 2px;
    background: var(--faint);
  }

  .fill {
    height: 100%;
    background: var(--edge);
    box-shadow: 0 0 8px var(--cyan);
    transition: width 120ms linear;
  }

  .bar.rec .fill {
    background: var(--rec);
    box-shadow: 0 0 8px var(--rec);
  }

  .times {
    display: flex;
    justify-content: space-between;
    margin-top: 6px;
    font-size: 11px;
  }

  .hint {
    margin: 0;
  }

  .notice {
    display: flex;
    justify-content: space-between;
    gap: 12px;
    margin-top: 14px;
    padding: 7px 10px;
    font-size: 11px;
    letter-spacing: 0.04em;
    border: 1px solid var(--line);
    background: rgba(34, 211, 238, 0.06);
    color: var(--cyan);
  }

  .notice.warn {
    border-color: rgba(251, 191, 36, 0.45);
    background: rgba(251, 191, 36, 0.06);
    color: var(--warn);
  }

  .again {
    margin-left: auto;
    padding: 3px 10px;
    font-size: 11px;
  }

  .chip {
    margin-left: auto;
    padding: 1px 7px;
    font-size: 10px;
    line-height: 14px;
    letter-spacing: 0.08em;
    text-transform: uppercase;
    color: var(--cyan);
    border: 1px solid rgba(34, 211, 238, 0.45);
    background: rgba(34, 211, 238, 0.08);
  }

  .link {
    border: none;
    background: none;
    padding: 0;
    color: inherit;
    text-decoration: underline;
    text-transform: uppercase;
    font-size: 11px;
  }

  .proof {
    display: flex;
    align-items: center;
    gap: 8px;
    margin-top: 16px;
    font-size: 11px;
    letter-spacing: 0.08em;
    text-transform: uppercase;
  }

  .proof .dot {
    width: 6px;
    height: 6px;
    background: currentColor;
    box-shadow: 0 0 8px currentColor;
  }

  .volume {
    display: grid;
    grid-template-columns: auto 1fr 32px;
    gap: 12px;
    align-items: center;
    margin-top: 14px;
    font-size: 11px;
    text-transform: uppercase;
    letter-spacing: 0.1em;
  }

  input[type="range"] {
    appearance: none;
    height: 2px;
    background: var(--edge);
  }

  input[type="range"]::-webkit-slider-thumb {
    appearance: none;
    width: 10px;
    height: 14px;
    background: var(--text);
    border: 1px solid var(--cyan);
  }
</style>
