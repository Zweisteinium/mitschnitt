<script lang="ts">
  import { echolot } from "../lib/echolot.svelte";
  import Panel from "./Panel.svelte";

  const s = $derived(echolot.snapshot);
  /** Disconnecting would drop the current song. */
  const busy = $derived(s.current !== null);
</script>

<Panel label="Echolot">
  {#snippet actions()}
    {#if echolot.connected}
      <button class="btn" onclick={() => echolot.disconnect()} disabled={echolot.busy || busy} title={busy ? "Wait until the current song is done" : undefined}>Disconnect</button>
    {/if}
  {/snippet}

  {#if echolot.connected}
    <div class="who small">
      <span class="dot" class:offline={s.connection === "offline"}></span>
      {s.connection === "offline" ? "Offline" : "Connected"}
    </div>
    <div class="account mono muted" title="{s.user}@{s.host}">{s.user}@{s.host}</div>
    <p class="small note">As "{s.device}". Echolot's songs are recorded while Mitschnitt is open.</p>
  {:else if echolot.pairing}
    <p class="small">Approve the connection in the browser. If Mitschnitt doesn't open by itself, paste the code Echolot shows:</p>
    <form
      class="code"
      onsubmit={(e) => {
        e.preventDefault();
        echolot.submitCode();
      }}
    >
      <input class="mono" bind:value={echolot.code} placeholder="Code" aria-label="Code from Echolot" />
      <button class="btn primary" type="submit" disabled={echolot.busy || !echolot.code.trim()}>OK</button>
    </form>
    <button class="btn wide" onclick={() => echolot.cancel()}>Cancel</button>
  {:else}
    <p class="small note">Optional. Connect your Echolot library to let it record the songs it is missing.</p>
    <label class="small muted" for="echolot-url">Address</label>
    <input id="echolot-url" class="mono url" bind:value={echolot.url} placeholder="https://echolot.example.org" />
    <p class="small note">Echolot will be able to start Spotify on this PC and put recordings into your library.</p>
    {#if echolot.confirming}<p class="small">Connect to this Echolot?</p>{/if}
    <button class="btn primary wide" onclick={() => echolot.connect()} disabled={echolot.busy || !echolot.url.trim()}>Connect Echolot</button>
  {/if}

  {#if echolot.error}<div class="error small">{echolot.error}</div>{/if}
</Panel>

<style>
  .who {
    display: flex;
    align-items: center;
    gap: 8px;
  }

  .account {
    margin: 2px 0 0 15px;
    font-size: 11px;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .dot {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: var(--ok);
    box-shadow: 0 0 8px rgba(52, 211, 153, 0.6);
  }

  .dot.offline {
    background: var(--warn);
    box-shadow: none;
  }

  .small {
    font-size: 12px;
  }

  .url,
  .code input {
    width: 100%;
    margin: 4px 0 8px;
    padding: 7px 9px;
    border: 1px solid var(--faint);
    background: var(--panel-2);
    color: var(--text);
  }

  .code {
    display: grid;
    grid-template-columns: 1fr auto;
    gap: 8px;
    align-items: center;
  }

  .code input {
    margin: 0;
  }

  .note {
    color: var(--muted);
    margin: 8px 0 10px;
  }

  .wide {
    width: 100%;
  }

  .error {
    margin-top: 10px;
    color: var(--bad);
  }
</style>
