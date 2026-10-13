// `npm run demo`: Vite points `@tauri-apps/api/{core,event,app}` here, for the README screenshot.
import type { Check, EcholotSession, HistoryRecord, RecorderSnapshot, Settings, Status, Track } from "../lib/api";

type Handler = (event: { payload: unknown }) => void;

const handlers = new Map<string, Set<Handler>>();

function emit(event: string, payload: unknown) {
  for (const h of handlers.get(event) ?? []) h({ payload });
}

/** Emit as if it happened `minutes` ago (the UI stamps entries with `new Date()`). */
function earlier(minutes: number, action: () => void) {
  const Real = Date;
  const at = Real.now() - minutes * 60_000;
  globalThis.Date = class extends Real {
    constructor(...args: unknown[]) {
      if (args.length) super(...(args as [string]));
      else super(at);
    }
  } as DateConstructor;
  try {
    action();
  } finally {
    globalThis.Date = Real;
  }
}

const rick: Track = { title: "Never Gonna Give You Up", artist: "Rick Astley", album: "Whenever You Need Somebody", duration_ms: 213_573 };

const out = "C:\\Users\\Anna\\Music\\Mitschnitt";
const settings: Settings = {
  out_dir: out,
  format: "flac",
  template: "{artist} - {title}",
  keep_flagged: true,
  skip_existing: true,
  protect_media_keys: true,
  monitor: true,
  keep_echolot_copies: true,
  echolot_url: "https://echolot.example.org",
};

const checks: Check[] = [
  { id: "cable", title: "VB-Cable installed", state: "ok", detail: "CABLE Input (VB-Audio Virtual Cable)", fix: null },
  { id: "rate", title: "Cable at the app's sample rate", state: "ok", detail: "44.1 kHz, like Spotify's lossless streams", fix: null },
  { id: "route", title: "Spotify plays into the cable", state: "ok", detail: "Only Spotify's own audio is captured", fix: null },
  { id: "normalize", title: "Volume normalization off", state: "ok", detail: "Spotify passes the audio through untouched", fix: null },
  { id: "autoplay", title: "Autoplay off", state: "ok", detail: "Nothing starts by itself after a song", fix: null },
];

// What Echolot sent so far: 9 of 24 songs answered.
const session: EcholotSession = {
  title: "80s Classics · Not lossless",
  state: "running",
  reason: "",
  total: 24,
  done: 9,
  filed: 7,
  not_better: 1,
  skipped: 1,
  failed: 0,
  minutes_left: 59,
  seconds_total: 5_712,
  seconds_done: 2_131,
};
let snapshot: RecorderSnapshot = {
  connection: "connected",
  user: "anna",
  host: "echolot.example.org",
  device: "STUDIO-PC",
  state: "recording",
  detail: "",
  session,
  current: rick,
  uploads: 0,
  error: null,
};

const mb = (minutes: number, seconds: number, bits: 16 | 24) => Math.round((minutes * 60 + seconds) * (bits === 16 ? 112_000 : 168_000));
const day = 86_400_000;
const record = (daysAgo: number, artist: string, title: string, length: [number, number], bits: 16 | 24, echolot: string | null, exists = true, faded = false): HistoryRecord => ({
  at: Date.now() - daysAgo * day,
  track: { title, artist, album: "", duration_ms: (length[0] * 60 + length[1]) * 1000 },
  file: `${out}\\${artist} - ${title}.flac`,
  size: mb(length[0], length[1], bits),
  verdict: faded ? "altered" : "info",
  master: faded ? "bit16_partly" : bits === 16 ? "bit16_exact" : "bit24",
  exact: !faded,
  faded,
  echolot: echolot !== null,
  result: echolot,
  exists,
});
const history: HistoryRecord[] = [
  record(1, "Tears for Fears", "Everybody Wants to Rule the World", [4, 11], 16, "filed"),
  record(1, "Eurythmics", "Sweet Dreams (Are Made of This)", [3, 36], 16, "filed", true, true),
  record(1, "Talk Talk", "It's My Life", [3, 53], 16, "not_better", false),
  record(2, "Daft Punk", "Digital Love", [5, 1], 24, null),
  record(3, "Kraftwerk", "Computer Love", [7, 15], 24, null),
  record(6, "New Order", "Blue Monday", [7, 29], 16, "filed"),
];

// This run so far, oldest first: [minutes ago, track, Echolot's answer].
const earlierSongs: [number, Track, string][] = [
  [18, { title: "Take On Me", artist: "a-ha", album: "Hunting High and Low", duration_ms: 225_280 }, "filed"],
  [14, { title: "Kids in America", artist: "Kim Wilde", album: "Kim Wilde", duration_ms: 206_000 }, "filed"],
  [10, { title: "Don't You (Forget About Me)", artist: "Simple Minds", album: "Don't You (Forget About Me)", duration_ms: 261_000 }, "not_better"],
  [5, { title: "Smalltown Boy", artist: "Bronski Beat", album: "The Age of Consent", duration_ms: 302_000 }, "filed"],
];

const resultDetail: Record<string, string> = { filed: "added to the library", not_better: "the library copy is as good" };

function start() {
  emit("listening", { app: "Spotify", device: "CABLE Input (VB-Audio Virtual Cable)", rate: 44_100 });
  for (const [ago, track, result] of earlierSongs) {
    earlier(ago, () => {
      emit("update", { kind: "saved", track, file: `${out}\\${track.artist} - ${track.title}.flac`, size: Math.round((track.duration_ms / 1000) * 112_000), verdict: "info", master: "bit16_exact", exact: true, faded: false, issues: [], echolot: true });
      emit("echolot", { kind: "song", job: track, result, detail: resultDetail[result] });
    });
  }
  emit("update", { kind: "monitor_volume", volume: 0.42 });
  emit("echolot", { kind: "snapshot", snapshot });

  // The song plays on: 1:27 in, levels of a loud pop master.
  let recorded = 87_000;
  let last = performance.now();
  const smooth = [0.25, 0.25];
  setInterval(() => {
    const now = performance.now();
    recorded = (recorded + (now - last)) % rick.duration_ms;
    last = now;
    const beat = Math.pow(Math.max(0, Math.sin((recorded / 60_000) * 113 * Math.PI)), 6);
    const level = [0, 1].map((c) => {
      smooth[c] += (0.18 + 0.1 * beat + 0.05 * Math.random() - smooth[c]) * 0.35;
      return smooth[c];
    });
    const status: Status = {
      phase: "recording",
      track: rick,
      recorded_ms: recorded,
      level: [Math.min(0.97, level[0] * 2.6 + 0.1 * beat), Math.min(0.97, level[1] * 2.5 + 0.1 * beat)],
      rms: [level[0], level[1]],
      grid: { bit24: 1 },
    };
    emit("update", { kind: "status", status });
  }, 50);
}

function setSession(state: EcholotSession["state"]) {
  snapshot = { ...snapshot, session: { ...session, state, reason: state === "paused" ? "by_user" : "" } };
  emit("echolot", { kind: "snapshot", snapshot });
}

const commands: Record<string, (args: Record<string, unknown>) => unknown> = {
  sources: () => [{ id: "spotify", name: "Spotify", running: true, playing: true, title: rick.title, artist: rick.artist }],
  platform: () => ({ os: "windows", monitor: true, media_keys: true, gain: 0.5 }),
  get_settings: () => settings,
  save_settings: (a) => Object.assign(settings, a.settings),
  health: () => checks,
  history: () => history,
  echolot_state: () => ({ snapshot, url: settings.echolot_url, pairing: false }),
  echolot_session: (a) => setSession(a.action === "pause" ? "paused" : a.action === "stop" ? "stopped" : "running"),
  set_monitor_volume: (a) => emit("update", { kind: "monitor_volume", volume: a.volume }),
  start: () => ({ app: "Spotify", device: "CABLE Input (VB-Audio Virtual Cable)", rate: 44_100 }),
  stop: () => emit("update", { kind: "stopped", error: null }),
};

export async function invoke<T>(cmd: string, args: Record<string, unknown> = {}): Promise<T> {
  return (commands[cmd]?.(args) ?? null) as T;
}

export async function listen<T>(event: string, handler: (event: { payload: T }) => void) {
  const set = handlers.get(event) ?? new Set();
  set.add(handler as Handler);
  handlers.set(event, set);
  return () => set.delete(handler as Handler);
}

export async function getVersion(): Promise<string> {
  return "0.4.3";
}

// Once both stores have subscribed.
setTimeout(start, 300);

// `?click=Setup&click=History`: click those buttons (by their text) once the demo runs, for screenshots of them.
setTimeout(async () => {
  for (const text of new URLSearchParams(location.search).getAll("click")) {
    const target = [...document.querySelectorAll<HTMLElement>("button, summary")].find((e) => e.textContent?.trim().toLowerCase().startsWith(text.toLowerCase()));
    target?.click();
    await new Promise((done) => setTimeout(done, 200));
  }
}, 600);

// `?shot`: for a headless screenshot, whose clock runs ahead of CSS transitions (they would stay at their start).
if (new URLSearchParams(location.search).has("shot")) {
  const style = document.createElement("style");
  style.textContent = "*, *::before, *::after { transition: none !important; animation: none !important; }";
  document.head.append(style);
}
