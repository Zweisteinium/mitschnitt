import { api, type Job, type RecorderSnapshot } from "./api";
import { session } from "./session.svelte";

export type SongResult = { id: number; at: Date; job: Job; result: string; detail: string };

/** What became of a song, in a word. `stalled` and `failed` are the same to Echolot: it tries once more. */
export const resultText: Record<string, string> = {
  filed: "added",
  not_better: "not better",
  rejected: "rejected",
  recorded: "recorded",
  lossy_only: "no lossless",
  wrong_song: "wrong song",
  took_over: "taken over",
  stalled: "failed",
  failed: "failed",
};

const empty: RecorderSnapshot = { connection: "", user: "", host: "", device: "", state: null, detail: "", session: null, current: null, uploads: 0, error: null, done: 0, total: 0 };

/** Echolot: connection, the session it runs on this PC, and what became of each song. */
class Echolot {
  snapshot = $state<RecorderSnapshot>(empty);
  /** The address to connect to. */
  url = $state("");
  /** A sign-in waits for the approval in the browser. */
  pairing = $state(false);
  /** Started from Echolot's account page: confirm the address first. */
  confirming = $state(false);
  code = $state("");
  error = $state<string | null>(null);
  busy = $state(false);
  results = $state<SongResult[]>([]);
  #nextId = 0;

  get connected(): boolean {
    return this.snapshot.connection === "connected" || this.snapshot.connection === "offline";
  }

  async init() {
    await api.onEcholot((u) => {
      if (u.kind === "snapshot") {
        this.snapshot = u.snapshot;
        if (this.connected) this.pairing = false;
      } else {
        this.results = [{ id: this.#nextId++, at: new Date(), job: u.job, result: u.result, detail: u.detail }, ...this.results].slice(0, 100);
      }
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

  /** Mitschnitt's own Stop / Stop after track: a running session pauses first. */
  async pauseForStop() {
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
