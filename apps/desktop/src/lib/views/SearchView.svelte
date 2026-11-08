<script lang="ts">
  import { api, ops, type ExtCount, type Kind, type SavedSearch, type SearchHit, type SortBy } from "../api";
  import type { AppStore } from "../store.svelte";
  import { count, bytes } from "../format";
  import EmptyState from "../components/EmptyState.svelte";
  import VirtualList from "../components/VirtualList.svelte";
  import ResultRow from "../components/ResultRow.svelte";
  import DetailPane from "../components/DetailPane.svelte";

  let {
    store,
    onGoDrives,
    handoff = $bindable<string | null>(null),
  }: { store: AppStore; onGoDrives: () => void; handoff?: string | null } = $props();

  // Saved searches
  let saved = $state<SavedSearch[]>([]);
  let savingName = $state<string | null>(null);
  async function loadSaved() {
    saved = await ops.savedSearches();
  }
  async function saveCurrent() {
    const n = (savingName ?? "").trim();
    if (!n) return;
    try {
      await ops.saveSearch(n, effectiveQuery);
      savingName = null;
      await loadSaved();
    } catch (e) {
      store.error = String(e);
    }
  }
  async function removeSaved(id: number) {
    await ops.deleteSavedSearch(id);
    await loadSaved();
  }
  $effect(() => {
    void loadSaved();
  });
  $effect(() => {
    if (handoff !== null) {
      query = handoff;
      kind = "all";
      exts = [];
      handoff = null;
      input?.focus();
    }
  });

  const PAGE = 300;
  const KIND_CHIPS: { id: Kind | "all"; label: string }[] = [
    { id: "all", label: "All" },
    { id: "document", label: "Documents" },
    { id: "image", label: "Images" },
    { id: "video", label: "Video" },
    { id: "audio", label: "Audio" },
    { id: "design", label: "Design" },
    { id: "archive", label: "Archives" },
    { id: "dir", label: "Folders" },
  ];

  let query = $state("");
  let kind = $state<Kind | "all">("all");
  let sort = $state<SortBy>("relevance");
  /** The order the results actually came back in. */
  let shownSort = $state<SortBy>("relevance");
  /** Extension chips for the chosen kind, and the ones switched on. */
  let extOptions = $state<ExtCount[]>([]);
  let exts = $state<string[]>([]);
  let input: HTMLInputElement | undefined = $state();
  let list: VirtualList<SearchHit> | undefined = $state();

  let hits = $state<SearchHit[]>([]);
  let total = $state(0);
  let elapsed = $state(0);
  let shownText = $state("");
  let selectedIndex = $state(-1);
  let loadingMore = false;
  let seq = 0;
  let timer: ReturnType<typeof setTimeout> | null = null;

  const effectiveQuery = $derived(
    [query, kind === "all" ? "" : `kind:${kind}`, exts.length ? `ext:${exts.join(",")}` : ""]
      .filter((p) => p.trim())
      .join(" "),
  );

  function chooseKind(k: Kind | "all") {
    if (kind === k) return;
    kind = k;
    exts = [];
  }

  $effect(() => {
    const k = kind;
    void store.catalogVersion;
    if (k === "all" || k === "dir") {
      extOptions = [];
      return;
    }
    void api.extensionCounts(k).then((r) => {
      if (kind === k) extOptions = r;
    });
  });

  function toggleExt(e: string) {
    exts = exts.includes(e) ? exts.filter((x) => x !== e) : [...exts, e];
  }

  /** Column headings: first click sorts the natural way, the next reverses it. */
  const COLUMNS: { label: string; first?: SortBy; second?: SortBy }[] = [
    { label: "Name", first: "name_asc", second: "name_desc" },
    { label: "Drive" },
    { label: "Size", first: "size_desc", second: "size_asc" },
    { label: "Modified", first: "modified_desc", second: "modified_asc" },
  ];
  function sortBy(c: (typeof COLUMNS)[number]) {
    if (!c.first || !c.second) return;
    sort = shownSort === c.first ? c.second : c.first;
  }
  function arrow(c: (typeof COLUMNS)[number]): string {
    if (shownSort === c.first) return c.first.endsWith("asc") ? "↑" : "↓";
    if (shownSort === c.second) return c.second!.endsWith("asc") ? "↑" : "↓";
    return "";
  }
  const selected = $derived(selectedIndex >= 0 ? (hits[selectedIndex] ?? null) : null);
  const hasQuery = $derived(effectiveQuery.trim().length > 0);

  $effect(() => {
    input?.focus();
  });

  // Re-run on query/filter/sort change, and whenever the catalogue content changes.
  $effect(() => {
    const q = effectiveQuery;
    const s = sort;
    void store.catalogVersion;
    if (timer) clearTimeout(timer);
    timer = setTimeout(() => run(q, s), 40);
  });

  async function run(q: string, s: SortBy) {
    const my = ++seq;
    if (!q.trim()) {
      hits = [];
      total = 0;
      selectedIndex = -1;
      return;
    }
    const r = await api.search(q, { limit: PAGE, offset: 0, sort: s });
    if (my !== seq) return;
    hits = r.hits;
    total = r.total;
    elapsed = r.elapsed_ms;
    shownText = r.text;
    shownSort = r.sort ?? s;
    selectedIndex = hits.length ? 0 : -1;
    list?.scrollToIndex(0);
  }

  async function loadMore() {
    if (loadingMore || hits.length >= total) return;
    loadingMore = true;
    const my = seq;
    try {
      const r = await api.search(effectiveQuery, { limit: PAGE, offset: hits.length, sort });
      if (my === seq) hits = [...hits, ...r.hits];
    } finally {
      loadingMore = false;
    }
  }

  function openSelected() {
    const h = selected;
    if (h && h.volume_online) void api.openItem(h.volume_id, h.item_id, h.item_type);
  }

  function onKey(e: KeyboardEvent) {
    if (!hits.length) return;
    if (e.key === "ArrowDown") {
      e.preventDefault();
      selectedIndex = Math.min(hits.length - 1, selectedIndex + 1);
      list?.scrollToIndex(selectedIndex);
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      selectedIndex = Math.max(0, selectedIndex - 1);
      list?.scrollToIndex(selectedIndex);
    } else if (e.key === "Enter") {
      openSelected();
    } else if (e.key === "Escape") {
      query = "";
      exts = [];
    }
  }
</script>

<div class="hero">
  <h1>Find it before you <em>plug it in.</em></h1>
  <span class="muted">
    {#if store.stats.files > 0}
      {count(store.stats.files)} files · {bytes(store.stats.bytes)} · {store.stats.volumes}
      {store.stats.volumes === 1 ? "drive" : "drives"}
    {:else}
      0 files · 0 drives catalogued
    {/if}
  </span>
</div>

<label for="q" class="sr-only">Search all drives</label>
<div class="searchbox">
  <svg width="20" height="20" viewBox="0 0 16 16" fill="none" stroke="var(--accent)" stroke-width="1.8" aria-hidden="true">
    <circle cx="7" cy="7" r="4.5" /><path d="M10.5 10.5 14 14" />
  </svg>
  <input
    id="q"
    bind:this={input}
    bind:value={query}
    onkeydown={onKey}
    placeholder="Search across every drive…  try: invoice ext:pdf  ·  size:>1gb kind:video  ·  modified:2023"
    autocomplete="off"
    spellcheck="false"
  />
  {#if query}
    <button class="clear" aria-label="Clear search" onclick={() => (query = "")}>×</button>
  {/if}
</div>

<div class="filters">
  {#each KIND_CHIPS as c (c.id)}
    <button class="chip" class:on={kind === c.id} onclick={() => chooseKind(c.id)}>{c.label}</button>
  {/each}
  {#if sort !== "relevance" && query.trim()}
    <span class="sep"></span>
    <button class="chip" onclick={() => (sort = "relevance")} title="Order by how well each result matches your words">Best match</button>
  {/if}
  {#if hasQuery}
    {#if savingName === null}
      <button class="chip star" onclick={() => (savingName = "")} title="Save this search">★ Save</button>
    {:else}
      <form class="savebox" onsubmit={(e) => { e.preventDefault(); void saveCurrent(); }}>
        <input bind:value={savingName} placeholder="Name this search" aria-label="Saved search name" />
        <button class="btn small primary" type="submit">Save</button>
        <button class="btn small ghost" type="button" onclick={() => (savingName = null)}>Cancel</button>
      </form>
    {/if}
    <span class="muted status">
      {count(total)} {total === 1 ? "result" : "results"} in {elapsed.toFixed(1)} ms
    </span>
  {/if}
</div>

{#if store.stats.index_rebuilding}
  {@const t = store.stats.index_rebuild_total ?? 0}
  {@const pct = t > 0 ? Math.min(100, ((store.stats.index_rebuild_done ?? 0) / t) * 100) : 0}
  <div class="rebuild" role="status">
    <span class="spin"></span>
    <span>Updating the search index{t > 0 ? ` · ${Math.floor(pct)}%` : "…"}</span>
    <span class="muted">Results may be incomplete until it finishes.</span>
    {#if t > 0}<div class="rbar"><div style="width: {pct}%"></div></div>{/if}
  </div>
{/if}

{#if extOptions.length > 0}
  <div class="filters exts" aria-label="File types">
    {#each extOptions as e (e.ext)}
      <button class="chip small" class:on={exts.includes(e.ext)} onclick={() => toggleExt(e.ext)} title={`${count(e.files)} .${e.ext} files`}>
        {e.ext.toUpperCase()}<span class="n">{count(e.files)}</span>
      </button>
    {/each}
    {#if exts.length}
      <button class="chip small ghost" onclick={() => (exts = [])}>Clear</button>
    {/if}
  </div>
{/if}

{#if store.stats.volumes === 0}
  <EmptyState
    title="Nothing catalogued yet"
    body="Add a drive and Storecat will record every file on it. After that you can search here whether the drive is plugged in or not."
  >
    <button class="btn primary" onclick={onGoDrives}>+ Add a drive</button>
  </EmptyState>
{:else if !hasQuery}
  {#if saved.length}
    <section class="saved" aria-label="Saved searches">
      <span class="muted lbl">Saved</span>
      {#each saved as s (s.id)}
        <span class="savedchip">
          <button class="go" onclick={() => (query = s.query)} title={s.query}>★ {s.name}</button>
          <button class="x" aria-label="Delete saved search" onclick={() => removeSaved(s.id)}>×</button>
        </span>
      {/each}
    </section>
  {/if}
  <EmptyState
    title="Type to search"
    body="Names, folders, tags, metadata and document text across every catalogued drive, connected or not. Filters: ext:pdf  kind:video  size:>1gb  modified:2023  drive:seagate  tag:client  content:unpaid  has:thumb."
  />
{:else if hits.length === 0 && total === 0}
  <EmptyState title={`No results for “${shownText || query}”`} body="Try fewer words, a different spelling, or drop a filter." />
{:else}
  <div class="results">
    <section class="card list" aria-label="Results">
      <div class="head muted">
        {#each COLUMNS as c (c.label)}
          {#if c.first}
            <button class="sort" class:active={!!arrow(c)} onclick={() => sortBy(c)} title={`Sort by ${c.label.toLowerCase()}`}>
              {c.label}<span class="arrow">{arrow(c)}</span>
            </button>
          {:else}
            <span>{c.label}</span>
          {/if}
        {/each}
      </div>
      <VirtualList bind:this={list} items={hits} rowHeight={56} onNearEnd={loadMore} class="vl">
        {#snippet row(hit, i)}
          <ResultRow
            {hit}
            selected={i === selectedIndex}
            onselect={() => (selectedIndex = i)}
            onopen={openSelected}
          />
        {/snippet}
      </VirtualList>
    </section>
    <DetailPane hit={selected} query={effectiveQuery} />
  </div>
{/if}

<style>
  .hero {
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
    color: var(--accent);
  }
  .hero .muted {
    padding-bottom: 6px;
  }
  .searchbox {
    display: flex;
    align-items: center;
    gap: 14px;
    height: 60px;
    padding: 0 22px;
    background: var(--surface);
    border: 1px solid var(--line-strong);
    border-radius: 30px;
    flex-shrink: 0;
  }
  .searchbox:focus-within {
    border-color: var(--accent-line);
    box-shadow: 0 0 0 3px var(--accent-soft);
  }
  .searchbox input {
    flex: 1;
    min-width: 0;
    border: 0;
    outline: 0;
    background: transparent;
    font-size: 18px;
    color: #fff;
  }
  .searchbox input::placeholder {
    color: var(--fg-muted);
    font-size: 15px;
  }
  .clear {
    width: 28px;
    height: 28px;
    border: 0;
    border-radius: 50%;
    background: rgba(255, 255, 255, 0.08);
    color: var(--fg);
    font-size: 18px;
    line-height: 1;
  }
  .clear:hover {
    background: rgba(255, 255, 255, 0.16);
  }
  .filters {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
    align-items: center;
    flex-shrink: 0;
  }
  .chip {
    height: 32px;
    padding: 0 14px;
    border: 1px solid var(--line-strong);
    border-radius: var(--radius-pill);
    background: transparent;
    color: var(--fg-soft);
    font-size: 13px;
    display: inline-flex;
    align-items: center;
    gap: 6px;
  }
  .chip:hover {
    border-color: rgba(255, 255, 255, 0.35);
    color: #fff;
  }
  .chip.on {
    background: #fff;
    border-color: #fff;
    color: var(--on-accent);
    font-weight: 600;
  }
  .rebuild {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px 10px;
    margin: 0 0 12px;
    padding: 10px 14px;
    border: 1px solid var(--line);
    border-radius: 10px;
    background: var(--surface);
    font-size: 13px;
  }
  .rebuild .spin {
    width: 12px;
    height: 12px;
    border: 2px solid var(--line);
    border-top-color: var(--accent);
    border-radius: 50%;
    animation: spin 0.9s linear infinite;
  }
  .rbar {
    flex-basis: 100%;
    height: 4px;
    border-radius: 2px;
    background: var(--line);
    overflow: hidden;
  }
  .rbar div {
    height: 100%;
    background: var(--accent);
    transition: width 0.4s;
  }
  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }
  .exts {
    margin-top: -4px;
    gap: 6px;
  }
  .chip.small {
    height: 26px;
    padding: 0 10px;
    font-size: 12px;
  }
  .chip.small .n {
    font-size: 11px;
    opacity: 0.6;
    font-weight: 400;
  }
  .chip.ghost {
    border-color: transparent;
  }
  .sort {
    border: 0;
    background: transparent;
    color: inherit;
    font: inherit;
    letter-spacing: inherit;
    text-transform: inherit;
    text-align: left;
    padding: 0;
    cursor: pointer;
    display: inline-flex;
    gap: 4px;
    align-items: center;
  }
  .sort:hover,
  .sort.active {
    color: var(--fg);
  }
  .arrow {
    color: var(--accent);
  }
  .sep {
    width: 1px;
    height: 22px;
    background: var(--line-strong);
    margin: 0 6px;
  }
  .status {
    margin-left: auto;
  }
  .chip.star {
    color: var(--amber);
  }
  .savebox {
    display: flex;
    gap: 6px;
    align-items: center;
  }
  .savebox input {
    background: rgba(0, 0, 0, 0.25);
    border: 1px solid var(--line-strong);
    border-radius: 8px;
    padding: 6px 10px;
    font-size: 13px;
    outline: 0;
    width: 180px;
  }
  .btn.small {
    height: 30px;
    font-size: 12px;
    padding: 0 12px;
  }
  .btn.ghost {
    border-color: transparent;
    color: var(--fg-muted);
  }
  .saved {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
    align-items: center;
  }
  .saved .lbl {
    font-size: 11px;
    font-weight: 700;
    letter-spacing: 0.06em;
    text-transform: uppercase;
  }
  .savedchip {
    display: inline-flex;
    align-items: center;
    border: 1px solid var(--line-strong);
    border-radius: var(--radius-pill);
    overflow: hidden;
  }
  .savedchip button {
    all: unset;
    cursor: pointer;
    padding: 6px 10px;
    font-size: 13px;
  }
  .savedchip .go:hover {
    background: var(--hover);
  }
  .savedchip .x {
    padding: 6px 10px 6px 4px;
    color: var(--fg-muted);
  }
  .savedchip .x:hover {
    color: var(--danger);
  }
  .results {
    flex: 1;
    min-height: 0;
    display: flex;
    gap: var(--gap);
    align-items: stretch;
  }
  .list {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    padding: 8px;
  }
  .head {
    display: grid;
    grid-template-columns: minmax(0, 1fr) 170px 86px 110px;
    gap: 14px;
    padding: 8px 14px 8px;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    font-size: 11px;
    flex-shrink: 0;
  }
  .list :global(.vl) {
    flex: 1;
    min-height: 0;
  }
  .results :global(aside) {
    flex: 0 0 340px;
  }
  .sr-only {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip: rect(0 0 0 0);
  }
</style>
