<script lang="ts">
  import { onMount } from "svelte";
  import TopBar from "./lib/components/TopBar.svelte";
  import SearchView from "./lib/views/SearchView.svelte";
  import DrivesView from "./lib/views/DrivesView.svelte";
  import StorageView from "./lib/views/StorageView.svelte";
  import PlaceholderView from "./lib/views/PlaceholderView.svelte";
  import DuplicatesView from "./lib/views/DuplicatesView.svelte";
  import SettingsView from "./lib/views/SettingsView.svelte";
  import OperationsView from "./lib/views/OperationsView.svelte";
  import PaletteView from "./lib/views/PaletteView.svelte";
  import UpdateBanner from "./lib/components/UpdateBanner.svelte";
  import { updates } from "./lib/updates.svelte";
  import { isPaletteWindow, ops, type JobSpec } from "./lib/api";
  import type { ViewId } from "./lib/nav";
  import { AppStore } from "./lib/store.svelte";

  const store = new AppStore();
  const isPalette = isPaletteWindow();
  let view = $state<ViewId>("search");
  /** A query handed over by the quick-search palette or the tray. */
  let handoff = $state<string | null>(null);
  /** A job proposed by a storage suggestion, waiting to be reviewed in Operations. */
  let pendingJob = $state<JobSpec | null>(null);
  let storageFocus = $state<number | null>(null);

  function review(spec: JobSpec) {
    pendingJob = spec;
    view = "operations";
  }

  onMount(() => {
    if (isPalette) return;
    void store.init();
    // Out of the way of startup: the window is usable long before this runs.
    setTimeout(() => void updates.maybeCheck(), 10_000);
    void ops.onSetQuery((q) => {
      handoff = q;
      view = "search";
    });
  });
</script>

{#if isPalette}
  <PaletteView />
{:else}
<TopBar bind:view {store} />

<main class:fill={view === "search"}>
  <UpdateBanner />
  {#if view === "search"}
    <SearchView {store} onGoDrives={() => (view = "drives")} bind:handoff />
  {:else if view === "drives"}
    <DrivesView {store} />
  {:else if view === "storage"}
    <StorageView {store} bind:focusVolume={storageFocus} onNavigate={(v) => (view = v)} onReview={review} />
  {:else if view === "duplicates"}
    <DuplicatesView {store} />
  {:else if view === "settings"}
    <SettingsView {store} />
  {:else if view === "operations"}
    <OperationsView {store} bind:prefill={pendingJob} />
  {:else}
    <PlaceholderView title="Not here yet" body="This view arrives in a later phase." />
  {/if}
</main>
{/if}

<style>
  main {
    flex: 1;
    width: 100%;
    max-width: 1600px;
    margin: 0 auto;
    padding: 24px 32px 28px;
    display: flex;
    flex-direction: column;
    gap: var(--gap);
  }
  /* The search view owns the remaining height so its result list can scroll. */
  main.fill {
    height: calc(100vh - 71px);
    overflow: hidden;
  }
</style>
