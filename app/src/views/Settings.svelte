<script lang="ts">
  import Panel from "../components/Panel.svelte";
  import { api, type Format, type Settings } from "../lib/api";
  import { echolot } from "../lib/echolot.svelte";
  import { session } from "../lib/session.svelte";

  const s = $derived(session.settings);
  const formats: { value: Format; label: string; note: string }[] = [
    { value: "flac", label: "FLAC", note: "lossless, compressed, tagged – 16-bit for 16-bit masters, else 24-bit; lossy recordings as MP3" },
    { value: "wav", label: "WAV", note: "lossless, uncompressed, no tags – 16-bit for 16-bit masters, else 24-bit; lossy recordings as MP3" },
    { value: "mp3", label: "MP3", note: "320 kbit/s, tagged – lossy, encoded from the verified recording" },
  ];

  const sample = { artist: "Daft Punk", title: "Digital Love", album: "Discovery" };
  /** Saved when the field is left. */
  let draft = $state<string | null>(null);
  const preview = $derived.by(() => {
    if (!s) return "";
    const filled = (draft ?? s.template).split("/").map((part) => part.replaceAll("{artist}", sample.artist).replaceAll("{title}", sample.title).replaceAll("{album}", sample.album));
    return `${filled.join(" / ")}.${s.format}`;
  });

  function set<K extends keyof Settings>(key: K, value: Settings[K]) {
    session.settings![key] = value;
    session.save();
  }
</script>

{#if s}
  <div class="grid">
    <Panel label="Output" index="01">
      <label class="field">
        <span class="name mono">Folder</span>
        <div class="row">
          <input value={s.out_dir} onchange={(e) => set("out_dir", e.currentTarget.value)} spellcheck="false" />
          <button class="btn" onclick={() => api.open(s.out_dir)}>Open</button>
        </div>
      </label>

      <div class="field">
        <span class="name mono">Format</span>
        <div class="seg">
          {#each formats as f}
            <button class:on={s.format === f.value} onclick={() => set("format", f.value)}>{f.label}</button>
          {/each}
        </div>
        <span class="note muted">{formats.find((f) => f.value === s.format)?.note}</span>
      </div>

      <label class="field">
        <span class="name mono">File name</span>
        <input
          value={s.template}
          oninput={(e) => (draft = e.currentTarget.value)}
          onchange={(e) => {
            set("template", e.currentTarget.value);
            draft = null;
          }}
          spellcheck="false"
        />
        <span class="note muted"><code>{"{artist}"}</code> <code>{"{title}"}</code> <code>{"{album}"}</code> · <code>/</code> makes folders</span>
        <span class="note mono preview">→ {preview}</span>
      </label>

      <p class="muted rate">Sample rate: original (no resampling – required for bit-exact files).</p>
      <div class="log">
        <span class="muted">What happened while listening, for tracking down problems.</span>
        <button class="btn" onclick={() => api.openLog()}>Show log</button>
      </div>
    </Panel>

    <Panel label="Behaviour" index="02">
      {#if session.platform?.monitor}
        <label class="toggle">
          <input type="checkbox" checked={s.monitor} onchange={(e) => set("monitor", e.currentTarget.checked)} />
          <span>
            <span class="title">Monitor</span>
            <span class="note muted">Play what is recorded on your default output. Replaces Windows' "Listen to this device" (muted while listening).</span>
          </span>
        </label>
      {/if}
      {#if session.platform?.media_keys}
        <label class="toggle">
          <input type="checkbox" checked={s.protect_media_keys} onchange={(e) => set("protect_media_keys", e.currentTarget.checked)} />
          <span>
            <span class="title">Protect recordings from media keys</span>
            <span class="note muted">While recording, media keys, headset buttons and earbud in-ear detection are ignored. Only Spotify's own pause button interrupts.</span>
          </span>
        </label>
      {/if}
      <label class="toggle">
        <input type="checkbox" checked={s.skip_existing} onchange={(e) => set("skip_existing", e.currentTarget.checked)} />
        <span>
          <span class="title">Skip tracks you already have</span>
          <span class="note muted">Don't record a track again when its file already exists (any format).</span>
        </span>
      </label>
      <label class="toggle">
        <input type="checkbox" checked={s.keep_flagged} onchange={(e) => set("keep_flagged", e.currentTarget.checked)} />
        <span>
          <span class="title">Keep flagged files</span>
          <span class="note muted">Keep recordings marked as altered or corrupt. They are listed either way.</span>
        </span>
      </label>
      <label class="toggle">
        <input type="checkbox" checked={s.keep_echolot_copies} onchange={(e) => set("keep_echolot_copies", e.currentTarget.checked)} />
        <span>
          <span class="title">Keep a copy of Echolot recordings</span>
          <span class="note muted">Also keep what Echolot took in the output folder. Rejected ones always stay in "Not taken".</span>
        </span>
      </label>
      <p class="muted rate">Changes apply the next time you start listening{session.platform?.monitor ? "; the monitor volume applies immediately" : ""}.</p>
    </Panel>

    <Panel label="Development" index="03">
      <div class="dev">
        <span>
          <span class="title">Record an exported list</span>
          <span class="note muted">For development and testing with Echolot. Records the songs of a list exported from Echolot, without a connection. Progress shows on the Listen tab.</span>
        </span>
        <label class="btn" class:disabled={echolot.listing || echolot.busy}>
          Choose list…
          <input type="file" accept=".json,application/json" disabled={echolot.listing || echolot.busy} onchange={(e) => {
              const input = e.currentTarget;
              if (input.files?.[0]) echolot.recordList(input.files[0]);
              input.value = "";
            }} />
        </label>
      </div>
      {#if echolot.error}<p class="error">{echolot.error}</p>{/if}
    </Panel>
  </div>
{/if}

<style>
  .dev {
    display: grid;
    grid-template-columns: 1fr auto;
    gap: 12px;
    align-items: center;
  }

  .dev .title,
  .dev .note {
    display: block;
  }

  .dev .note {
    font-size: 12px;
  }

  .dev label {
    cursor: pointer;
    white-space: nowrap;
  }

  .dev input {
    display: none;
  }

  .dev .disabled {
    opacity: 0.5;
    pointer-events: none;
  }

  .error {
    margin: 10px 0 0;
    font-size: 12px;
    color: var(--bad);
  }

  .grid {
    display: grid;
    grid-template-columns: 1.4fr 1fr;
    gap: 16px;
    align-items: start;
  }

  .field {
    display: grid;
    gap: 6px;
    margin-bottom: 18px;
  }

  .name {
    font-size: 11px;
    letter-spacing: 0.12em;
    text-transform: uppercase;
    color: var(--muted);
  }

  .row {
    display: grid;
    grid-template-columns: 1fr auto;
    gap: 8px;
  }

  input:not([type]) {
    width: 100%;
    padding: 7px 10px;
    border: 1px solid var(--faint);
    background: var(--panel-2);
    outline: none;
    font-family: var(--mono);
    font-size: 12px;
    user-select: text;
  }

  input:not([type]):focus {
    border-color: var(--line-strong);
  }

  .seg {
    display: flex;
    border: 1px solid var(--line);
    width: max-content;
  }

  .seg button {
    padding: 6px 16px;
    border: none;
    background: transparent;
    font-family: var(--mono);
    font-size: 12px;
    letter-spacing: 0.06em;
    color: var(--muted);
  }

  .seg button + button {
    border-left: 1px solid var(--line);
  }

  .seg button.on {
    background: var(--edge);
    color: #05070a;
    font-weight: 700;
  }

  .note {
    font-size: 12px;
  }

  code {
    font-family: var(--mono);
    color: var(--cyan);
  }

  .preview {
    color: var(--text);
  }

  .rate {
    margin: 0;
    font-size: 12px;
  }

  .log {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    margin-top: 16px;
    padding-top: 14px;
    border-top: 1px solid var(--faint);
    font-size: 12px;
  }

  .toggle {
    display: grid;
    grid-template-columns: auto 1fr;
    gap: 12px;
    margin-bottom: 16px;
    cursor: pointer;
  }

  .toggle input {
    appearance: none;
    width: 16px;
    height: 16px;
    margin-top: 2px;
    border: 1px solid var(--line-strong);
    background: transparent;
  }

  .toggle input:checked {
    background: var(--edge);
    box-shadow: 0 0 8px rgba(34, 211, 238, 0.5);
  }

  .toggle > span {
    display: grid;
  }

  .title {
    font-weight: 600;
  }

  @media (max-width: 840px) {
    .grid {
      grid-template-columns: 1fr;
    }
  }
</style>
