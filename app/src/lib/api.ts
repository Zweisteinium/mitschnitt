import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

export type Track = { title: string; artist: string; album: string; duration_ms: number };
export type Phase = "idle" | "waiting" | "recording";
/** Share of the audio arriving as exact 24-bit / 16-bit values (1 = untouched lossless source). */
export type Grid = { bit24: number; bit16: number };
export type Status = { phase: Phase; track: Track | null; recorded_ms: number; level: [number, number]; rms: [number, number]; grid: Grid | null };
export type Severity = "info" | "altered" | "corrupt";
export type Master = "bit16_exact" | "bit16_partly" | "bit24";

export type Update =
  | { kind: "status"; status: Status }
  | { kind: "started"; track: Track }
  | { kind: "dropped"; track: Track; reason: string; message: string }
  | { kind: "exists"; track: Track; file: string }
  | { kind: "saved"; track: Track; file: string | null; size: number; verdict: Severity; master: Master; exact: boolean; issues: string[]; echolot: boolean }
  | { kind: "failed"; track: Track; error: string }
  | { kind: "key_blocked"; count: number }
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

/** What this system offers; the UI shows only what works here. */
export type Platform = { os: string; monitor: boolean; media_keys: boolean; gain: number };

export type CheckState = "ok" | "warn" | "bad" | "check";
export type Check = { id: string; title: string; state: CheckState; detail: string; fix: string | null };

/** A song Echolot (or an exported list) wants recorded. */
export type Job = { id: number | null; uri: string; artist: string; artists: string[]; title: string; album: string; duration_ms: number; isrc: string };
export type EcholotSession = {
  id: number;
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
  /** Length of all songs, and of those answered for (not the one being recorded). */
  seconds_total: number;
  seconds_done: number;
};
export type DeviceState = "idle" | "busy" | "recording" | "setup";
export type RecorderSnapshot = {
  /** "connected", "offline", "disconnected", "file" (an exported list runs) or "" (nothing). */
  connection: string;
  user: string;
  host: string;
  device: string;
  state: DeviceState | null;
  detail: string;
  session: EcholotSession | null;
  current: Job | null;
  uploads: number;
  error: string | null;
  done: number;
  total: number;
};
export type RecorderUpdate = { kind: "snapshot"; snapshot: RecorderSnapshot } | { kind: "song"; job: Job; result: string; detail: string };
export type CardState = { snapshot: RecorderSnapshot; url: string; pairing: boolean };
/** A file Mitschnitt wrote (any run). `at`: Unix ms. `result`: Echolot's answer to the upload. */
export type HistoryRecord = { at: number; track: Track; file: string; size: number; verdict: Severity; master: Master; exact: boolean; echolot: boolean; result: string | null; exists: boolean };

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
  platform: () => invoke<Platform>("platform"),
  history: () => invoke<HistoryRecord[]>("history"),
  echolotState: () => invoke<CardState>("echolot_state"),
  echolotConnect: (url: string) => invoke<void>("echolot_connect", { url }),
  echolotCode: (code: string) => invoke<void>("echolot_code", { code }),
  echolotCancel: () => invoke<void>("echolot_cancel"),
  echolotDisconnect: () => invoke<void>("echolot_disconnect"),
  echolotSession: (action: "pause" | "resume" | "stop") => invoke<void>("echolot_session", { action }),
  onEcholot: (handler: (u: RecorderUpdate) => void): Promise<UnlistenFn> => listen<RecorderUpdate>("echolot", (e) => handler(e.payload)),
  onEcholotConnect: (handler: (url: string) => void): Promise<UnlistenFn> => listen<string>("echolot-connect", (e) => handler(e.payload)),
  onEcholotError: (handler: (error: string) => void): Promise<UnlistenFn> => listen<string>("echolot-error", (e) => handler(e.payload)),
  onListening: (handler: (info: SourceInfo) => void): Promise<UnlistenFn> => listen<SourceInfo>("listening", (e) => handler(e.payload)),
  onUpdate: (handler: (u: Update) => void): Promise<UnlistenFn> => listen<Update>("update", (e) => handler(e.payload)),
};

/** File size as Explorer counts it (1 MB = 1024 KB). */
export function megabytes(bytes: number): string {
  return `${(bytes / 1048576).toFixed(1)} MB`;
}

export function clock(ms: number): string {
  const s = Math.floor(ms / 1000);
  return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, "0")}`;
}

