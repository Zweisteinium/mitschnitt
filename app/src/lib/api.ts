import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

export type Track = { title: string; artist: string; album: string; duration_ms: number };
/** `grid.bit24`: share of the audio arriving as exact 24-bit values (1 = untouched lossless source). */
export type Status = { phase: "idle" | "waiting" | "recording"; track: Track | null; recorded_ms: number; level: [number, number]; rms: [number, number]; grid: { bit24: number } | null };
export type Severity = "info" | "altered" | "corrupt";
export type Master = "bit16_exact" | "bit16_partly" | "bit24";

export type Update =
  | { kind: "status"; status: Status }
  | { kind: "started"; track: Track }
  | { kind: "dropped"; track: Track; message: string }
  | { kind: "exists"; track: Track; file: string }
  | { kind: "saved"; track: Track; file: string | null; size: number; verdict: Severity; master: Master; exact: boolean; faded: boolean; issues: string[]; echolot: boolean }
  | { kind: "failed"; track: Track; error: string }
  | { kind: "key_blocked" }
  | { kind: "monitor_muted"; muted: boolean }
  | { kind: "monitor_volume"; volume: number }
  | { kind: "stopped"; error: string | null };

export type Source = { id: string; name: string; running: boolean; playing: boolean; title: string; artist: string };
export type SourceInfo = { app: string; device: string; rate: number };

export type Format = "flac" | "wav" | "mp3";
export type Settings = {
  out_dir: string;
  format: Format;
  template: string;
  keep_flagged: boolean;
  skip_existing: boolean;
  protect_media_keys: boolean;
  monitor: boolean;
  keep_echolot_copies: boolean;
  echolot_url: string;
};

export type Platform = { os: string; monitor: boolean; media_keys: boolean; gain: number };

export type CheckState = "ok" | "warn" | "bad" | "check";
export type Check = { id: string; title: string; state: CheckState; detail: string; fix: string | null };

export type EcholotSession = {
  title: string;
  state: "running" | "paused" | "done" | "stopped";
  reason: string;
  total: number;
  done: number;
  filed: number;
  not_better: number;
  skipped: number;
  failed: number;
  minutes_left: number;
  /** `seconds_done` excludes the song being recorded. */
  seconds_total: number;
  seconds_done: number;
};
export type RecorderSnapshot = {
  /** "file": an exported list runs. */
  connection: "connected" | "offline" | "disconnected" | "file" | "";
  user: string;
  host: string;
  device: string;
  state: "idle" | "busy" | "recording" | "setup" | null;
  detail: string;
  session: EcholotSession | null;
  current: Track | null;
  uploads: number;
  error: string | null;
};
type RecorderUpdate = { kind: "snapshot"; snapshot: RecorderSnapshot } | { kind: "song"; job: Track; result: string; detail: string };
type CardState = { snapshot: RecorderSnapshot; url: string; pairing: boolean };
/** `at`: Unix ms. `result`: Echolot's answer to the upload. */
export type HistoryRecord = { at: number; track: Track; file: string; size: number; verdict: Severity; master: Master; exact: boolean; faded: boolean; echolot: boolean; result: string | null; exists: boolean };

export const api = {
  sources: () => invoke<Source[]>("sources"),
  start: (app: string) => invoke<SourceInfo>("start", { app }),
  stop: () => invoke<void>("stop"),
  stopAfterTrack: () => invoke<void>("stop_after_track"),
  release: () => invoke<void>("release"),
  unmuteMonitor: () => invoke<void>("unmute_monitor"),
  setMonitorVolume: (volume: number) => invoke<void>("set_monitor_volume", { volume }),
  getSettings: () => invoke<Settings>("get_settings"),
  saveSettings: (settings: Settings) => invoke<void>("save_settings", { settings }),
  health: (app: string) => invoke<Check[]>("health", { app }),
  fix: (action: string, app: string) => invoke<string>("fix", { action, app }),
  open: (target: string) => invoke<void>("open", { target }),
  openLog: () => invoke<void>("open_log"),
  /** With the default app for its type. */
  play: (file: string) => invoke<void>("play", { file }),
  platform: () => invoke<Platform>("platform"),
  history: () => invoke<HistoryRecord[]>("history"),
  echolotState: () => invoke<CardState>("echolot_state"),
  echolotConnect: (url: string) => invoke<void>("echolot_connect", { url }),
  echolotCode: (code: string) => invoke<void>("echolot_code", { code }),
  echolotCancel: () => invoke<void>("echolot_cancel"),
  echolotDisconnect: () => invoke<void>("echolot_disconnect"),
  echolotSession: (action: "pause" | "resume" | "stop") => invoke<void>("echolot_session", { action }),
  onEcholot: (handler: (u: RecorderUpdate) => void) => listen<RecorderUpdate>("echolot", (e) => handler(e.payload)),
  onEcholotConnect: (handler: (url: string) => void) => listen<string>("echolot-connect", (e) => handler(e.payload)),
  onEcholotError: (handler: (error: string) => void) => listen<string>("echolot-error", (e) => handler(e.payload)),
  onListening: (handler: (info: SourceInfo) => void) => listen<SourceInfo>("listening", (e) => handler(e.payload)),
  onUpdate: (handler: (u: Update) => void) => listen<Update>("update", (e) => handler(e.payload)),
};

/** File size as Explorer counts it (1 MB = 1024 KB). */
/** One decimal at most, none when it is 0: 24 MB, 22.8 MB, 44.1 kHz, 48 kHz. */
export function num(x: number): string {
  return String(Math.round(x * 10) / 10);
}

export function megabytes(bytes: number): string {
  return `${num(bytes / 1048576)} MB`;
}

export function clock(ms: number): string {
  const s = Math.floor(ms / 1000);
  return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, "0")}`;
}

