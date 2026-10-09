<script lang="ts">
  import { onMount } from "svelte";
  import History from "./components/History.svelte";
  import Now from "./components/Now.svelte";
  import Source from "./components/Source.svelte";
  import { session, type Tab } from "./lib/session.svelte";
  import Settings from "./views/Settings.svelte";
  import Setup from "./views/Setup.svelte";

  const tabs: { id: Tab; label: string }[] = [
    { id: "listen", label: "Listen" },
    { id: "setup", label: "Setup" },
    { id: "settings", label: "Settings" },
  ];

  onMount(() => {
    session.init();
  });
</script>

<div class="shell">
  <header>
    <div class="brand">
      <span class="mark"></span>
      <span class="mono">MITSCHNITT</span>
    </div>
    <nav>
      {#each tabs as t}
        <button class="mono" class:on={session.tab === t.id} onclick={() => (session.tab = t.id)}>
          {t.label}{#if t.id === "setup" && session.problems > 0}<span class="count">{session.problems}</span>{/if}
        </button>
      {/each}
    </nav>
    <div class="status mono" class:live={session.listening} class:rec={session.status?.phase === "recording"}>
      <span class="dot"></span>{session.status?.phase === "recording" ? "REC" : session.listening ? "LISTENING" : "IDLE"}
    </div>
  </header>

  {#if session.error}
    <div class="error mono" role="alert">
      <span>{session.error}</span>
      <button onclick={() => (session.error = null)} aria-label="Dismiss">×</button>
    </div>
  {/if}

  <main>
    {#if session.tab === "listen"}
      <div class="listen">
        <Source />
        <div class="col">
          <Now />
          <History />
        </div>
      </div>
    {:else if session.tab === "setup"}
      <Setup />
    {:else}
      <Settings />
    {/if}
  </main>
</div>

<style>
  .shell {
    max-width: 1040px;
    margin: 0 auto;
    padding: 18px 24px 40px;
  }

  header {
    display: grid;
    grid-template-columns: 1fr auto 1fr;
    align-items: center;
    padding-bottom: 14px;
    margin-bottom: 20px;
    border-bottom: 1px solid var(--line);
  }

  .brand {
    display: flex;
    align-items: center;
    gap: 10px;
    font-size: 13px;
    font-weight: 700;
    letter-spacing: 0.3em;
  }

  .mark {
    width: 14px;
    height: 14px;
    background: var(--edge);
    box-shadow: 0 0 14px rgba(34, 211, 238, 0.6);
  }

  nav {
    display: flex;
    border: 1px solid var(--line);
  }

  nav button {
    position: relative;
    padding: 7px 18px;
    border: none;
    background: transparent;
    font-size: 11px;
    letter-spacing: 0.16em;
    text-transform: uppercase;
    color: var(--muted);
  }

  nav button + button {
    border-left: 1px solid var(--line);
  }

  nav button.on {
    color: var(--text);
    background: rgba(34, 211, 238, 0.08);
    box-shadow: inset 0 -2px 0 var(--cyan);
  }

  .count {
    margin-left: 8px;
    padding: 0 5px;
    background: var(--bad);
    color: #05070a;
    font-weight: 700;
  }

  .status {
    justify-self: end;
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 11px;
    letter-spacing: 0.16em;
    color: var(--muted);
  }

  .status .dot {
    width: 7px;
    height: 7px;
    background: var(--faint);
  }

  .status.live {
    color: var(--cyan);
  }

  .status.live .dot {
    background: var(--cyan);
    box-shadow: 0 0 10px var(--cyan);
  }

  .status.rec {
    color: var(--rec);
  }

  .status.rec .dot {
    background: var(--rec);
    box-shadow: 0 0 10px var(--rec);
  }

  .listen {
    display: grid;
    grid-template-columns: minmax(270px, 320px) 1fr;
    gap: 16px;
    align-items: start;
  }

  .col {
    display: grid;
    gap: 16px;
    min-width: 0;
  }

  .error {
    display: flex;
    justify-content: space-between;
    gap: 12px;
    margin-bottom: 16px;
    padding: 9px 14px;
    font-size: 12px;
    border: 1px solid rgba(251, 113, 133, 0.5);
    background: rgba(251, 113, 133, 0.07);
    color: var(--bad);
  }

  .error button {
    border: none;
    background: none;
    color: inherit;
    font-size: 16px;
    line-height: 1;
  }

  @media (max-width: 820px) {
    .listen {
      grid-template-columns: 1fr;
    }
  }
</style>
