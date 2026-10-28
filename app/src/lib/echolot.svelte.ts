import { api, type RecorderSnapshot, type RecorderUpdate, type Track } from "./api";
import { session } from "./session.svelte";

type SongResult = { id: number; at: Date; job: Track; result: string; detail: string };

/** `stalled` and `failed` are the same to Echolot: it tries once more. */
export const resultText: Record<string, string> = {
  filed: "added",
  not_better: "not better",
  rejected: "rejected",
  recorded: "recorded",
  lossy_only: "no lossless",
  wrong_song: "wrong song",
  took_over: "taken over",
  crossfade: "crossfade on",
  skipped: "skipped by you",
  stalled: "failed",
  failed: "failed",
};

const empty: RecorderSnapshot = { connection: "", user: "", host: "", device: "", state: null, detail: "", session: null, current: null, uploads: 0, error: null, done: 0, total: 0, paused: false, lagging: false };

class Echolot {
  snapshot = $state<RecorderSnapshot>(empty);
  url = $state("");
  /** A sign-in waits for the approval in the browser. */
  pairing = $state(false);
  /** Started from Echolot's account page: confirm the address first. */
  confirming = $state(false);
  code = $state("");
  error = $state<string | null>(null);
  busy = $state(false);
  results = $state<SongResult[]>([]);
  list = $state<RecorderSnapshot | null>(null);
  listResults = $state<SongResult[]>([]);
  #nextId = 0;

  get connected(): boolean {
    return this.snapshot.connection === "connected" || this.snapshot.connection === "offline";
  }

  get listing(): boolean {
    return this.list?.connection === "file";
  }

  /** Running, paused, or a song still being finished: the card has the controls then. */
  get active(): boolean {
    const state = this.snapshot.session?.state;
    return this.connected && (state === "running" || state === "paused" || this.snapshot.current !== null);
  }

  async init() {
    await api.onEcholot((u) => {
      if (u.kind === "snapshot") {
        this.snapshot = u.snapshot;
        if (this.connected) this.pairing = false;
      } else {
        if (u.moved) session.moved(...u.moved);
        this.results = [this.#result(u), ...this.results].slice(0, 100);
      }
    });
    await api.onEcholotList((u) => {
      if (u.kind === "snapshot") this.list = u.snapshot;
      else this.listResults = [this.#result(u), ...this.listResults].slice(0, 100);
    });
    await api.onEcholotConnect((url) => {
      this.url = url;
      this.confirming = true;
      session.tab = "setup";
    });
    await api.onEcholotError((e) => {
      this.error = e;
      this.busy = false;
    });
    const state = await api.echolotState();
    this.snapshot = state.snapshot;
    this.list = state.list;
    this.url = state.url;
    this.pairing = state.pairing;
  }

  async connect() {
    this.confirming = false;
    await this.#try(async () => {
      await api.echolotConnect(this.url);
      this.pairing = true;
    });
  }

  async submitCode() {
    if (!this.code.trim()) return;
    await this.#try(async () => {
      await api.echolotCode(this.code);
      this.code = "";
      this.pairing = false;
    });
  }

  async cancel() {
    this.confirming = false;
    this.pairing = false;
    await this.#try(() => api.echolotCancel());
  }

  async disconnect() {
    await this.#try(() => api.echolotDisconnect());
  }

  async session(action: "pause" | "resume" | "stop") {
    await this.#try(() => api.echolotSession(action));
  }

  async skip() {
    await this.#try(() => api.echolotSkip());
  }

  async recordList(file: File) {
    await this.#try(async () => {
      this.listResults = [];
      await api.echolotList(await file.text());
      session.tab = "listen";
    });
  }

  async listControl(action: "skip" | "pause" | "resume" | "stop") {
    await this.#try(() => api.echolotListControl(action));
  }

  #result(u: Extract<RecorderUpdate, { kind: "song" }>): SongResult {
    return { id: this.#nextId++, at: new Date(), job: u.job, result: u.result, detail: u.detail };
  }

  /** Mitschnitt's own Stop: a running session pauses first, a list ends. */
  async pauseForStop() {
    if (this.listing) await this.listControl("stop");
    if (this.connected && this.snapshot.session?.state === "running") await this.session("pause");
  }

  async #try(action: () => Promise<void>) {
    this.error = null;
    this.busy = true;
    try {
      await action();
    } catch (e) {
      this.error = String(e);
    }
    this.busy = false;
  }
}

export const echolot = new Echolot();
