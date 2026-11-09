<script lang="ts">
  import { onMount } from "svelte";
  import Panel from "../components/Panel.svelte";
  import { api, type Format, type Settings } from "../lib/api";
  import { echolot } from "../lib/echolot.svelte";
  import { session } from "../lib/session.svelte";

  const s = $derived(session.settings);
  type Choice = { value: Format; label: string; note: string; needs?: "alac" | "aac" };
  const losslessFormats: Choice[] = [
    { value: "flac", label: "FLAC", note: "Every sample as it arrived – 16-bit for 16-bit masters, else 24-bit. Plays almost everywhere but Apple Music." },
    { value: "alac", label: "ALAC", note: "Lossless like FLAC, as .m4a for Apple Music and iPhone.", needs: "alac" },
  ];
  const lossyFormats: Choice[] = [
    { value: "flac", label: "FLAC", note: "Keeps the stream exactly as it arrived, about four times the size of Opus." },
    { value: "opus", label: "Opus", note: "256 kbit/s – the best sound per size. Plays in VLC, foobar2000, browsers and on Android; not in Apple Music." },
    { value: "aac", label: "AAC", note: "192 kbit/s .m4a – plays everywhere, Apple and car stereos too.", needs: "aac" },
    { value: "mp3", label: "MP3", note: "320 kbit/s – plays on anything, the weakest sound of these at its size." },
  ];
  const extOf: Record<Format, string> = { flac: "flac", alac: "m4a", opus: "opus", aac: "m4a", mp3: "mp3" };

  const sample = { artist: "Daft Punk", title: "Digital Love", album: "Discovery", album_artist: "Daft Punk", track: "03" };
  /** Saved when the field is left. */
  let draft = $state<string | null>(null);
  const preview = $derived.by(() => {
    if (!s) return "";
    const filled = (draft ?? s.template).split("/").map((part) => part.replaceAll("{artist}", sample.artist).replaceAll("{title}", sample.title).replaceAll("{album_artist}", sample.album_artist).replaceAll("{album}", sample.album).replaceAll("{track}", sample.track).trim());
    return `${filled.join(" / ")}.${extOf[s.lossless_format]}`;
  });

  // ffmpeg, for AAC on Linux, may have been installed since the start.
  onMount(async () => (session.platform = await api.platform()));

  function set<K extends keyof Settings>(key: K, value: Settings[K]) {
    session.settings![key] = value;
    session.save();
  }
</script>

{#snippet choose(settings: Settings, key: "lossless_format" | "lossy_format", name: string, choices: Choice[])}
  {@const missing = choices.filter((f) => f.needs !== undefined && session.platform?.[f.needs] === false)}
  <div class="field">
    <span class="name mono">{name}</span>
    <div class="seg">
      {#each choices as f}
        {@const off = f.needs !== undefined && session.platform?.[f.needs] === false}
        <button class:on={settings[key] === f.value} disabled={off} title={off ? "Needs ffmpeg, which isn't installed" : undefined} onclick={() => set(key, f.value)}>{f.label}</button>
      {/each}
    </div>
    <span class="note muted">{choices.find((f) => f.value === settings[key])?.note}{missing.length ? ` ${missing.map((f) => f.label).join(", ")} needs ffmpeg: install it to pick it.` : ""}</span>
  </div>
{/snippet}

{#if s}
  <div class="grid">
    <Panel label="Output">
      <label class="field">
        <span class="name mono">Folder</span>
        <div class="row">
          <input value={s.out_dir} onchange={(e) => set("out_dir", e.currentTarget.value)} spellcheck="false" />
          <button class="btn" onclick={() => api.open(s.out_dir)}>Open</button>
        </div>
      </label>

      {@render choose(s, "lossless_format", "Lossless songs", losslessFormats)}
      {@render choose(s, "lossy_format", "Lossy songs", lossyFormats)}
      {#if s.lossy_format !== "flac"}
        <p class="info">
          Spotify streams these lossy already, or their master was made from a lossy file. Saving them as {lossyFormats.find((f) => f.value === s.lossy_format)?.label} encodes them a second time:
          not audible, but no longer exactly what arrived. FLAC keeps them exactly. Echolot always gets FLAC.
        </p>
      {/if}

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
        <span class="note muted"><code>{"{artist}"}</code> <code>{"{title}"}</code> <code>{"{album}"}</code> <code>{"{album_artist}"}</code> <code>{"{track}"}</code> · <code>/</code> makes folders</span>
        <span class="note mono preview">→ {preview}</span>
      </label>

      <p class="muted rate">Sample rate: original (no resampling – required for bit-exact files). Opus is always 48 kHz.</p>
      <div class="log">
        <span class="muted">What happened while listening, for tracking down problems.</span>
        <button class="btn" onclick={() => api.openLog()}>Show log</button>
      </div>
    </Panel>

    <Panel label="Behaviour">
      {#if session.platform?.monitor}
        <label class="toggle">
          <input type="checkbox" checked={s.monitor} onchange={(e) => set("monitor", e.currentTarget.checked)} />
          <span>
            <span class="title">Monitor</span>
            <span class="note muted">Play what is recorded on your default output, with its own volume. Off, recordings are silent.{session.platform.os === "windows" ? ` Replaces Windows' "Listen to this device" (muted while listening).` : ""}</span>
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
        <input type="checkbox" checked={s.mark_files} onchange={(e) => set("mark_files", e.currentTarget.checked)} />
        <span>
          <span class="title">Mark files as Mitschnitt recordings</span>
          <span class="note muted">Tags the encoder "Mitschnitt" and a comment such as "Recorded from Spotify on 2026-11-05: bit-exact, 16-bit". Off, a file doesn't say where it came from.</span>
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

    <Panel label="Development">
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
  <p class="about muted">
    Mitschnitt is open source:
    <button class="link mono" onclick={() => api.open("https://github.com/Zweisteinium/mitschnitt")}>github.com/Zweisteinium/mitschnitt</button>
    – ideas, bugs and a star are welcome there.
  </p>
{/if}

<style>
  .about {
    margin: 18px 0 0;
    font-size: 12px;
    text-align: center;
  }

  .link {
    border: none;
    background: none;
    padding: 0;
    color: var(--cyan);
    font: inherit;
    cursor: pointer;
  }

  .link:hover {
    text-decoration: underline;
  }

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

  .seg button:disabled {
    opacity: 0.4;
    cursor: default;
  }

  .note {
    font-size: 12px;
  }

  .info {
    margin: -4px 0 14px;
    padding: 8px 10px;
    font-size: 12px;
    line-height: 1.5;
    color: var(--cyan);
    border: 1px solid var(--line);
    background: rgba(34, 211, 238, 0.06);
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
