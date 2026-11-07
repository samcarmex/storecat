<script lang="ts">
  import { NAV, type ViewId } from "../nav";
  import type { AppStore } from "../store.svelte";
  import Logo from "./Logo.svelte";

  let { view = $bindable<ViewId>("search"), store }: { view?: ViewId; store: AppStore } = $props();

  const scanningCount = $derived(
    Object.values(store.progress).filter((p) => p.phase === "walking" || p.phase === "committing").length +
      Object.values(store.enrich).filter((p) => p.phase === "running").length,
  );
</script>

<header>
  <div class="brand">
    <Logo size={28} />
    <span>Storecat</span>
  </div>

  <nav aria-label="Main">
    {#each NAV as item (item.id)}
      <button
        class="tab"
        class:on={view === item.id}
        aria-current={view === item.id ? "page" : undefined}
        onclick={() => (view = item.id)}
      >
        {item.label}
        {#if item.id === "drives" && scanningCount > 0}
          <span class="badge" title="Scanning">{scanningCount}</span>
        {/if}
      </button>
    {/each}
  </nav>

  <div class="status">
    <span class="muted dot-line">
      <span class="dot" class:live={store.online > 0}></span>
      {#if store.volumes.length === 0}
        No drives catalogued
      {:else}
        {store.online} of {store.volumes.length} drives connected
      {/if}
    </span>
    <button class="btn icon" class:on={view === "settings"} aria-label="Settings" title="Settings" onclick={() => (view = "settings")}>
      <svg width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.6" aria-hidden="true">
        <circle cx="8" cy="8" r="2.5" />
        <path d="M8 1.5v2M8 12.5v2M1.5 8h2M12.5 8h2M3.4 3.4l1.4 1.4M11.2 11.2l1.4 1.4M3.4 12.6l1.4-1.4M11.2 4.8l1.4-1.4" />
      </svg>
    </button>
  </div>
</header>

<style>
  header {
    display: flex;
    align-items: center;
    gap: 16px;
    flex-wrap: wrap;
    padding: 14px 32px;
    border-bottom: 1px solid var(--line);
    height: 71px;
    box-sizing: border-box;
  }
  .brand {
    display: flex;
    align-items: center;
    gap: 10px;
    font-family: var(--font-display);
    font-size: 18px;
    font-weight: 700;
    letter-spacing: -0.02em;
  }
  nav {
    display: flex;
    gap: 4px;
    margin-left: 24px;
    padding: 4px;
    border-radius: var(--radius-pill);
    background: rgba(255, 255, 255, 0.04);
  }
  .tab {
    height: 38px;
    padding: 0 18px;
    border: 0;
    border-radius: var(--radius-pill);
    background: transparent;
    color: #aab0bd;
    font-size: 14px;
    font-weight: 500;
    display: inline-flex;
    align-items: center;
    gap: 8px;
  }
  .tab:hover {
    color: #fff;
    background: var(--hover);
  }
  .tab.on {
    color: var(--on-accent);
    background: var(--accent);
    font-weight: 600;
  }
  .badge {
    min-width: 18px;
    height: 18px;
    padding: 0 5px;
    border-radius: 9px;
    background: var(--teal);
    color: var(--on-accent);
    font-size: 11px;
    font-weight: 700;
    display: inline-flex;
    align-items: center;
    justify-content: center;
  }
  .status {
    margin-left: auto;
    display: flex;
    align-items: center;
    gap: 14px;
  }
  .dot-line {
    display: inline-flex;
    align-items: center;
    gap: 7px;
  }
  .dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--kind-other);
  }
  .dot.live {
    background: var(--teal);
  }
  .btn.icon {
    width: var(--control-h);
    padding: 0;
    justify-content: center;
  }
  .btn.icon.on {
    background: var(--accent);
    border-color: var(--accent);
    color: var(--on-accent);
  }
</style>
