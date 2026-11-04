<script lang="ts">
  import { coverOf } from "../lib/covers";

  /** `src` when known, else read from `file`. */
  let { src = null, file = null, size }: { src?: string | null; file?: string | null; size: number } = $props();

  let read = $state<string | null>(null);
  $effect(() => {
    read = null;
    if (!src && file) {
      const asked = file;
      void coverOf(asked).then((c) => {
        if (file === asked) read = c;
      });
    }
  });
  const image = $derived(src ?? read);
</script>

<span class="cover" style:width="{size}px" style:height="{size}px">
  {#if image}
    <img src={image} alt="" draggable="false" />
  {:else}
    <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M9 17.5V6l10-2v11.5M9 17.5a2.5 2.5 0 1 1-5 0 2.5 2.5 0 0 1 5 0Zm10-2a2.5 2.5 0 1 1-5 0 2.5 2.5 0 0 1 5 0Z" /></svg>
  {/if}
</span>

<style>
  .cover {
    flex: none;
    display: grid;
    place-items: center;
    overflow: hidden;
    border: 1px solid var(--line);
    background: linear-gradient(135deg, rgba(34, 211, 238, 0.08), rgba(167, 139, 250, 0.08)), var(--panel-2);
  }

  img {
    width: 100%;
    height: 100%;
    object-fit: cover;
  }

  svg {
    width: 42%;
    height: 42%;
    fill: none;
    stroke: var(--line-strong);
    stroke-width: 1.4;
    stroke-linejoin: round;
  }
</style>
