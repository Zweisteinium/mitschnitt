import { clock, megabytes, readable, type Lossy, type Master, type Severity, type Track } from "./api";

export type Tone = "ok" | "lossy" | "warn" | "bad" | "muted";
export type Quality = { tone: Tone; tag: string; text: string };
export type Card = { facts: [string, string][]; findings: string[] };

export const time = (d: Date) => d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
export const full = (d: Date) => d.toLocaleString([], { dateStyle: "medium", timeStyle: "short" });
const exactTime = (d: Date) => d.toLocaleString([], { dateStyle: "medium", timeStyle: "medium" });
export const when = (d: Date) => (d.toDateString() === new Date().toDateString() ? time(d) : d.toLocaleDateString([], { day: "numeric", month: "short" }));

/** A finding's gist for the row: up to its details ("end not verified: no pause …" -> "End not verified"). */
export function headline(issue: string | undefined): string {
  const gist = readable((issue ?? "").split(/: | \(/)[0]);
  return gist.charAt(0).toUpperCase() + gist.slice(1);
}

const bits = (master: Master) => (master === "bit24" ? "24-bit" : "16-bit");

function format(file: string | null): string | null {
  const ext = file?.split(".").pop()?.toLowerCase();
  return !ext ? null : ext === "mp3" ? "MP3 320" : ext.toUpperCase();
}

/** Every saved song reads the same way: what arrived, the master's bit depth, the file's format. A lossy stream has no
 * master depth to tell, a damaged take only its damage. */
export function quality(verdict: Severity, master: Master, exact: boolean, faded: boolean, lossy: Lossy | null, file: string | null, issues: string[]): Quality {
  if (verdict === "corrupt") return { tone: "bad", tag: "bad", text: issues.length ? `Damaged · ${headline(issues[0])}` : "Damaged" };
  const text = (what: string, depth = true) => [what, depth && !file?.endsWith(".mp3") ? bits(master) : null, format(file)].filter(Boolean).join(" · ");
  if (lossy === "stream") return { tone: "lossy", tag: "lossy", text: text("Lossy stream", false) };
  if (lossy) return { tone: "lossy", tag: "lossy", text: text("Lossy master") };
  if (faded) return { tone: "ok", tag: "fine", text: text("Fade-in, rest bit-exact") };
  if (verdict === "altered") return { tone: "warn", tag: "alt", text: text(issues.length ? headline(issues[0]) : "Not fully original") };
  return exact ? { tone: "ok", tag: "exact", text: text("Bit-exact") } : { tone: "ok", tag: "fine", text: text("Clean") };
}

export const facts = (durationMs: number, size: number) => `${clock(durationMs)} · ${megabytes(size)}`;

export type Saved = { track: Track; file: string | null; size: number; master: Master; exact: boolean; faded: boolean; lossy: Lossy | null; issues: string[]; frames: number | null; peak_dbfs: number | null };

/** A card line, left out without a value. */
const fact = (label: string, value: string | null | undefined): [string, string][] => (value ? [[label, value]] : []);

export const songFacts = (track: Track, at: Date): [string, string][] => [
  ...fact("Album", track.album),
  ...fact("Length", `${clock(track.duration_ms)}.${Math.floor((track.duration_ms % 1000) / 100)} listed`),
  ...fact("Time", exactTime(at)),
];

/** Everything about a file, for its info card. */
export function fileCard(f: Saved, at: Date, origin: string): Card {
  const ext = f.file?.split(".").pop()?.toUpperCase();
  const facts = [
    ...fact("Origin", origin),
    ...fact("Master", f.lossy === "source" ? "made from a lossy file" : f.master === "bit16_exact" ? "16-bit" : f.master === "bit16_partly" ? "16-bit, altered in places" : "24-bit"),
    ...fact("Stream", f.exact ? "every sample arrived untouched" : f.faded ? "untouched after Spotify's fade-in" : f.lossy === "stream" ? "lossy" : "not every sample proven untouched"),
    ...fact("Format", ext && (ext === "MP3" ? "MP3, 320 kbps" : `${ext}, ${f.master === "bit16_exact" ? 16 : 24}-bit`)),
    ...songFacts(f.track, at),
    ...fact("Samples", f.frames ? `${f.frames.toLocaleString()} per channel` : null),
    ...fact("Peak", f.peak_dbfs !== null ? `${f.peak_dbfs.toFixed(2)} dBFS` : null),
    ...fact("Size", f.size ? `${(f.size / 1048576).toFixed(2)} MB, ${Math.round((f.size * 8) / f.track.duration_ms)} kbps` : null),
    ...fact("File", f.file),
  ];
  return { facts, findings: f.issues.map((i) => readable(i, true)) };
}
