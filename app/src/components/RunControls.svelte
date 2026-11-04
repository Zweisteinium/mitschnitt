<script lang="ts">
  import { echolot } from "../lib/echolot.svelte";
  import { session } from "../lib/session.svelte";

  const queue = $derived(echolot.active || echolot.listing);
</script>

<!-- During an Echolot session or a list, its card has the after-song controls. -->
<div class="controls">
  {#if !session.listening}
    <button class="btn primary" onclick={() => session.start()} disabled={!session.sources.find((s) => s.id === session.selected)?.running || session.busy}>
      {session.busy ? "Starting…" : "Start recording"}
    </button>
  {:else}
    {#if !queue}
      <button class="btn" onclick={() => session.stopAfterTrack()} disabled={session.busy || session.stopping || session.status?.phase !== "recording"}>
        {session.stopping ? "Stopping after track…" : "Stop after track"}
      </button>
    {/if}
    <button
      class="btn danger"
      onclick={async () => {
        await echolot.pauseForStop();
        await session.stop();
      }}
      disabled={session.busy}
      title={echolot.listing ? "Drops the song being recorded and stops the list" : echolot.active ? "Drops the song being recorded and pauses the Echolot session" : undefined}>{queue ? "Stop now" : "Stop"}</button>
  {/if}
</div>

<style>
  .controls {
    display: flex;
    gap: 8px;
  }
</style>
