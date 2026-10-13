<script lang="ts">
  import type { Snippet } from "svelte";

  let { label, index, actions, children }: { label: string; index?: string; actions?: Snippet; children: Snippet } = $props();
</script>

<section class="panel">
  <header>
    <h2>{#if index}<span class="index">{index}</span>{/if}{label}</h2>
    {#if actions}<div class="actions">{@render actions()}</div>{/if}
  </header>
  {@render children()}
</section>

<style>
  .panel {
    position: relative;
    padding: 16px 18px 18px;
    border: 1px solid var(--line);
    background: linear-gradient(180deg, rgba(34, 211, 238, 0.045), rgba(167, 139, 250, 0.015) 55%, transparent), var(--panel);
  }

  .panel::before {
    content: "";
    position: absolute;
    inset: -1px -1px auto -1px;
    height: 1px;
    background: var(--edge);
    opacity: 0.9;
  }

  .panel::after {
    content: "";
    position: absolute;
    right: -1px;
    bottom: -1px;
    width: 10px;
    height: 10px;
    border-right: 1px solid var(--violet);
    border-bottom: 1px solid var(--violet);
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

  .index {
    color: var(--cyan);
    margin-right: 10px;
  }

  .actions {
    display: flex;
    gap: 8px;
  }
</style>
