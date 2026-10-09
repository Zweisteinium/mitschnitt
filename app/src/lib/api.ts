import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

export type Track = { title: string; artist: string; album: string; duration_ms: number };
export type Phase = "idle" | "waiting" | "recording";
/** Share of the audio arriving as exact 24-bit / 16-bit values (1 = untouched lossless source). */
export type Grid = { bit24: number; bit16: number };
export type Status = { phase: Phase; track: Track | null; recorded_ms: number; level: [number, number]; grid: Grid | null };
export type Severity = "info" | "altered" | "corrupt";
export type Master = "bit16_exact" | "bit16_partly" | "bit24";

export type Update =
  | { kind: "status"; status: Status }
  | { kind: "started"; track: Track }
  | { kind: "dropped"; track: Track; reason: string; message: string }
  | { kind: "exists"; track: Track; file: string }
  | { kind: "saved"; track: Track; file: string | null; verdict: Severity; master: Master; exact: boolean; issues: string[] }
  | { kind: "failed"; track: Track; error: string }
  | { kind: "stopped"; error: string | null };

export type Source = { id: string; name: string; running: boolean; playing: boolean; title: string; artist: string };
export type SourceInfo = { app: string; device: string; rate: number };

export type Format = "flac" | "wav" | "mp3";
export type BitDepth = "auto" | "b16" | "b24";
export type Settings = {
  out_dir: string;
  format: Format;
  bit_depth: BitDepth;
  template: string;
  keep_flagged: boolean;
  skip_existing: boolean;
  monitor: boolean;
  monitor_volume: number;
};

export type CheckState = "ok" | "warn" | "bad" | "check";
export type Check = { id: string; title: string; state: CheckState; detail: string; fix: string | null };

export const api = {
  sources: () => invoke<Source[]>("sources"),
  start: (app: string) => invoke<SourceInfo>("start", { app }),
  stop: () => invoke<void>("stop"),
  stopAfterTrack: () => invoke<void>("stop_after_track"),
  release: () => invoke<void>("release"),
  getSettings: () => invoke<Settings>("get_settings"),
  saveSettings: (settings: Settings) => invoke<void>("save_settings", { settings }),
  health: (app: string) => invoke<Check[]>("health", { app }),
  fix: (action: string, app: string) => invoke<string>("fix", { action, app }),
  open: (target: string) => invoke<void>("open", { target }),
  onUpdate: (handler: (u: Update) => void): Promise<UnlistenFn> => listen<Update>("update", (e) => handler(e.payload)),
};

export function clock(ms: number): string {
  const s = Math.floor(ms / 1000);
  return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, "0")}`;
}

