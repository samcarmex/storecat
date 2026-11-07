<script lang="ts">
  import { api, ops, type DupGroup, type DupMember, type DupPick, type DupScope, type DupSort, type DupSummary } from "../api";
  import type { AppStore } from "../store.svelte";
  import type { ViewId } from "../nav";
  import { bytes, count, date, displayPath, KIND_TILE } from "../format";
  import EmptyState from "../components/EmptyState.svelte";

  let { store, onNavigate }: { store: AppStore; onNavigate: (view: ViewId) => void } = $props();

  const PAGE = 100;

  let groups = $state<DupGroup[]>([]);
  let summary = $state<DupSummary>({ groups: 0, files: 0, waste: 0 });
  let loading = $state(false);
  let error = $state<string | null>(null);

  let scope = $state<DupScope>("mine");
  let sort = $state<DupSort>("waste");
  let desc = $state(true);

  let open = $state<Set<string>>(new Set());
  let verifying = $state<Set<string>>(new Set());
  /** Chosen copy to keep per group, "volume:file". */
  let keep = $state<Record<string, string>>({});
  let selected = $state<Set<string>>(new Set());
  let working = $state(false);
  let confirmBulk = $state(false);
  /** Groups handed to a job, with what was said about them. */
  let sent = $state<Record<string, string>>({});
  /** How many copies each sent group had, to notice when the job has changed it. */
  let sentCount: Record<string, number> = {};
  let notice = $state<string | null>(null);

  async function load(more = false) {
    loading = true;
    try {
      const page = await api.duplicates({ scope, sort, desc, limit: PAGE, offset: more ? groups.length : 0 });
      groups = more ? [...groups, ...page.groups] : page.groups;
      summary = page.summary;
      error = null;
      if (!more) {
        const live = new Set(groups.map((g) => g.hash));
        selected = new Set([...selected].filter((h) => live.has(h)));
        // A group the job has finished with comes back smaller (or not at all).
        const still = Object.fromEntries(
          Object.entries(sent).filter(([h]) => groups.some((g) => g.hash === h && g.count === sentCount[h])),
        );
        if (Object.keys(still).length !== Object.keys(sent).length) sent = still;
      }
    } catch (e) {
      error = String(e);
    } finally {
      loading = false;
    }
  }

  $effect(() => {
    void store.catalogVersion;
    void scope;
    void sort;
    void desc;
    void load();
  });

  function sortBy(s: DupSort) {
    if (sort === s) desc = !desc;
    else {
      sort = s;
      desc = s !== "name";
    }
  }

  function setScope(s: DupScope) {
    if (scope === s) return;
    scope = s;
    selected = new Set();
    confirmBulk = false;
  }

  function toggle(hash: string) {
    const next = new Set(open);
    if (next.has(hash)) next.delete(hash);
    else next.add(hash);
    open = next;
  }

  const id = (m: { volume_id: number; file_id: number }) => `${m.volume_id}:${m.file_id}`;

  function keepKey(g: DupGroup): string | null {
    if (keep[g.hash]) return keep[g.hash];
    if (g.suggested_keep) return `${g.suggested_keep[0]}:${g.suggested_keep[1]}`;
    return null;
  }

  function keeper(g: DupGroup): DupMember | undefined {
    const k = keepKey(g);
    return g.members.find((m) => id(m) === k);
  }

  /** Copies that would go to the Recycle Bin: yours, connected, not the one kept. */
  function removable(g: DupGroup): DupMember[] {
    const k = keepKey(g);
    return g.members.filter((m) => !m.app_managed && m.volume_online && id(m) !== k);
  }

  /** Why a group can't be cleaned up right now, or null when it can. */
  function blocked(g: DupGroup): string | null {
    if (sent[g.hash]) return sent[g.hash];
    if (g.user_copies === 0) return "Every copy is in an app folder, so Storecat leaves them alone.";
    if (g.user_copies < 2) return "The other copies are in app folders, so there is nothing to remove.";
    const k = keeper(g);
    if (!k) return "Pick the copy to keep.";
    if (!k.volume_online) return `Connect ${k.volume_label} first: the copy to keep is on it.`;
    if (removable(g).length === 0) return "The other copies are on drives that aren't connected.";
    return null;
  }

  function pick(g: DupGroup): DupPick {
    const k = keeper(g)!;
    return { hash: g.hash, keep_volume_id: k.volume_id, keep_file_id: k.file_id };
  }

  const eligible = $derived(groups.filter((g) => !blocked(g)));
  const chosen = $derived(groups.filter((g) => selected.has(g.hash) && !blocked(g)));
  const chosenCopies = $derived(chosen.reduce((n, g) => n + removable(g).length, 0));
  const chosenBytes = $derived(chosen.reduce((n, g) => n + removable(g).length * g.size, 0));
  const allChosen = $derived(eligible.length > 0 && eligible.every((g) => selected.has(g.hash)));

  function select(g: DupGroup, on: boolean) {
    const next = new Set(selected);
    if (on) next.add(g.hash);
    else next.delete(g.hash);
    selected = next;
    confirmBulk = false;
  }

  function selectAll(on: boolean) {
    selected = on ? new Set(eligible.map((g) => g.hash)) : new Set();
    confirmBulk = false;
  }

  async function recycle(targets: DupGroup[]) {
    if (targets.length === 0) return;
    working = true;
    try {
      const job = await ops.duplicateCleanup(targets.map(pick));
      const n = job.files_total;
      notice = `Job #${job.id} started: ${count(n)} ${n === 1 ? "copy" : "copies"} on the way to the Recycle Bin. Each is checked against the copy you keep first.`;
      const next = { ...sent };
      for (const g of targets) {
        next[g.hash] = "Sent to Operations. The list refreshes when the job finishes.";
        sentCount[g.hash] = g.count;
      }
      sent = next;
      const left = new Set(selected);
      for (const g of targets) left.delete(g.hash);
      selected = left;
      confirmBulk = false;
    } catch (e) {
      error = String(e);
    } finally {
      working = false;
    }
  }

  async function verify(g: DupGroup) {
    verifying = new Set([...verifying, g.hash]);
    try {
      const updated = await api.verifyGroup(g.hash);
      groups = groups.map((x) => (x.hash === g.hash ? { ...updated, user_copies: x.user_copies, waste: x.waste } : x));
    } catch (e) {
      error = String(e);
    } finally {
      const next = new Set(verifying);
      next.delete(g.hash);
      verifying = next;
    }
  }

  const anyHashed = $derived(Object.values(store.coverage).some((c) => c.hashed > 0));

  const COLUMNS: { key: DupSort; label: string }[] = [
    { key: "name", label: "File" },
    { key: "count", label: "Copies" },
    { key: "size", label: "Each" },
    { key: "waste", label: "Reclaimable" },
  ];
</script>

<div class="head">
  <h1>Same file, <em>several shelves.</em></h1>
  <span class="muted">
    {#if summary.groups > 0}
      {count(summary.groups)} groups · {count(summary.files)} files · {bytes(summary.waste)} reclaimable
    {:else}
      Files over the size threshold that share a hash across your drives
    {/if}
  </span>
</div>

<div class="toolbar">
  <div class="seg" role="group" aria-label="Which duplicates">
    <button class:on={scope === "mine"} onclick={() => setScope("mine")} title="Copies in app caches, build output and app data are left out">Your files</button>
    <button class:on={scope === "all"} onclick={() => setScope("all")} title="Also list groups that live in app folders. Those copies are never removed.">Include app folders</button>
  </div>
  <span class="spacer"></span>
  {#if chosen.length > 0}
    <span class="muted">{count(chosen.length)} {chosen.length === 1 ? "group" : "groups"} selected</span>
    {#if confirmBulk}
      <button class="btn small" disabled={working} onclick={() => (confirmBulk = false)}>Cancel</button>
      <button class="btn small danger" disabled={working} onclick={() => recycle(chosen)}>
        {working ? "Working…" : `Yes, recycle ${count(chosenCopies)} ${chosenCopies === 1 ? "copy" : "copies"}`}
      </button>
    {:else}
      <button class="btn small danger" disabled={working} onclick={() => (confirmBulk = true)}>
        Recycle extra copies ({bytes(chosenBytes)})
      </button>
    {/if}
  {/if}
</div>

{#if notice}
  <div class="notice card">
    <span>{notice}</span>
    <button class="btn small" onclick={() => onNavigate("operations")}>Open Operations</button>
    <button class="btn tiny ghost" onclick={() => (notice = null)} aria-label="Dismiss">✕</button>
  </div>
{/if}

{#if error}
  <div class="error card">{error}</div>
{/if}

{#if groups.length === 0 && !loading}
  <EmptyState
    title={!anyHashed ? "No hashes yet" : scope === "mine" ? "No duplicates among your files" : "No duplicates found"}
    body={!anyHashed
      ? "Duplicates appear once drives have been enriched. Enrichment runs automatically after a scan while the drive is connected; you can also start it from a drive card."
      : scope === "mine"
        ? "Every hashed file of yours above the threshold is unique. Choose Include app folders to see copies inside app caches and build output."
        : "Every hashed file above the threshold is unique across your catalogued drives."}
  />
{:else}
  <section class="card list" aria-label="Duplicate groups">
    <div class="row-wrap cols">
      <span class="check">
        <input type="checkbox" checked={allChosen} disabled={eligible.length === 0} onchange={(e) => selectAll(e.currentTarget.checked)} aria-label="Select every group that can be cleaned up" title="Select every group that can be cleaned up" />
      </span>
      <div class="grid muted">
        {#each COLUMNS as c (c.key)}
          <button class="sort" class:active={sort === c.key} onclick={() => sortBy(c.key)} aria-pressed={sort === c.key} title={"Sort by " + c.label.toLowerCase()}>
            {c.label}<span class="arrow">{sort === c.key ? (desc ? "↓" : "↑") : ""}</span>
          </button>
        {/each}
        <span></span>
      </div>
    </div>
    {#each groups as g (g.hash)}
      {@const tile = KIND_TILE[g.kind] ?? KIND_TILE.other}
      {@const why = blocked(g)}
      {@const gone = removable(g)}
      <div class="group" class:open={open.has(g.hash)} class:sent={!!sent[g.hash]}>
        <div class="row-wrap">
          <span class="check">
            <input type="checkbox" checked={selected.has(g.hash) && !why} disabled={!!why} onchange={(e) => select(g, e.currentTarget.checked)} aria-label={"Select " + g.name} title={why ?? "Select for cleanup"} />
          </span>
          <button class="grid grow-row" onclick={() => toggle(g.hash)} aria-expanded={open.has(g.hash)}>
            <span class="name-cell">
              <span class="tile" style="background: {tile[0]}; color: {tile[1]}">{g.name.split(".").pop()?.toUpperCase().slice(0, 4) ?? "·"}</span>
              <span class="name">{g.name}</span>
            </span>
            <span>{g.count} ×{#if g.user_copies < g.count}<span class="sub" title="Copies in app folders, left alone">{g.count - g.user_copies} app</span>{/if}</span>
            <span>{bytes(g.size)}</span>
            <span class="waste">{g.waste > 0 ? bytes(g.waste) : "—"}</span>
            <span class="state" class:ok={g.verified}>{sent[g.hash] ? "in Operations" : g.verified ? "verified" : "quick hash"}</span>
          </button>
        </div>
        {#if open.has(g.hash)}
          <ul>
            {#each g.members as m (id(m))}
              {@const isKeep = keepKey(g) === id(m)}
              <li class:app={m.app_managed} class:keep={isKeep && !m.app_managed}>
                {#if m.app_managed}
                  <span class="radio-gap"></span>
                {:else}
                  <input type="radio" name={"keep-" + g.hash} checked={isKeep} onchange={() => (keep[g.hash] = id(m))} aria-label="Keep this copy" title="Keep this copy" />
                {/if}
                <span class="dot" class:live={m.volume_online} title={m.volume_online ? "Connected" : "Not connected"}></span>
                <span class="vol">{m.volume_label}</span>
                <span class="path mono" title={(m.path ? displayPath(m.path) + "\\" : "") + m.name}>{m.path ? displayPath(m.path) + "\\" : ""}{m.name}</span>
                {#if m.app_managed}
                  <span class="tag">App copy · left alone</span>
                {:else if isKeep}
                  <span class="tag keep-tag">Keep</span>
                {:else if !m.volume_online}
                  <span class="tag">Not connected</span>
                {/if}
                <span class="muted when">{date(m.mtime)}</span>
                <button class="btn tiny" disabled={!m.volume_online} onclick={() => api.revealItem(m.volume_id, m.file_id, "file")}>Reveal</button>
              </li>
            {/each}
            <li class="actions">
              {#if why}
                <span class="muted">{why}</span>
              {:else}
                <button class="btn small danger" disabled={working} onclick={() => recycle([g])}>
                  {working ? "Working…" : `Recycle ${gone.length} other ${gone.length === 1 ? "copy" : "copies"}`}
                </button>
                <span class="muted">Each copy is compared byte for byte with the one you keep before it goes to the Recycle Bin.</span>
              {/if}
              {#if !g.verified && !sent[g.hash]}
                <span class="spacer"></span>
                <button class="btn tiny" disabled={verifying.has(g.hash) || !g.members.some((m) => m.volume_online)} onclick={() => verify(g)}>
                  {verifying.has(g.hash) ? "Hashing…" : "Verify now"}
                </button>
              {/if}
            </li>
          </ul>
        {/if}
      </div>
    {/each}
    {#if groups.length < summary.groups}
      <div class="more">
        <button class="btn small" disabled={loading} onclick={() => load(true)}>
          {loading ? "Loading…" : `Show more (${count(summary.groups - groups.length)} left)`}
        </button>
      </div>
    {/if}
  </section>
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
    color: var(--purple);
  }
  .head .muted {
    padding-bottom: 6px;
  }
  .toolbar {
    display: flex;
    align-items: center;
    gap: 10px;
    flex-wrap: wrap;
    font-size: 12px;
  }
  .spacer {
    flex: 1;
  }
  .seg {
    display: inline-flex;
    padding: 3px;
    border-radius: 10px;
    background: var(--hover);
    gap: 2px;
  }
  .seg button {
    border: 0;
    background: transparent;
    color: var(--fg-soft);
    font-size: 12px;
    padding: 6px 12px;
    border-radius: 8px;
  }
  .seg button.on {
    background: var(--surface, rgba(255, 255, 255, 0.08));
    color: var(--fg);
    font-weight: 600;
    box-shadow: 0 1px 2px rgba(0, 0, 0, 0.2);
  }
  .notice {
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 10px 14px;
    font-size: 13px;
  }
  .notice span {
    flex: 1;
  }
  .error {
    padding: 12px 16px;
    color: var(--danger);
  }
  .list {
    padding: 8px;
    display: flex;
    flex-direction: column;
  }
  .row-wrap {
    display: flex;
    align-items: center;
  }
  .check {
    width: 34px;
    flex-shrink: 0;
    display: inline-flex;
    justify-content: center;
  }
  .grid {
    flex: 1;
    min-width: 0;
    display: grid;
    grid-template-columns: minmax(0, 1fr) 90px 100px 120px 100px;
    gap: 14px;
    align-items: center;
    padding: 8px 14px 8px 6px;
  }
  .cols .grid {
    font-size: 11px;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.06em;
  }
  .sort {
    border: 0;
    background: transparent;
    color: inherit;
    font: inherit;
    letter-spacing: inherit;
    text-transform: inherit;
    text-align: left;
    padding: 4px 0;
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
    width: 10px;
    color: var(--accent);
  }
  .grow-row {
    border: 0;
    border-radius: 12px;
    background: transparent;
    color: var(--fg);
    text-align: left;
    font-size: 13px;
    height: 52px;
  }
  .group > .row-wrap:hover,
  .group.open > .row-wrap {
    background: var(--hover);
    border-radius: 12px;
  }
  .group.open > .row-wrap {
    border-radius: 12px 12px 0 0;
  }
  .group.sent {
    opacity: 0.6;
  }
  .name-cell {
    display: flex;
    align-items: center;
    gap: 12px;
    min-width: 0;
  }
  .tile {
    width: 34px;
    height: 34px;
    border-radius: 9px;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    font-size: 10px;
    font-weight: 700;
    flex-shrink: 0;
  }
  .name {
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    font-weight: 500;
  }
  .sub {
    margin-left: 6px;
    font-size: 11px;
    color: var(--fg-muted);
  }
  .waste {
    font-family: var(--font-display);
    font-weight: 700;
    color: var(--accent);
  }
  .state {
    font-size: 11px;
    color: var(--fg-muted);
  }
  .state.ok {
    color: var(--teal);
  }
  ul {
    list-style: none;
    margin: 0 0 6px;
    padding: 6px 14px 10px 48px;
    background: rgba(255, 255, 255, 0.03);
    border-radius: 0 0 12px 12px;
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  li {
    display: flex;
    align-items: center;
    gap: 12px;
    font-size: 13px;
    min-width: 0;
  }
  li.app {
    opacity: 0.55;
  }
  .radio-gap {
    width: 13px;
    flex-shrink: 0;
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
    width: 150px;
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
  li.keep .path {
    color: var(--fg);
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
  .keep-tag {
    color: var(--teal);
  }
  .when {
    flex-shrink: 0;
  }
  .btn.tiny {
    height: 24px;
    padding: 0 10px;
    font-size: 11px;
  }
  .btn.ghost {
    background: transparent;
    border-color: transparent;
  }
  .btn.small {
    height: 30px;
    font-size: 12px;
    padding: 0 12px;
  }
  .actions {
    margin-top: 4px;
    font-size: 12px;
  }
  .btn.danger {
    background: var(--danger);
    border-color: var(--danger);
    color: #fff;
  }
  .more {
    display: flex;
    justify-content: center;
    padding: 10px;
  }
  input[type="radio"],
  input[type="checkbox"] {
    accent-color: var(--accent);
    margin: 0;
  }
</style>
