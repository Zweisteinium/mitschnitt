<script lang="ts">
  import { api, clock } from "../lib/api";
  import { session, type Entry } from "../lib/session.svelte";
  import Panel from "./Panel.svelte";

  const time = (d: Date) => d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });

  function summary(e: Entry): { tone: "ok" | "warn" | "bad" | "muted"; tag: string; text: string } {
    if (e.outcome === "exists") return { tone: "muted", tag: "have", text: "Already recorded" };
    if (e.outcome !== "saved") return e.outcome === "dropped" ? { tone: "muted", tag: "skip", text: e.message } : { tone: "bad", tag: "fail", text: e.message };
    const removed = e.file === null ? " · removed" : "";
    if (e.verdict === "corrupt") return { tone: "bad", tag: "bad", text: `Corrupt${removed}` };
    if (e.verdict === "altered") return { tone: "warn", tag: "alt", text: `Not fully original${removed}` };
    const master = e.master === "bit16_exact" ? "16-bit master" : "24-bit master";
    return { tone: "ok", tag: "ok", text: e.exact ? `Bit-exact · ${master}` : `Clean · ${master}` };
  }
</script>

<Panel label="Session" index="03">
  {#snippet actions()}
    <button class="btn" onclick={() => session.settings && api.open(session.settings.out_dir)}>Open folder</button>
  {/snippet}

  {#if session.history.length === 0}
    <p class="muted empty">Recorded and skipped tracks appear here.</p>
  {:else}
    <ul>
      {#each session.history as e (e.id)}
        {@const s = summary(e)}
        <li>
          <span class="tag mono {s.tone}">{s.tag}</span>
          <div class="main">
            <div class="name">{e.track.artist} – {e.track.title}</div>
            <div class="detail {s.tone}">{s.text}</div>
            {#if e.outcome === "saved" && e.issues.length}
              <ul class="issues">
                {#each e.issues as issue}<li>{issue}</li>{/each}
              </ul>
            {/if}
          </div>
          <div class="meta mono muted">
            <span>{clock(e.track.duration_ms)}</span>
            <span>{time(e.at)}</span>
          </div>
          {#if (e.outcome === "saved" || e.outcome === "exists") && e.file}
            <button class="link mono" onclick={() => api.open(e.file!)} title={e.file}>show</button>
          {/if}
        </li>
      {/each}
    </ul>
  {/if}
</Panel>

<style>
  .empty {
    margin: 0;
  }

  ul {
    list-style: none;
    margin: 0;
    padding: 0;
  }

  li {
    display: grid;
    grid-template-columns: 38px 1fr auto auto;
    gap: 12px;
    align-items: start;
    padding: 10px 0;
  }

  li + li {
    border-top: 1px solid var(--faint);
  }

  .tag {
    margin-top: 2px;
    padding: 1px 0;
    text-align: center;
    font-size: 10px;
    letter-spacing: 0.1em;
    text-transform: uppercase;
    border: 1px solid currentColor;
  }

  .tag.muted {
    color: var(--muted);
  }

  .main {
    min-width: 0;
  }

  .name {
    font-weight: 600;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .detail {
    font-size: 12px;
  }

  .issues li {
    display: block;
    padding: 0;
    border: none;
    font-size: 12px;
    color: var(--muted);
  }

  .meta {
    display: grid;
    justify-items: end;
    font-size: 11px;
  }

  .link {
    border: none;
    background: none;
    padding: 0;
    font-size: 11px;
    color: var(--cyan);
    text-transform: uppercase;
    letter-spacing: 0.1em;
  }
</style>
