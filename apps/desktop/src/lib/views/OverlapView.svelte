<script lang="ts" module>
  import type { FolderRelation } from "../api";
  /** Filters kept while the user visits other views. */
  const remembered: { pair: [number, number] | null; relation: FolderRelation | "all"; shown: number } = {
    pair: null,
    relation: "all",
    shown: 25,
  };
</script>

<script lang="ts">
  import DriveName from "../components/DriveName.svelte";
  import { api, type FolderMatch, type FolderSide, type OverlapReport, type Volume } from "../api";
  import type { AppStore } from "../store.svelte";
  import { bytes, count, displayPath, KIND_COLOR, KIND_LABEL } from "../format";
  import EmptyState from "../components/EmptyState.svelte";

  let { store }: { store: AppStore } = $props();

  const GROUPS_PAGE = 25;

  let report = $state<OverlapReport | null>(null);
  let loading = $state(false);
  let error = $state<string | null>(null);

  let pair = $state<[number, number] | null>(remembered.pair);
  let relation = $state<FolderRelation | "all">(remembered.relation);
  let shown = $state(remembered.shown);
  $effect(() => {
    remembered.pair = pair;
    remembered.relation = relation;
    remembered.shown = shown;
  });

  $effect(() => {
    void store.catalogVersion;
    void load();
  });

  async function load() {
    loading = true;
    try {
      report = await api.overlapReport();
      error = null;
    } catch (e) {
      error = String(e);
    } finally {
      loading = false;
    }
  }

  const vols = $derived(new Map<number, Volume>(store.volumes.map((v) => [v.id, v])));
  const label = (id: number) => vols.get(id)?.label ?? `Drive #${id}`;

  /** Drives in the report, biggest first (the order the report gives). */
  const drives = $derived(report?.drives ?? []);
  const key = (a: number, b: number) => (a < b ? `${a}-${b}` : `${b}-${a}`);
  const shared = $derived(new Map((report?.pairs ?? []).map((p) => [key(p.a, p.b), p.shared])));
  const matchCount = $derived.by(() => {
    const m = new Map<string, number>();
    for (const f of report?.folders ?? []) {
      const k = key(f.a.volume_id, f.b.volume_id);
      m.set(k, (m.get(k) ?? 0) + 1);
    }
    return m;
  });

  // Heat scale: one hue, five steps on a log scale from 1 GB to the biggest pair.
  const FLOOR = 2 ** 30;
  const maxShared = $derived(Math.max(FLOOR * 2, ...(report?.pairs ?? []).map((p) => p.shared)));
  function step(b: number): number {
    if (b <= 0) return 0;
    if (b < FLOOR) return 1;
    return Math.min(5, 2 + Math.floor((4 * Math.log(b / FLOOR)) / Math.log(maxShared / FLOOR)));
  }
  /** Lower bound of each step, for the legend. */
  const stepFrom = $derived([0, 1, FLOOR, ...[1, 2, 3].map((i) => FLOOR * Math.pow(maxShared / FLOOR, i / 4))]);
  /** Short enough for a grid square: whole numbers from 10 up. */
  const short = (b: number) => bytes(b).replace(/^(\d{2,})\.\d/, "$1");

  function select(a: number, b: number) {
    pair = pair && key(pair[0], pair[1]) === key(a, b) ? null : [a, b];
    shown = GROUPS_PAGE;
  }

  // Tooltip for the grid
  let mapEl = $state<HTMLDivElement | null>(null);
  let tip = $state<{ a: number; b: number; x: number; y: number } | null>(null);
  function showTip(e: Event, a: number, b: number) {
    if (!mapEl) return;
    const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
    const w = mapEl.getBoundingClientRect();
    tip = { a, b, x: r.left - w.left + r.width / 2 + mapEl.scrollLeft, y: r.top - w.top };
  }

  // Folder matches, filtered then grouped
  const matches = $derived(
    (report?.folders ?? []).filter(
      (f) =>
        (relation === "all" || f.relation === relation) &&
        (!pair || key(f.a.volume_id, f.b.volume_id) === key(pair[0], pair[1])),
    ),
  );
  const groups = $derived.by(() => {
    const out: { id: number; items: FolderMatch[]; shared: number }[] = [];
    const at = new Map<number, number>();
    for (const f of matches) {
      let i = at.get(f.group);
      if (i === undefined) {
        i = out.length;
        at.set(f.group, i);
        out.push({ id: f.group, items: [], shared: 0 });
      }
      out[i].items.push(f);
      out[i].shared += f.shared;
    }
    return out;
  });
  const freed = $derived(matches.reduce((n, f) => n + f.shared, 0));

  const RELATIONS: { id: FolderRelation | "all"; label: string }[] = [
    { id: "all", label: "All" },
    { id: "identical", label: "Identical" },
    { id: "inside", label: "Inside another" },
    { id: "mostly_inside", label: "Mostly inside" },
  ];
  const RELATION_WORD: Record<FolderRelation, string> = {
    identical: "Identical",
    inside: "Inside",
    mostly_inside: "Mostly inside",
  };

  /** The drive with the most free space that could take this much, other than `id`. */
  function room(id: number, need: number): Volume | null {
    const fits = store.volumes.filter((v) => v.id !== id && v.role !== "system" && v.free >= need);
    return fits.sort((x, y) => y.free - x.free)[0] ?? null;
  }

  function hashing(id: number): boolean {
    const c = store.coverage[id];
    return !!c && c.done < c.files;
  }

  const pct = (part: number, whole: number) => (whole > 0 ? Math.round((part / whole) * 100) : 0);
  const kinds = $derived((report?.single_by_kind ?? []).filter((k) => k.bytes > 0));
  /** Kinds too small to see in the bar are folded into one segment. */
  const kindBar = $derived.by(() => {
    if (!report) return [];
    const total = report.single_copy || 1;
    const big = kinds.filter((k) => k.bytes / total >= 0.01);
    const rest = kinds.filter((k) => k.bytes / total < 0.01).reduce((n, k) => n + k.bytes, 0);
    return rest > 0 ? [...big, { kind: "other" as const, bytes: rest, folded: true }] : big;
  });

  function path(s: FolderSide): string {
    return s.path ? displayPath(s.path) : "(drive root)";
  }
</script>

{#if error}
  <div class="error card">{error}</div>
{/if}

{#if !report && loading}
  <p class="muted loading">Comparing drives…</p>
{:else if report && report.distinct === 0}
  <EmptyState
    title="Nothing to compare yet"
    body="This view compares hashed files across drives. Files are hashed during enrichment, which runs after a scan while a drive is connected."
  />
{:else if report}
  <!-- Summary -->
  <section class="stats" aria-label="Summary">
    <div class="card stat">
      <span class="k">Reclaimable</span>
      <span class="v accent">{bytes(report.reclaimable)}</span>
      <span class="muted">by keeping one copy of everything</span>
    </div>
    <div class="card stat">
      <span class="k">On one drive only</span>
      <span class="v">{bytes(report.single_copy)}</span>
      <span class="muted">{pct(report.single_copy, report.distinct)}% of your content has no second copy</span>
    </div>
    <div class="card stat">
      <span class="k">On two or more</span>
      <span class="v">{bytes(report.multi_copy)}</span>
      <span class="muted">{pct(report.multi_copy, report.distinct)}% already exists elsewhere</span>
    </div>
    <div class="card stat">
      <span class="k">Distinct content</span>
      <span class="v">{bytes(report.distinct)}</span>
      <span class="muted">each file counted once</span>
    </div>
  </section>

  {#if kindBar.length > 0}
    <section class="card kinds" aria-label="Single-copy content by type">
      <div class="kinds-head">
        <h2>What has no second copy</h2>
        <span class="muted">Hashed files over {bytes(report.min_file_bytes)}, app folders left out</span>
      </div>
      <div class="bar" role="img" aria-label="Single-copy content by type">
        {#each kindBar as k (k.kind + ("folded" in k ? "-rest" : ""))}
          <span
            style="flex: {k.bytes} 1 0; background: {'folded' in k ? 'var(--fg-muted)' : KIND_COLOR[k.kind]}"
            title="{'folded' in k ? 'Everything else' : KIND_LABEL[k.kind]}: {bytes(k.bytes)}"
          ></span>
        {/each}
      </div>
      <ul class="legend">
        {#each kindBar as k (k.kind + ("folded" in k ? "-rest" : ""))}
          <li>
            <span class="sw" style="background: {'folded' in k ? 'var(--fg-muted)' : KIND_COLOR[k.kind]}"></span>
            {"folded" in k ? "Everything else" : KIND_LABEL[k.kind]}
            <span class="muted">{bytes(k.bytes)}</span>
          </li>
        {/each}
      </ul>
    </section>
  {/if}

  <div class="two">
    <!-- Overlap map -->
    <section class="card map-card" aria-label="What each pair of drives shares">
      <div class="sec-head">
        <h2>Overlap map</h2>
        <span class="muted">Content found on both drives. Click a square to list the folders they share.</span>
      </div>
      <div class="map-wrap" bind:this={mapEl} onmouseleave={() => (tip = null)} role="presentation">
        <div class="map" style="grid-template-columns: minmax(110px, 160px) repeat({drives.length}, 58px)">
          {#each drives as row, i (row.volume_id)}
            <span class="rlabel"><DriveName id={row.volume_id} label={label(row.volume_id)} /></span>
            {#each drives as col, j (col.volume_id)}
              {#if j < i}
                {@const b = shared.get(key(row.volume_id, col.volume_id)) ?? 0}
                {@const s = step(b)}
                {@const on = !!pair && key(pair[0], pair[1]) === key(row.volume_id, col.volume_id)}
                <button
                  class="cell s{s}"
                  class:on
                  disabled={b === 0}
                  aria-pressed={on}
                  aria-label="{label(row.volume_id)} and {label(col.volume_id)}: {b > 0 ? bytes(b) + ' shared' : 'nothing shared'}"
                  onclick={() => select(row.volume_id, col.volume_id)}
                  onmouseenter={(e) => showTip(e, row.volume_id, col.volume_id)}
                  onfocus={(e) => showTip(e, row.volume_id, col.volume_id)}
                  onblur={() => (tip = null)}
                >{b >= FLOOR ? short(b) : ""}</button>
              {:else if j === i}
                {@const on = !!pair && pair[0] === row.volume_id && pair[1] === row.volume_id}
                <button
                  class="cell self"
                  class:on
                  disabled={row.within === 0}
                  aria-pressed={on}
                  aria-label="{label(row.volume_id)}: {bytes(row.within)} of extra copies within the drive"
                  onclick={() => select(row.volume_id, row.volume_id)}
                  onmouseenter={(e) => showTip(e, row.volume_id, row.volume_id)}
                  onfocus={(e) => showTip(e, row.volume_id, row.volume_id)}
                  onblur={() => (tip = null)}
                >{row.within >= FLOOR ? short(row.within) : ""}</button>
              {:else}
                <span></span>
              {/if}
            {/each}
          {/each}
          <span></span>
          {#each drives as col (col.volume_id)}
            <span class="clabel" title={label(col.volume_id)}>{label(col.volume_id)}</span>
          {/each}
        </div>
        {#if tip}
          {@const self = tip.a === tip.b}
          {@const b = self ? (drives.find((d) => d.volume_id === tip!.a)?.within ?? 0) : (shared.get(key(tip.a, tip.b)) ?? 0)}
          {@const n = matchCount.get(key(tip.a, tip.b)) ?? 0}
          <div class="tip" style="left: {tip.x}px; top: {tip.y}px" role="tooltip">
            {#if self}
              <strong>{label(tip.a)}</strong>
              <span>{bytes(b)} of extra copies within the drive</span>
            {:else}
              <strong>{label(tip.a)} + {label(tip.b)}</strong>
              <span>{bytes(b)} on both</span>
            {/if}
            <span class="muted">{n > 0 ? `${count(n)} folder ${n === 1 ? "match" : "matches"}` : "No folder matches over " + bytes(report.min_folder_bytes, 0)}</span>
          </div>
        {/if}
      </div>
      <div class="scale" aria-label="Colour scale">
        <span class="muted">Less</span>
        {#each [1, 2, 3, 4, 5] as s (s)}
          <span class="cell s{s} sw-cell" title={s === 1 ? `Under ${bytes(FLOOR, 0)}` : `${short(stepFrom[s])} or more`}></span>
        {/each}
        <span class="muted">More ({short(maxShared)})</span>
        <span class="cell self sw-cell"></span>
        <span class="muted">Extra copies within one drive</span>
      </div>
    </section>

    <!-- Only on this drive -->
    <section class="card only" aria-label="What's only on each drive">
      <div class="sec-head">
        <h2>Only on this drive</h2>
        <span class="muted">Lost if the drive fails</span>
      </div>
      <ul>
        {#each drives as d (d.volume_id)}
          {@const v = vols.get(d.volume_id)}
          {@const fit = d.only_here > 0 ? room(d.volume_id, d.only_here) : null}
          <li>
            <div class="only-line">
              <span class="dname"><DriveName id={d.volume_id} label={label(d.volume_id)} /></span>
              {#if v?.fault}<span class="tag bad" title={v.fault}>Read errors</span>{/if}
              {#if hashing(d.volume_id)}<span class="tag" title="Enrichment hasn't finished hashing this drive, so these numbers will change">Still hashing</span>{/if}
              <span class="spacer"></span>
              <span class="only-v">{bytes(d.only_here)}</span>
              <span class="muted">of {bytes(d.distinct)}</span>
            </div>
            <div class="obar" title="{bytes(d.only_here)} only here, {bytes(d.distinct - d.only_here)} also on another drive">
              <span class="o" style="width: {pct(d.only_here, d.distinct)}%"></span>
            </div>
            <div class="muted hint">
              {#if d.only_here === 0}
                Everything here is also on another drive.
              {:else if fit}
                <span title="Free space when the drive was last connected">Fits on {fit.label} ({bytes(fit.free)} free)</span>
              {:else}
                <span class="warn">No other drive has {bytes(d.only_here)} free.</span>
              {/if}
            </div>
          </li>
        {/each}
      </ul>
    </section>
  </div>

  <!-- Folder matches -->
  <section class="folders" aria-label="Folders found on more than one drive">
    <div class="sec-head">
      <h2>Folder matches</h2>
      <span class="muted">
        Folders over {bytes(report.min_folder_bytes, 0)} whose files are also in another folder.
        {#if matches.length > 0}{count(matches.length)} {matches.length === 1 ? "match" : "matches"} · {bytes(freed)} in the extra copies{/if}
      </span>
    </div>
    <div class="filters">
      {#each RELATIONS as r (r.id)}
        <button class="chip" class:on={relation === r.id} onclick={() => { relation = r.id; shown = GROUPS_PAGE; }}>{r.label}</button>
      {/each}
      {#if pair}
        <span class="sep"></span>
        <button class="chip on pair" onclick={() => (pair = null)} title="Show matches between every drive">
          {pair[0] === pair[1] ? `Within ${label(pair[0])}` : `${label(pair[0])} + ${label(pair[1])}`}
          <span aria-hidden="true">×</span>
        </button>
      {/if}
    </div>

    {#if groups.length === 0}
      <p class="muted none">
        {!pair
          ? "No folder matches with this filter."
          : pair[0] === pair[1]
            ? "No folder matches within this drive with this filter."
            : "No folder matches between these drives with this filter."}
      </p>
    {:else}
      <div class="glist">
        {#each groups.slice(0, shown) as g (g.id)}
          <div class="card group">
            {#if g.items.length > 1}
              <div class="ghead muted">{g.items.length} connected matches · the same content in several places</div>
            {/if}
            {#each g.items as f, i (i)}
              {@const scattered = f.relation !== "identical" && f.b.bytes > f.a.bytes * 2}
              <div class="match">
                <div class="mhead">
                  <span class="rel {f.relation}">{RELATION_WORD[f.relation]}</span>
                  <span class="muted">
                    {f.relation === "identical"
                      ? "The same files"
                      : f.relation === "inside"
                        ? "Every file in the first is also in the second"
                        : `${pct(f.shared, f.a.bytes)}% of the first is also in the second`}
                  </span>
                  <span class="spacer"></span>
                  <span class="freed">{bytes(f.shared)}</span>
                </div>
                {#each [f.a, f.b] as s, k (k)}
                  {@const isKeep = f.keep === (k === 0 ? "a" : "b")}
                  {@const v = vols.get(s.volume_id)}
                  <div class="side" class:keep={isKeep}>
                    <span class="dot" class:live={v?.online} title={v?.online ? "Connected" : "Not connected"}></span>
                    <span class="vol"><DriveName id={s.volume_id} label={label(s.volume_id)} /></span>
                    <span class="path mono" title={path(s)}>{path(s)}</span>
                    {#if k === 1 && scattered}
                      <span class="tag" title="The first folder's files are spread through this bigger folder rather than in one subfolder">spread out</span>
                    {/if}
                    <span class="muted size">{bytes(s.bytes)} · {count(s.files)} files</span>
                    {#if isKeep}<span class="tag keep-tag" title="Suggested copy to keep">Keep</span>{:else}<span class="tag-gap"></span>{/if}
                    <button class="btn tiny" disabled={!v?.online} onclick={() => api.revealItem(s.volume_id, s.dir_id, "dir")}>Reveal</button>
                  </div>
                {/each}
              </div>
            {/each}
          </div>
        {/each}
      </div>
      {#if groups.length > shown}
        <div class="more">
          <button class="btn small" onclick={() => (shown += GROUPS_PAGE)}>Show more ({count(groups.length - shown)} left)</button>
        </div>
      {/if}
      {#if report.more_folders > 0 && !pair && relation === "all"}
        <p class="muted none">{count(report.more_folders)} smaller matches aren't listed.</p>
      {/if}
    {/if}
  </section>
  <p class="muted foot">Worked out from the catalogue, so drives don't need to be connected. Nothing is changed here.</p>
{/if}

<style>
  .loading,
  .none,
  .foot {
    padding: 6px 2px;
  }
  .error {
    padding: 12px 16px;
    color: var(--danger);
  }
  h2 {
    font-size: 15px;
    font-weight: 600;
  }
  .sec-head,
  .kinds-head {
    display: flex;
    align-items: baseline;
    gap: 12px;
    flex-wrap: wrap;
    margin-bottom: 12px;
  }
  .stats {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(190px, 1fr));
    gap: 12px;
  }
  .stat {
    padding: 14px 16px;
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .stat .k {
    font-size: 11px;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--fg-muted);
  }
  .stat .v {
    font-family: var(--font-display);
    font-size: 26px;
    font-weight: 700;
  }
  .stat .v.accent {
    color: var(--accent);
  }
  .kinds {
    padding: 14px 16px;
  }
  .bar {
    display: flex;
    gap: 2px;
    height: 12px;
  }
  .bar span {
    min-width: 3px;
    border-radius: 3px;
  }
  .legend {
    list-style: none;
    margin: 10px 0 0;
    padding: 0;
    display: flex;
    flex-wrap: wrap;
    gap: 6px 18px;
    font-size: 12px;
  }
  .legend li {
    display: inline-flex;
    align-items: center;
    gap: 6px;
  }
  .sw {
    width: 10px;
    height: 10px;
    border-radius: 3px;
  }
  .two {
    display: grid;
    grid-template-columns: minmax(0, 1.6fr) minmax(280px, 1fr);
    gap: 12px;
    align-items: start;
  }
  @media (max-width: 1100px) {
    .two {
      grid-template-columns: minmax(0, 1fr);
    }
  }
  .map-card,
  .only {
    padding: 14px 16px;
    min-width: 0;
  }
  .map-wrap {
    position: relative;
    overflow-x: auto;
    padding-top: 4px;
  }
  .map {
    display: grid;
    gap: 2px;
    align-items: center;
    width: max-content;
  }
  .rlabel {
    font-size: 12px;
    color: var(--fg-soft);
    padding-right: 10px;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    text-align: right;
  }
  .clabel {
    font-size: 11px;
    color: var(--fg-soft);
    writing-mode: vertical-rl;
    transform: rotate(180deg);
    height: 96px;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    justify-self: center;
    padding-top: 6px;
  }
  .cell {
    height: 38px;
    border: 0;
    border-radius: 4px;
    padding: 0;
    font-size: 11px;
    font-weight: 600;
    color: var(--fg);
    background: rgba(255, 255, 255, 0.03);
    cursor: pointer;
  }
  .cell[disabled] {
    cursor: default;
    opacity: 1;
  }
  .cell.s1 {
    background: color-mix(in srgb, var(--purple) 14%, var(--surface));
  }
  .cell.s2 {
    background: color-mix(in srgb, var(--purple) 30%, var(--surface));
  }
  .cell.s3 {
    background: color-mix(in srgb, var(--purple) 48%, var(--surface));
  }
  .cell.s4 {
    background: color-mix(in srgb, var(--purple) 68%, var(--surface));
  }
  .cell.s5 {
    background: color-mix(in srgb, var(--purple) 92%, var(--surface));
    color: var(--bg);
  }
  .cell.self {
    background: transparent;
    border: 1px dashed var(--line-strong);
    color: var(--fg-soft);
    font-weight: 500;
  }
  .cell:not([disabled]):hover,
  .cell:focus-visible {
    outline: 2px solid var(--fg-soft);
    outline-offset: 1px;
  }
  .cell.on {
    outline: 2px solid var(--fg);
    outline-offset: 1px;
  }
  .tip {
    position: absolute;
    transform: translate(-50%, calc(-100% - 8px));
    background: var(--surface-2, #1f232d);
    border: 1px solid var(--line-strong);
    border-radius: 10px;
    padding: 8px 10px;
    font-size: 12px;
    display: flex;
    flex-direction: column;
    gap: 2px;
    white-space: nowrap;
    pointer-events: none;
    z-index: 5;
    box-shadow: 0 6px 20px rgba(0, 0, 0, 0.35);
  }
  .scale {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 4px;
    margin-top: 12px;
  }
  .scale .muted {
    margin: 0 4px;
  }
  .sw-cell {
    width: 18px;
    height: 14px;
    cursor: default;
  }
  .scale .cell.self {
    margin-left: 14px;
  }
  .only ul {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 14px;
  }
  .only-line {
    display: flex;
    align-items: baseline;
    gap: 8px;
    font-size: 13px;
    min-width: 0;
  }
  .dname {
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    min-width: 0;
  }
  .only-v {
    font-family: var(--font-display);
    font-weight: 700;
  }
  .obar {
    height: 6px;
    border-radius: 3px;
    background: color-mix(in srgb, var(--purple) 18%, var(--surface));
    margin: 6px 0 4px;
    overflow: hidden;
  }
  .obar .o {
    display: block;
    height: 100%;
    background: var(--purple);
    border-radius: 3px;
  }
  .hint {
    font-size: 11px;
  }
  .warn {
    color: var(--amber);
  }
  .spacer {
    flex: 1;
  }
  .tag {
    flex-shrink: 0;
    font-size: 11px;
    padding: 2px 8px;
    border-radius: 999px;
    background: var(--hover);
    color: var(--fg-soft);
    white-space: nowrap;
  }
  .tag.bad {
    color: var(--danger);
  }
  .keep-tag {
    color: var(--teal);
  }
  .tag-gap {
    width: 44px;
    flex-shrink: 0;
  }
  .filters {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 8px;
    margin-bottom: 10px;
  }
  .chip {
    height: 30px;
    padding: 0 12px;
    border: 1px solid var(--line-strong);
    border-radius: var(--radius-pill);
    background: transparent;
    color: var(--fg-soft);
    font-size: 12px;
    display: inline-flex;
    align-items: center;
    gap: 8px;
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
  .sep {
    width: 1px;
    height: 18px;
    background: var(--line-strong);
    margin: 0 2px;
  }
  .glist {
    display: flex;
    flex-direction: column;
    gap: 10px;
  }
  .group {
    padding: 6px 8px;
    display: flex;
    flex-direction: column;
  }
  .ghead {
    padding: 6px 8px 0;
    font-size: 11px;
  }
  .match {
    padding: 8px;
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .match + .match {
    border-top: 1px solid var(--line);
  }
  .mhead {
    display: flex;
    align-items: center;
    gap: 10px;
    font-size: 12px;
  }
  .rel {
    font-size: 11px;
    font-weight: 600;
    padding: 2px 8px;
    border-radius: 999px;
    background: color-mix(in srgb, var(--purple) 22%, transparent);
    color: var(--fg);
  }
  .freed {
    font-family: var(--font-display);
    font-weight: 700;
    color: var(--accent);
    font-size: 14px;
  }
  .side {
    display: flex;
    align-items: center;
    gap: 12px;
    font-size: 13px;
    min-width: 0;
    padding-left: 4px;
  }
  .dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--kind-other);
    flex-shrink: 0;
  }
  .dot.live {
    background: var(--teal);
  }
  .vol {
    width: 140px;
    flex-shrink: 0;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .path {
    flex: 1;
    min-width: 0;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .mono {
    font-family: var(--font-mono);
    font-size: 12px;
    color: var(--fg-soft);
  }
  .side.keep .path {
    color: var(--fg);
  }
  .size {
    flex-shrink: 0;
    white-space: nowrap;
  }
  .btn.tiny {
    height: 24px;
    padding: 0 10px;
    font-size: 11px;
  }
  .btn.small {
    height: 30px;
    font-size: 12px;
    padding: 0 12px;
  }
  .more {
    display: flex;
    justify-content: center;
    padding: 10px;
  }
</style>
