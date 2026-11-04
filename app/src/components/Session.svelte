<script lang="ts">
  import { api, readable } from "../lib/api";
  import { facts, fileCard, headline, quality, songFacts, time, type Card, type Quality } from "../lib/quality";
  import { session, type Entry } from "../lib/session.svelte";
  import Panel from "./Panel.svelte";
  import TrackRow from "./TrackRow.svelte";

  function summary(e: Entry): Quality {
    if (e.outcome === "exists") return { tone: "muted", tag: "have", text: "Already have it" };
    if (e.outcome !== "saved") return e.outcome === "dropped" ? { tone: "muted", tag: "skip", text: headline(e.message) } : { tone: "bad", tag: "fail", text: headline(e.message) };
    const q = quality(e.verdict, e.master, e.exact, e.faded, e.lossy, e.file, e.issues, e.format);
    return e.file === null ? { ...q, text: `${q.text} · removed` } : q;
  }

  function card(e: Entry): Card {
    if (e.outcome === "saved") return fileCard(e, e.at, e.echolot ? "Echolot" : "Manual");
    if (e.outcome === "exists") return { facts: [...songFacts(e.track, e.at), ["File", e.file]], findings: [] };
    return { facts: songFacts(e.track, e.at), findings: [readable(e.message, true)] };
  }

  const saved = $derived(session.history.filter((e) => e.outcome === "saved" && e.file && e.verdict !== "corrupt").length);
</script>

<Panel label="Session">
  {#snippet actions()}
    {#if saved}<span class="count mono muted">{saved} saved</span>{/if}
    <button class="btn" onclick={() => session.settings && api.open(session.settings.out_dir)}>Open folder</button>
  {/snippet}

  {#if session.history.length === 0}
    <p class="muted empty">This run's songs appear here. Earlier ones are in the Library.</p>
  {:else}
    <ul>
      {#each session.history as e (e.id)}
        {@const file = e.outcome === "saved" || e.outcome === "exists" ? e.file : null}
        <TrackRow
          track={e.track}
          q={summary(e)}
          at={e.at}
          whenText={time(e.at)}
          card={card(e)}
          cover={session.coverOf(e.track)}
          {file}
          note={e.outcome === "saved" && e.echolot ? "for Echolot" : null}
          facts={e.outcome === "saved" && e.file ? facts(e.track.duration_ms, e.size) : null} />
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

  .count {
    font-size: 11px;
    letter-spacing: 0.08em;
    text-transform: uppercase;
  }
</style>
