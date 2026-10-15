import { api, type Check, type Lossy, type Master, type Platform, type Settings, type Severity, type Source, type SourceInfo, type Status, type Track, type Update } from "./api";

type Outcome =
  | { outcome: "saved"; file: string | null; size: number; verdict: Severity; master: Master; exact: boolean; faded: boolean; lossy: Lossy | null; issues: string[]; echolot: boolean; frames: number; peak_dbfs: number | null }
  | { outcome: "exists"; file: string }
  | { outcome: "dropped" | "failed"; message: string };

export type Entry = { id: number; at: Date; track: Track } & Outcome;

export type Tab = "listen" | "setup" | "settings";

class Session {
  tab = $state<Tab>("listen");
  sources = $state<Source[]>([]);
  selected = $state<string | null>(null);
  source = $state<SourceInfo | null>(null);
  status = $state<Status | null>(null);
  history = $state<Entry[]>([]);
  settings = $state<Settings | null>(null);
  platform = $state<Platform | null>(null);
  checks = $state<Check[]>([]);
  checking = $state(false);
  fixing = $state<string | null>(null);
  notice = $state<string | null>(null);
  error = $state<string | null>(null);
  busy = $state(false);
  /** "Stop after track" was requested. */
  stopping = $state(false);
  /** When a media key was last swallowed (shown briefly). */
  keyBlockedAt = $state<number | null>(null);
  monitorMuted = $state(false);
  /** Null until the monitor reports it. */
  monitorVolume = $state<number | null>(null);
  /** Files older than this are history, newer ones are in this run's list. */
  readonly startedAt = Date.now();
  #nextId = 0;
  #polling = false;

  get listening(): boolean {
    return this.source !== null;
  }

  get problems(): number {
    return this.checks.filter((c) => c.state === "bad").length;
  }

  async init() {
    await api.onUpdate((u) => this.#apply(u));
    // Echolot started listening for its songs.
    await api.onListening((info) => (this.source = info));
    this.source ??= await api.listening();
    this.platform = await api.platform();
    this.settings = await api.getSettings();
    await this.#try(async () => this.#setSources(await api.sources()));
    await this.check();
    setInterval(() => void this.#poll(), 1000);
  }

  async #poll() {
    if (this.#polling || this.listening || this.tab !== "listen" || document.hidden) return;
    this.#polling = true;
    try {
      this.#setSources(await api.sources());
    } catch {}
    this.#polling = false;
  }

  #setSources(sources: Source[]) {
    this.sources = sources;
    if (!sources.find((s) => s.id === this.selected)?.running) {
      this.selected = (sources.find((s) => s.running) ?? sources.find((s) => s.id === this.selected) ?? sources[0])?.id ?? null;
    }
  }

  async check() {
    this.checking = true;
    await this.#try(async () => {
      this.checks = await api.health(this.selected ?? "spotify");
    });
    this.checking = false;
  }

  async fix(action: string) {
    this.fixing = action;
    await this.#try(async () => {
      this.notice = await api.fix(action, this.selected ?? "spotify");
    });
    this.fixing = null;
    await this.check();
  }

  async start() {
    const app = this.selected;
    if (!app) return;
    this.busy = true;
    await this.#try(async () => {
      this.source = await api.start(app);
    });
    this.busy = false;
  }

  async stop() {
    this.busy = true;
    await this.#try(() => api.stop());
    this.busy = false;
  }

  setMonitorVolume(volume: number) {
    this.monitorVolume = volume;
    void this.#try(() => api.setMonitorVolume(volume));
  }

  async unmuteMonitor() {
    await this.#try(() => api.unmuteMonitor());
  }

  async stopAfterTrack() {
    this.stopping = true;
    await this.#try(() => api.stopAfterTrack());
  }

  async save() {
    await this.#try(() => api.saveSettings($state.snapshot(this.settings!)));
  }

  async #try(action: () => Promise<void>) {
    this.error = null;
    try {
      await action();
    } catch (e) {
      this.error = String(e);
    }
  }

  #apply(u: Update) {
    switch (u.kind) {
      case "status":
        this.status = u.status;
        break;
      case "saved":
        this.#log(u.track, { outcome: "saved", file: u.file, size: u.size, verdict: u.verdict, master: u.master, exact: u.exact, faded: u.faded, lossy: u.lossy, issues: u.issues, echolot: u.echolot, frames: u.frames, peak_dbfs: u.peak_dbfs });
        break;
      case "key_blocked": {
        const at = Date.now();
        this.keyBlockedAt = at;
        setTimeout(() => {
          if (this.keyBlockedAt === at) this.keyBlockedAt = null;
        }, 5000);
        break;
      }
      case "monitor_muted":
        this.monitorMuted = u.muted;
        break;
      case "monitor_volume":
        this.monitorVolume = u.volume;
        break;
      case "exists":
        this.#log(u.track, { outcome: "exists", file: u.file });
        break;
      case "dropped":
        this.#log(u.track, { outcome: "dropped", message: u.message });
        break;
      case "failed":
        this.#log(u.track, { outcome: "failed", message: u.error });
        break;
      case "stopped":
        this.source = null;
        this.status = null;
        this.stopping = false;
        this.monitorMuted = false;
        this.monitorVolume = null;
        if (u.error) this.error = u.error;
        void api.release(); // the engine may have stopped by itself
        break;
    }
  }

  /** Echolot's answer moved a recording out of the waiting folder (`to`: null when it was deleted). */
  moved(from: string, to: string | null) {
    this.history = this.history.map((e) => (e.outcome === "saved" && e.file === from ? { ...e, file: to } : e));
  }

  #log(track: Track, outcome: Outcome) {
    this.history = [{ id: this.#nextId++, at: new Date(), track, ...outcome }, ...this.history].slice(0, 200);
  }
}

export const session = new Session();
