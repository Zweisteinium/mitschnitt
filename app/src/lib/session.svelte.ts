import { api, type Check, type Master, type Platform, type Settings, type Severity, type Source, type SourceInfo, type Status, type Track, type Update } from "./api";

export type Outcome =
  | { outcome: "saved"; file: string | null; verdict: Severity; master: Master; exact: boolean; issues: string[] }
  | { outcome: "exists"; file: string }
  | { outcome: "dropped" | "failed"; message: string };

export type Entry = { id: number; at: Date; track: Track } & Outcome;

export type Tab = "listen" | "setup" | "settings";

/** App state: sources, the running listener, its results, settings and setup health. */
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
  /** "Stop after this track" was requested. */
  stopping = $state(false);
  /** When a media key was last swallowed to protect the recording (shown briefly). */
  keyBlockedAt = $state<number | null>(null);
  /** The monitor muted itself because the output device changed. */
  monitorMuted = $state(false);
  /** Monitor volume = Mitschnitt's Windows mixer entry; null until the monitor reports it. */
  monitorVolume = $state<number | null>(null);
  #nextId = 0;

  get listening(): boolean {
    return this.source !== null;
  }

  /** Problems that block a clean recording (shown as a hint on the Listen tab). */
  get problems(): number {
    return this.checks.filter((c) => c.state === "bad").length;
  }

  async init() {
    await api.onUpdate((u) => this.#apply(u));
    // Echolot started listening for its songs.
    await api.onListening((info) => (this.source = info));
    this.platform = await api.platform();
    this.settings = await api.getSettings();
    await this.refresh();
    await this.check();
  }

  async refresh() {
    await this.#try(async () => {
      this.sources = await api.sources();
      if (!this.sources.some((s) => s.id === this.selected)) {
        this.selected = (this.sources.find((s) => s.running) ?? this.sources[0])?.id ?? null;
      }
    });
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
    if (!this.selected) return;
    this.busy = true;
    await this.#try(async () => {
      this.source = await api.start(this.selected!);
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

  /** Persist settings (call after changing a field). */
  async save() {
    if (this.settings) await this.#try(() => api.saveSettings($state.snapshot(this.settings!)));
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
        this.#log(u.track, { outcome: "saved", file: u.file, verdict: u.verdict, master: u.master, exact: u.exact, issues: u.issues });
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

  #log(track: Track, outcome: Outcome) {
    this.history = [{ id: this.#nextId++, at: new Date(), track, ...outcome }, ...this.history].slice(0, 200);
  }
}

export const session = new Session();
