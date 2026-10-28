<script lang="ts">
  import { clock } from "../lib/api";
  import { session } from "../lib/session.svelte";
  import Meter from "./Meter.svelte";
  import Panel from "./Panel.svelte";

  const status = $derived(session.status);
  const track = $derived(status?.track ?? null);
  const phase = $derived(session.listening ? (status?.phase ?? "idle") : "off");
  const progress = $derived(track && status ? Math.min(1, status.recorded_ms / track.duration_ms) : 0);

  const labels = {
    off: "Not listening",
    idle: "Listening – waiting for a track",
    waiting: "Armed – waiting for sound",
    recording: "Recording",
  };

  // The mixer level is linear amplitude; a cubic audio taper makes the whole slider audibly useful.
  const position = $derived(Math.cbrt(session.monitorVolume ?? 1));
</script>

<Panel label="Now">
  <div class="state {phase}">
    <span class="dot"></span>
    <span class="mono">{labels[phase]}</span>
    {#if session.listening && session.keyBlockedAt}
      <span class="chip mono" title="The recording continues. To pause, use Spotify's own pause button.">Media key blocked</span>
    {/if}
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
        ? "Play a track from its start. It is kept whole: pausing, skipping or seeking discards it."
        : "Pick a source and start listening."}
    </p>
  {/if}

  {#if session.listening}
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
        {exact ? "Lossless · arriving bit-exact" : "Not lossless · lossy or altered"}
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
