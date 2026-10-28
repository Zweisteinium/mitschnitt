<script lang="ts">
  import type { Snippet } from "svelte";

  let { label, actions, children }: { label: string; actions?: Snippet; children: Snippet } = $props();
</script>

<section class="panel">
  <header>
    <h2>{label}</h2>
    {#if actions}<div class="actions">{@render actions()}</div>{/if}
  </header>
  {@render children()}
</section>

<style>
  .panel {
    position: relative;
    padding: 16px 18px 18px;
    border: 1px solid var(--line);
    background: var(--panel);
  }

  .panel::before {
    content: "";
    position: absolute;
    inset: -1px -1px auto -1px;
    height: 1px;
    background: var(--edge);
    opacity: 0.7;
  }

  header {
    display: flex;
    justify-content: space-between;
    align-items: center;
    margin-bottom: 14px;
    min-height: 26px;
  }

  h2 {
    margin: 0;
    font: 600 11px/1 var(--mono);
    letter-spacing: 0.16em;
    text-transform: uppercase;
    color: var(--muted);
  }

  .actions {
    display: flex;
    gap: 8px;
  }
</style>
