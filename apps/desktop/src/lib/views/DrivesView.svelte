<script lang="ts">
  import type { AppStore } from "../store.svelte";
  import { bytes, count, driveKind, MEDIA_COLOR, MEDIA_LABEL } from "../format";
  import DriveIcon from "../components/DriveIcon.svelte";
  import EmptyState from "../components/EmptyState.svelte";
  import DriveCard from "../components/DriveCard.svelte";

  let { store }: { store: AppStore } = $props();

  const headline = $derived.by(() => {
    const n = store.volumes.length;
    if (n === 0) return ["Every drive,", "one shelf."];
    const words = ["One", "Two", "Three", "Four", "Five", "Six", "Seven", "Eight", "Nine", "Ten", "Eleven", "Twelve"];
    const w = n <= 12 ? words[n - 1] : String(n);
    return [`${w} ${n === 1 ? "drive" : "drives"},`, "one shelf."];
  });
</script>

<div class="head">
  <h1>{headline[0]} <em>{headline[1]}</em></h1>
  <span class="muted">
    {#if store.stats.volumes > 0}
      {bytes(store.stats.bytes)} catalogued · {store.online} connected · {count(store.stats.files)} files
    {:else}
      0 catalogued · {store.detected.length} connected
    {/if}
  </span>
  <div class="head-actions">
    <button class="btn" onclick={() => store.importCatalogue()} title="Bring in a catalogue exported elsewhere or found in a drive's .storecat folder">Import…</button>
    <button class="btn primary" onclick={() => store.pickFolder()}>+ Add folder or share…</button>
  </div>
</div>

{#if store.error}
  <div class="error card">{store.error} <button class="btn small" onclick={() => (store.error = null)}>Dismiss</button></div>
{/if}

{#if store.detected.length > 0}
  <section class="card avail" aria-label="Connected drives not yet catalogued">
    <div class="avail-head">
      <strong>Connected now, not catalogued</strong>
      <span class="muted">Cataloguing reads names, sizes and dates only. Nothing is modified.</span>
    </div>
    <ul>
      {#each store.detected as d (d.identity)}
        <li>
          <span class="dicon" style="color: {MEDIA_COLOR[d.media_type]}"><DriveIcon kind={driveKind(d)} size={26} /></span>
          <span class="type" style="color: {MEDIA_COLOR[d.media_type]}">{MEDIA_LABEL[d.media_type]}</span>
          <span class="dname">
            <span>{d.label}</span>
            <span class="muted">{d.mount_path} · {d.fs} · {bytes(d.free)} free of {bytes(d.capacity)}{d.removable ? " · removable" : ""}</span>
          </span>
          <button class="btn small primary" onclick={() => store.addDetected(d.identity)}>Catalogue</button>
        </li>
      {/each}
    </ul>
  </section>
{/if}

{#if store.volumes.length === 0}
  <EmptyState
    title="No drives catalogued yet"
    body="Catalogue a connected drive above, or add any folder or network share. Each one gets its own catalogue and a stable identity that survives drive-letter changes."
  />
{:else}
  <div class="grid">
    {#each store.volumes as v (v.id)}
      <DriveCard volume={v} {store} />
    {/each}
  </div>
{/if}

<style>
  .head {
    display: flex;
    align-items: flex-end;
    gap: 24px;
    flex-wrap: wrap;
  }
  h1 {
    font-size: 34px;
    font-weight: 700;
    line-height: 1.1;
  }
  h1 em {
    font-style: normal;
    color: var(--teal);
  }
  .head .muted {
    padding-bottom: 6px;
  }
  .head-actions {
    margin-left: auto;
    display: flex;
    gap: 8px;
  }
  .error {
    padding: 12px 16px;
    color: var(--danger);
    display: flex;
    gap: 12px;
    align-items: center;
    justify-content: space-between;
  }
  .avail {
    padding: 14px 18px;
    display: flex;
    flex-direction: column;
    gap: 10px;
  }
  .avail-head {
    display: flex;
    gap: 14px;
    align-items: baseline;
    flex-wrap: wrap;
    font-size: 13px;
  }
  ul {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
  }
  li {
    display: flex;
    align-items: center;
    gap: 14px;
    padding: 10px 0;
    border-top: 1px solid var(--line);
  }
  .dicon {
    display: inline-flex;
    flex: none;
  }
  .type {
    font-family: var(--font-display);
    font-size: 12px;
    font-weight: 700;
    letter-spacing: 0.08em;
    width: 54px;
  }
  .dname {
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-width: 0;
    flex: 1;
    font-size: 14px;
  }
  .dname .muted {
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .btn.small {
    height: 30px;
    font-size: 12px;
    padding: 0 12px;
  }
  .grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(300px, 1fr));
    gap: 14px;
  }
</style>
