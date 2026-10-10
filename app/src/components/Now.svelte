<script lang="ts">
  import { clock } from "../lib/api";
  import { session } from "../lib/session.svelte";
  import Panel from "./Panel.svelte";

  const status = $derived(session.status);
  const track = $derived(status?.track ?? null);
  const phase = $derived(session.listening ? (status?.phase ?? "idle") : "off");
  const progress = $derived(track && status ? Math.min(1, status.recorded_ms / track.duration_ms) : 0);

  const labels = {
    off: "Not listening",
    idle: "Listening — recording starts with the next track",
    waiting: "Armed — waiting for the first sample",
    recording: "Recording",
  };

  /** Meter deflection 0..1 for a linear level, IEC 60268-18 style: -20 dB sits at half, the top 10 dB get room. */
  function meter(x: number): number {
    const db = 20 * Math.log10(Math.max(x, 1e-6));
    const curve: [number, number][] = [[-70, 0], [-60, 0.025], [-50, 0.075], [-40, 0.15], [-30, 0.3], [-20, 0.5], [0, 1]];
    if (db <= -70) return 0;
    if (db >= 0) return 1;
    const i = curve.findIndex(([d]) => d >= db);
    const [[d0, m0], [d1, m1]] = [curve[i - 1], curve[i]];
    return m0 + ((db - d0) / (d1 - d0)) * (m1 - m0);
  }

  // Mitschnitt's Windows mixer entry: moving this moves the mixer, and the other way round.
  const volume = $derived(session.monitorVolume ?? 1);
</script>

<Panel label="Now" index="02">
  <div class="state {phase}">
    <span class="dot"></span>
    <span class="mono">{labels[phase]}</span>
  </div>

  {#if track}
    <div class="track">
      <div class="title">{track.title}</div>
      <div class="muted">{track.artist}{track.album ? ` · ${track.album}` : ""}</div>
    </div>
    <div class="bar" class:rec={phase === "recording"}><div class="fill" style:width="{progress * 100}%"></div></div>
    <div class="times mono muted">
      <span>{clock(status?.recorded_ms ?? 0)}</span>
      <span>{clock(track.duration_ms)}</span>
    </div>
  {:else}
    <p class="muted hint">
      {session.listening
        ? "Play a track from its beginning. It is recorded from its first sample to its exact length; pausing, skipping or seeking discards it."
        : "Pick a source and start listening."}
    </p>
  {/if}

  {#if session.listening}
    {#if session.keyBlockedAt}
      <div class="notice mono">Media key blocked – the recording continues. To pause, use Spotify's own pause button.</div>
    {/if}
    {#if session.monitorMuted}
      <div class="notice warn mono">
        <span>Monitor muted – the output device changed.</span>
        <button class="link mono" onclick={() => session.unmuteMonitor()}>Unmute here</button>
      </div>
    {/if}
    {#if status?.grid}
      {@const exact = status.grid.bit24 === 1}
      <div class="proof mono" class:ok={exact} class:warn={!exact} title="Share of the audio that arrives as exact 24-bit values">
        <span class="dot"></span>
        {exact ? "Lossless · arriving bit-exact" : `Not lossless · ${(status.grid.bit24 * 100).toFixed(1)} % exact`}
      </div>
    {/if}
    <div class="meters">
      {#each [0, 1] as i (i)}
        <div class="meter">
          <div class="level" style:clip-path="inset(0 {100 - meter(status?.rms[i] ?? 0) * 100}% 0 0)"></div>
          <div class="peak" style:left="{meter(status?.level[i] ?? 0) * 100}%"></div>
        </div>
      {/each}
    </div>
    {#if session.settings?.monitor && session.monitorVolume !== null}
      <label class="volume" title="Same as Mitschnitt in the Windows volume mixer">
        <span class="mono muted">Monitor</span>
        <input type="range" min="0" max="1" step="0.01" value={volume} oninput={(e) => session.setMonitorVolume(+e.currentTarget.value)} />
        <span class="mono">{Math.round(volume * 100)}</span>
      </label>
    {/if}
  {/if}
</Panel>

<style>
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
    font-size: 22px;
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

  .meters {
    display: grid;
    gap: 3px;
    margin-top: 16px;
  }

  /* Segmented LED-style meter. */
  .meter {
    position: relative;
    height: 6px;
    background: repeating-linear-gradient(90deg, var(--faint) 0 6px, transparent 6px 8px);
  }

  .level {
    height: 100%;
    background:
      repeating-linear-gradient(90deg, transparent 0 6px, var(--panel) 6px 8px),
      linear-gradient(90deg, #0e5f73 0%, #22d3ee 38%, #a78bfa 64%, #f43f5e 80%, #b91c1c 100%);
    transition: clip-path 80ms linear;
  }

  /* Peak hold line; red from about -6 dBFS on. */
  .peak {
    position: absolute;
    top: -1px;
    bottom: -1px;
    width: 2px;
    margin-left: -2px;
    background: #e6f6fb;
    transition: left 80ms linear;
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
