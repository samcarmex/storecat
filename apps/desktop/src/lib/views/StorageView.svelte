<script lang="ts">
  import DriveName from "../components/DriveName.svelte";
  import { api, storage, type DupSummary, type FolderTree, type JobSpec, type KindBytes, type Suggestion, type Volume } from "../api";
  import type { AppStore } from "../store.svelte";
  import type { ViewId } from "../nav";
  import {
    bytes, count, date, HEALTH_COLOR, HEALTH_LABEL, KIND_COLOR, KIND_LABEL, MEDIA_COLOR, MEDIA_LABEL,
  } from "../format";
  import EmptyState from "../components/EmptyState.svelte";
  import Treemap, { type TreemapTile } from "../components/Treemap.svelte";

  let {
    store,
    focusVolume = $bindable(null),
    onNavigate,
    onReview,
  }: {
    store: AppStore;
    focusVolume?: number | null;
    onNavigate: (view: ViewId) => void;
    onReview: (spec: JobSpec) => void;
  } = $props();

  // ----- summary tiles -----
  let dupes = $state<DupSummary>({ groups: 0, files: 0, waste: 0 });
  $effect(() => {
    void store.catalogVersion;
    api.duplicateSummary().then((d) => (dupes = d)).catch(() => {});
  });

  /**
   * One entry per physical drive: catalogued folders share their drive's capacity, so a
   * folder counts only when no catalogued drive or earlier folder already covers it.
   */
  const physical = $derived.by(() => {
    const seen = new Set<string>();
    return store.volumes.filter((v) => {
      if (v.capacity <= 0) return false;
      if (v.source === "folder" && store.volumes.some((o) => o.source === "device" && o.online && o.capacity === v.capacity)) return false;
      const k = v.source === "device" ? `d${v.id}` : `c${v.capacity}`;
      if (seen.has(k)) return false;
      seen.add(k);
      return true;
    });
  });
  const fast = $derived(physical.filter((v) => v.media_type === "ssd" || v.media_type === "nvme"));
  const slow = $derived(physical.filter((v) => v.media_type === "hdd"));
  const sum = (vs: Volume[], f: (v: Volume) => number) => vs.reduce((a, v) => a + f(v), 0);
  const sorted = $derived([...store.volumes].sort((a, b) => b.total_bytes - a.total_bytes));
  const scanned = $derived(sorted.filter((v) => v.last_scan_at !== null && v.file_count > 0));

  // ----- treemap -----
  let volumeId = $state<number | null>(null);
  let tree = $state<FolderTree | null>(null);
  let kinds = $state<KindBytes[]>([]);
  let loading = $state(false);
  let treeError = $state<string | null>(null);
  /** Folder to open once the selected drive's tree loads (from a suggestion). */
  let pendingDir: number | null = null;

  $effect(() => {
    if (focusVolume !== null) {
      volumeId = focusVolume;
      focusVolume = null;
    } else if (volumeId === null && scanned.length) {
      volumeId = scanned[0].id;
    }
  });

  // Reload when the drive changes or its catalogue was rescanned.
  const scanStamp = $derived(store.volumes.find((v) => v.id === volumeId)?.last_scan_at ?? 0);
  $effect(() => {
    const id = volumeId;
    void scanStamp;
    if (id === null) return;
    const dir = pendingDir;
    pendingDir = null;
    void open(id, dir);
    storage.kindBreakdown(id).then((k) => (kinds = k)).catch(() => (kinds = []));
  });

  async function open(id: number, dirId: number | null) {
    loading = true;
    treeError = null;
    try {
      tree = await storage.folderTree(id, dirId);
    } catch (e) {
      tree = null;
      treeError = String(e);
    } finally {
      loading = false;
    }
  }

  const tiles = $derived.by<TreemapTile[]>(() => {
    if (!tree) return [];
    const t: TreemapTile[] = tree.children.map((c) => ({
      key: `d${c.id}`,
      value: c.bytes,
      label: c.name,
      color: KIND_COLOR[c.top_kind],
      dirId: c.id,
      kind: "dir",
      detail: `${bytes(c.bytes)} · ${count(c.files)} files · mostly ${KIND_LABEL[c.top_kind].toLowerCase()} · changed ${date(c.newest)}`,
    }));
    for (const f of tree.files) {
      t.push({
        key: `f${f.id}`,
        value: f.size,
        label: f.name,
        color: KIND_COLOR[f.kind],
        dirId: null,
        kind: "file",
        detail: `${bytes(f.size)} · ${KIND_LABEL[f.kind]} · modified ${date(f.mtime)}`,
      });
    }
    if (tree.rest_bytes > 0) {
      t.push({
        key: "rest",
        value: tree.rest_bytes,
        label: `${count(tree.rest_count)} more`,
        color: KIND_COLOR.other,
        dirId: null,
        kind: "rest",
        detail: `${bytes(tree.rest_bytes)} in smaller files and folders`,
      });
    }
    return t;
  });

  const kindTotal = $derived(kinds.reduce((a, k) => a + k.bytes, 0));
  type Segment = { key: string; label: string; bytes: number; color: string; hatch?: boolean };
  /**
   * The capacity bar: content types by size, then space the scan did not catalogue (system
   * folders it skips, file-system overhead), then free space last. A catalogued folder
   * shares its disk with other things, so it shows its content only.
   */
  const segments = $derived.by<Segment[]>(() => {
    const segs: Segment[] = kinds
      .filter((k) => k.bytes > 0)
      .map((k) => ({ key: k.kind, label: k.kind === "disk_image" ? "Disk images & VMs" : KIND_LABEL[k.kind], bytes: k.bytes, color: KIND_COLOR[k.kind] }));
    const v = store.volumes.find((o) => o.id === volumeId);
    if (v && v.source === "device" && v.capacity > 0) {
      const used = Math.max(0, v.capacity - v.free);
      const uncatalogued = used - kindTotal;
      if (uncatalogued > v.capacity * 0.005) {
        segs.push({ key: "system", label: "System and uncatalogued", bytes: uncatalogued, color: "#3a3f4b", hatch: true });
      }
      segs.push({ key: "free", label: "Free space", bytes: v.free, color: "transparent" });
    }
    return segs;
  });
  const barTotal = $derived(segments.reduce((a, g) => a + g.bytes, 0));

  // ----- suggestions -----
  let suggestions = $state<Suggestion[]>([]);
  let suggestLoading = $state(true);
  let reloadTimer: ReturnType<typeof setTimeout> | null = null;

  async function loadSuggestions() {
    try {
      suggestions = await storage.suggestions();
    } finally {
      suggestLoading = false;
    }
  }
  // Re-evaluate when drives or the catalogue change, debounced.
  $effect(() => {
    void store.catalogVersion;
    void store.volumes;
    if (reloadTimer) clearTimeout(reloadTimer);
    reloadTimer = setTimeout(() => void loadSuggestions(), 250);
    return () => {
      if (reloadTimer) clearTimeout(reloadTimer);
    };
  });

  async function dismiss(s: Suggestion, days: number | null) {
    suggestions = suggestions.filter((x) => x.key !== s.key);
    await storage.dismiss(s.key, days);
  }

  function missing(s: Suggestion): Volume[] {
    if (s.action?.type !== "job") return [];
    const needs = s.action.needs;
    return store.volumes.filter((v) => needs.includes(v.id) && !v.online);
  }

  function act(s: Suggestion) {
    const a = s.action;
    if (!a) return;
    if (a.type === "job") {
      onReview(a.spec);
    } else if (a.view === "storage") {
      if (a.volume_id !== null && a.volume_id !== volumeId) {
        pendingDir = a.dir_id;
        volumeId = a.volume_id;
      } else if (a.volume_id !== null) {
        void open(a.volume_id, a.dir_id);
      }
      document.getElementById("treemap")?.scrollIntoView({ behavior: "smooth", block: "start" });
    } else {
      onNavigate(a.view as ViewId);
    }
  }

  function actionLabel(s: Suggestion): string {
    const a = s.action;
    if (!a) return "";
    if (a.type === "job") return a.spec.kind === "move" ? "Review move" : a.spec.kind === "delete" ? "Review cleanup" : "Review copy";
    if (a.view === "storage" && a.dir_id !== null) return "Show folder";
    return { storage: "Show folders", drives: "Open drives", duplicates: "Review duplicates", operations: "Open operations" }[a.view] ?? "Open";
  }

  const SEVERITY_COLOR: Record<string, string> = {
    urgent: "var(--danger)", warning: "var(--amber)", tip: "var(--teal)", info: "var(--fg-muted)",
  };
</script>

<div class="head">
  <h1>What lives where, <em>and what should move.</em></h1>
</div>

{#if store.volumes.length === 0}
  <EmptyState
    title="Nothing to show yet"
    body="Once drives are catalogued this view shows capacity by content type, free space on fast versus archive storage, and suggestions you can approve as move jobs."
  />
{:else}
  <div class="tiles">
    <div class="card tile">
      <div class="muted">Catalogued</div>
      <div class="num">{bytes(store.stats.bytes)}</div>
      <div class="muted">{count(store.stats.files)} files on {store.stats.volumes} {store.stats.volumes === 1 ? "drive" : "drives"}</div>
    </div>
    <div class="card tile">
      <div class="muted">Free on SSD / NVMe</div>
      <div class="num" style="color: var(--teal)">{bytes(sum(fast, (v) => v.free))}</div>
      <div class="muted">of {bytes(sum(fast, (v) => v.capacity))} fast storage{fast.some((v) => !v.online) ? " (last seen)" : ""}</div>
    </div>
    <div class="card tile">
      <div class="muted">Free on HDD</div>
      {#if slow.length === 0}
        <div class="num" style="color: var(--fg-muted)">—</div>
        <div class="muted">No spinning disks catalogued yet</div>
      {:else}
        <div class="num" style="color: var(--amber)">{bytes(sum(slow, (v) => v.free))}</div>
        <div class="muted">of {bytes(sum(slow, (v) => v.capacity))} spinning disks{slow.some((v) => !v.online) ? " (last seen)" : ""}</div>
      {/if}
    </div>
    <div class="card tile">
      <div class="muted">Reclaimable duplicates</div>
      {#if dupes.groups > 0}
        <div class="num" style="color: var(--accent)">{bytes(dupes.waste)}</div>
        <div class="muted">{count(dupes.files)} files in {count(dupes.groups)} groups</div>
      {:else}
        <div class="num" style="color: var(--fg-muted)">—</div>
        <div class="muted">None found among hashed files yet</div>
      {/if}
    </div>
  </div>

  <div class="split">
    <section class="card map-card" id="treemap" aria-label="Folder sizes">
      <div class="map-head">
        <label class="pick">
          <span class="sr">Drive</span>
          <select bind:value={volumeId}>
            {#each scanned as v (v.id)}
              <option value={v.id}>{MEDIA_LABEL[v.media_type]} · {v.label} · {bytes(v.total_bytes)}</option>
            {/each}
          </select>
        </label>
        {#if tree}
          <nav class="crumbs" aria-label="Folder path">
            {#each tree.crumbs as c, i (c.id)}
              {#if i > 0}<span class="sep">/</span>{/if}
              {#if i < tree.crumbs.length - 1}
                <button class="crumb" onclick={() => open(tree!.volume_id, c.id)}>{c.name || "Root"}</button>
              {:else}
                <span class="crumb here">{c.name || "Root"}</span>
              {/if}
            {/each}
          </nav>
          <span class="muted total">{bytes(tree.node.bytes)} · {count(tree.node.files)} files</span>
        {/if}
      </div>

      {#if kinds.length && barTotal > 0}
        <div class="kindbar" role="img" aria-label="What fills this drive">
          {#each segments as g (g.key)}
            <div
              class:free={g.key === "free"}
              class:hatch={g.hatch}
              style="width: {(g.bytes / barTotal) * 100}%; background-color: {g.color}"
              title="{g.label}: {bytes(g.bytes)}"
            ></div>
          {/each}
        </div>
        <div class="legend">
          {#each segments.filter((g) => g.key === "free" || g.key === "system" || g.bytes / barTotal >= 0.01).slice(0, 9) as g (g.key)}
            <span>
              <i class:free={g.key === "free"} class:hatch={g.hatch} style="background-color: {g.color}"></i>{g.label} <b>{bytes(g.bytes)}</b>
            </span>
          {/each}
        </div>
      {/if}

      {#if treeError}
        <div class="muted pad">{treeError}</div>
      {:else if tree}
        <div class:dim={loading}>
          <Treemap {tiles} onOpen={(id) => open(tree!.volume_id, id)} />
        </div>
      {:else if scanned.length === 0}
        <div class="muted pad">Scan a drive to see where its space goes.</div>
      {:else}
        <div class="muted pad">Adding up folder sizes…</div>
      {/if}
    </section>

    <section class="card inbox" aria-label="Suggestions">
      <div class="inbox-head">
        <strong>Suggestions</strong>
        {#if suggestions.length}<span class="count">{suggestions.length}</span>{/if}
      </div>
      {#if suggestLoading}
        <div class="muted">Looking around…</div>
      {:else if suggestions.length === 0}
        <div class="quiet">
          <strong>All tidy.</strong>
          <span class="muted">Nothing needs attention right now. Storecat re-checks whenever drives or catalogues change.</span>
        </div>
      {:else}
        <ul>
          {#each suggestions as s (s.key)}
            {@const gone = missing(s)}
            <li class="sugg" style="--sev: {SEVERITY_COLOR[s.severity]}">
              <div class="top">
                <span class="sev">{s.severity}</span>
                {#if s.bytes > 0}<span class="muted">{bytes(s.bytes)}</span>{/if}
              </div>
              <strong class="title">{s.title}</strong>
              <p class="body">{s.body}</p>
              {#if gone.length}
                <p class="needs">Connect {gone.map((v) => v.label).join(" and ")} to review this.</p>
              {/if}
              <div class="actions">
                {#if s.action}
                  <button class="btn primary sm" disabled={gone.length > 0} onclick={() => act(s)}>{actionLabel(s)}</button>
                {/if}
                <button class="btn sm" onclick={() => dismiss(s, 30)} title="Hide for 30 days">Later</button>
                <button class="btn sm ghost" onclick={() => dismiss(s, null)} title="Don't suggest this again">Dismiss</button>
              </div>
            </li>
          {/each}
        </ul>
      {/if}
      <p class="muted foot">Suggestions never run on their own. A move opens as a plan in Operations, verified before anything is deleted.</p>
    </section>
  </div>

  <section class="card bars" aria-label="Used space by drive">
    <strong>Used space by drive</strong>
    {#each sorted as v (v.id)}
      {@const used = Math.max(0, v.capacity - v.free)}
      {@const pct = v.capacity > 0 ? (used / v.capacity) * 100 : 0}
      <div class="row">
        <span class="name">
          <span class="type" style="color: {MEDIA_COLOR[v.media_type]}">{MEDIA_LABEL[v.media_type]}</span><DriveName id={v.id} label={v.label} />
        </span>
        <div class="bar" title="{pct.toFixed(0)}% used"><div style="width: {pct}%; background: {pct >= 90 ? 'var(--danger)' : MEDIA_COLOR[v.media_type]}"></div></div>
        <span class="muted right">
          {#if v.capacity > 0}{bytes(v.free)} free{:else}{bytes(v.total_bytes)}{/if}
        </span>
        <span class="health" style="color: {HEALTH_COLOR[v.health]}" title="Drive health as reported by the operating system">
          {v.health === "unknown" ? "" : HEALTH_LABEL[v.health]}
        </span>
      </div>
    {/each}
  </section>
{/if}

<style>
  h1 {
    font-size: 34px;
    font-weight: 700;
    line-height: 1.1;
  }
  h1 em {
    font-style: normal;
    color: var(--accent);
  }
  .tiles {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(200px, 1fr));
    gap: 14px;
  }
  .tile {
    padding: 18px;
  }
  .num {
    font-family: var(--font-display);
    font-size: 32px;
    font-weight: 700;
    margin: 4px 0;
    line-height: 1.1;
  }
  .split {
    display: grid;
    grid-template-columns: minmax(0, 1fr) 360px;
    gap: 14px;
    align-items: start;
  }
  @media (max-width: 1100px) {
    .split {
      grid-template-columns: minmax(0, 1fr);
    }
  }
  .map-card {
    padding: 18px;
    display: flex;
    flex-direction: column;
    gap: 12px;
    min-width: 0;
  }
  .map-head {
    display: flex;
    align-items: center;
    gap: 14px;
    flex-wrap: wrap;
  }
  select {
    height: var(--control-h);
    border-radius: var(--radius-pill);
    border: 1px solid var(--line-strong);
    background: var(--bg);
    color: var(--fg);
    padding: 0 12px;
    font: inherit;
    font-size: 13px;
    max-width: 320px;
  }
  .sr {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip: rect(0 0 0 0);
  }
  .crumbs {
    display: flex;
    align-items: center;
    gap: 4px;
    flex-wrap: wrap;
    min-width: 0;
    font-size: 13px;
  }
  .crumb {
    background: none;
    border: none;
    padding: 2px 4px;
    color: var(--fg-soft);
    border-radius: 6px;
  }
  button.crumb:hover {
    background: var(--hover);
    color: var(--fg);
  }
  .crumb.here {
    color: var(--fg);
    font-weight: 600;
  }
  .sep {
    color: var(--fg-muted);
  }
  .total {
    margin-left: auto;
  }
  .kindbar {
    display: flex;
    height: 10px;
    border-radius: 5px;
    overflow: hidden;
    gap: 2px;
  }
  .kindbar div {
    min-width: 2px;
  }
  .kindbar .free,
  .legend i.free {
    box-shadow: inset 0 0 0 1px var(--line-strong);
    background: rgba(255, 255, 255, 0.04) !important;
  }
  .kindbar .hatch,
  .legend i.hatch {
    background-image: repeating-linear-gradient(
      135deg,
      rgba(255, 255, 255, 0.08) 0 3px,
      transparent 3px 6px
    );
  }
  .legend {
    display: flex;
    flex-wrap: wrap;
    gap: 6px 16px;
    font-size: 12px;
    color: var(--fg-muted);
  }
  .legend i {
    display: inline-block;
    width: 8px;
    height: 8px;
    border-radius: 2px;
    margin-right: 6px;
  }
  .legend b {
    color: var(--fg-soft);
    font-weight: 600;
  }
  .pad {
    padding: 40px 0;
    text-align: center;
  }
  .dim {
    opacity: 0.6;
    transition: opacity 0.15s;
  }
  .inbox {
    padding: 18px;
    display: flex;
    flex-direction: column;
    gap: 12px;
  }
  .inbox-head {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .count {
    background: var(--accent);
    color: var(--on-accent);
    border-radius: var(--radius-pill);
    font-size: 11px;
    font-weight: 700;
    padding: 1px 8px;
  }
  ul {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 10px;
  }
  .sugg {
    border: 1px solid var(--line);
    border-left: 3px solid var(--sev);
    border-radius: 12px;
    padding: 12px 14px;
    display: flex;
    flex-direction: column;
    gap: 6px;
    background: rgba(255, 255, 255, 0.02);
  }
  .top {
    display: flex;
    justify-content: space-between;
    align-items: center;
  }
  .sev {
    font-family: var(--font-display);
    font-size: 10px;
    font-weight: 700;
    letter-spacing: 0.08em;
    text-transform: uppercase;
    color: var(--sev);
  }
  .title {
    font-size: 14px;
    line-height: 1.3;
  }
  .body {
    margin: 0;
    font-size: 13px;
    color: var(--fg-soft);
    line-height: 1.45;
  }
  .needs {
    margin: 0;
    font-size: 12px;
    color: var(--amber);
  }
  .actions {
    display: flex;
    gap: 6px;
    flex-wrap: wrap;
    margin-top: 4px;
  }
  .btn.sm {
    height: 30px;
    padding: 0 12px;
    font-size: 12px;
  }
  .btn.ghost {
    border-color: transparent;
    color: var(--fg-muted);
  }
  .quiet {
    display: flex;
    flex-direction: column;
    gap: 4px;
    padding: 8px 0;
  }
  .foot {
    margin: 0;
  }
  .bars {
    padding: 20px;
    display: flex;
    flex-direction: column;
    gap: 14px;
  }
  .row {
    display: grid;
    grid-template-columns: 200px minmax(0, 1fr) 110px 64px;
    gap: 14px;
    align-items: center;
    font-size: 13px;
  }
  .name {
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .type {
    font-family: var(--font-display);
    font-size: 10px;
    font-weight: 700;
    letter-spacing: 0.08em;
    margin-right: 8px;
  }
  .bar {
    height: 14px;
    border-radius: 5px;
    background: rgba(255, 255, 255, 0.06);
    overflow: hidden;
  }
  .bar div {
    height: 100%;
    border-radius: 5px;
  }
  .right {
    text-align: right;
    font-variant-numeric: tabular-nums;
  }
  .health {
    font-size: 12px;
    font-weight: 600;
  }
</style>
