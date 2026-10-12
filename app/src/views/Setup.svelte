<script lang="ts">
  import EcholotSetup from "../components/EcholotSetup.svelte";
  import Panel from "../components/Panel.svelte";
  import { api, type CheckState } from "../lib/api";
  import { session } from "../lib/session.svelte";

  const badge: Record<CheckState, string> = { ok: "ok", warn: "warn", bad: "fix", check: "check" };
  const fixLabel: Record<string, string> = { install_cable: "Install VB-Cable", cable_rate: "Set rate", route_cable: "Route to cable" };
</script>

<div class="grid">
  <div class="col">
    <Panel label="Health" index="01">
      {#snippet actions()}
        <button class="btn" onclick={() => session.check()} disabled={session.checking}>{session.checking ? "Checking…" : "Re-check"}</button>
      {/snippet}

      <p class="muted intro mono">Source: {session.sources.find((s) => s.id === session.selected)?.name ?? "—"}</p>
      <ul class="checks">
        {#each session.checks as c (c.id)}
          <li class="s-{c.state}">
            <span class="badge mono">{badge[c.state]}</span>
            <div>
              <div class="title">{c.title}</div>
              <div class="detail muted">{c.detail}</div>
            </div>
            {#if c.fix}
              <button class="btn primary" onclick={() => session.fix(c.fix!)} disabled={session.fixing !== null}>
                {session.fixing === c.fix ? "Working…" : (fixLabel[c.fix] ?? "Fix")}
              </button>
            {/if}
          </li>
        {/each}
      </ul>
      {#if session.notice}<p class="notice ok mono">{session.notice}</p>{/if}
    </Panel>
    <EcholotSetup />
  </div>

  <Panel label="How it works" index="02">
    <ol>
      {#if session.platform?.os === "linux"}
        <li><b>Straight from the app.</b> Mitschnitt connects to the app's own PipeWire stream, before anything else is mixed in. You keep listening as usual.</li>
        <li><b>Bit-exact capture.</b> While listening, the app's volume stays at 100 % and PipeWire runs at the app's sample rate. Use your system volume for loudness.</li>
      {:else}
        <li><b>Virtual cable.</b> The app plays into VB-Cable – silent, effect-free, at its own sample rate. Mitschnitt plays it back to your headphones itself (monitor).</li>
        <li><b>Bit-exact capture.</b> Only the app's own audio is taken. Its volume is fixed at exactly 50 % while listening and restored in the file, so Windows' peak limiter never touches it.</li>
      {/if}
      <li><b>Whole tracks only.</b> A track counts if it starts after silence and plays to its exact end. Pause, skip, seek or a buffering stall discards it.</li>
      <li><b>Proven.</b> Every sample is checked on arrival: a lossless source arrives as exact 24-bit values, anything lossy or processed does not. Files are read back, compared and checked for dropouts, fades and limiting.</li>
    </ol>
    {#if session.platform?.os === "windows"}
      <p class="muted credit">
        VB-Cable is donationware by VB-Audio and is downloaded from their site, never bundled.
        <button class="link mono" onclick={() => api.open("https://vb-audio.com/Cable/")}>vb-audio.com</button>
      </p>
    {/if}
  </Panel>
</div>

<style>
  .grid {
    display: grid;
    grid-template-columns: 1.4fr 1fr;
    gap: 16px;
    align-items: start;
  }

  .col {
    display: grid;
    gap: 16px;
  }

  .intro {
    margin: -6px 0 10px;
    font-size: 11px;
    text-transform: uppercase;
    letter-spacing: 0.1em;
  }

  .checks {
    list-style: none;
    margin: 0;
    padding: 0;
  }

  .checks li {
    display: grid;
    grid-template-columns: 44px 1fr auto;
    gap: 12px;
    align-items: center;
    padding: 10px 0;
    border-top: 1px solid var(--faint);
  }

  .badge {
    padding: 1px 0;
    text-align: center;
    font-size: 10px;
    letter-spacing: 0.1em;
    text-transform: uppercase;
    border: 1px solid currentColor;
  }

  .s-ok .badge {
    color: var(--ok);
  }
  .s-warn .badge {
    color: var(--warn);
  }
  .s-bad .badge {
    color: var(--bad);
  }
  .s-check .badge {
    color: var(--violet);
  }

  .title {
    font-weight: 600;
  }

  .detail {
    font-size: 12px;
  }

  .notice {
    margin: 12px 0 0;
    font-size: 12px;
  }

  ol {
    margin: 0;
    padding-left: 18px;
    display: grid;
    gap: 10px;
    font-size: 13px;
    color: var(--muted);
  }

  b {
    color: var(--text);
    font-weight: 600;
  }

  .credit {
    margin: 16px 0 0;
    font-size: 12px;
  }

  .link {
    border: none;
    background: none;
    padding: 0;
    color: var(--cyan);
  }

  @media (max-width: 860px) {
    .grid {
      grid-template-columns: 1fr;
    }
  }
</style>
