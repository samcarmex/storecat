<script lang="ts">
  import { api, ops, type DupGroup, type DupSummary } from "../api";
  import type { AppStore } from "../store.svelte";
  import { bytes, count, date, displayPath, KIND_TILE } from "../format";
  import EmptyState from "../components/EmptyState.svelte";

  let { store }: { store: AppStore } = $props();

  let groups = $state<DupGroup[]>([]);
  let summary = $state<DupSummary>({ groups: 0, files: 0, waste: 0 });
  let loading = $state(false);
  let open = $state<Set<string>>(new Set());
  let verifying = $state<Set<string>>(new Set());
  let error = $state<string | null>(null);

  async function load() {
    loading = true;
    try {
      [groups, summary] = await Promise.all([api.duplicates(200, 0), api.duplicateSummary()]);
      error = null;
    } catch (e) {
      error = String(e);
    } finally {
      loading = false;
    }
  }

  $effect(() => {
    void store.catalogVersion;
    void load();
  });

  function toggle(hash: string) {
    const next = new Set(open);
    if (next.has(hash)) next.delete(hash);
    else next.add(hash);
    open = next;
  }

  async function verify(g: DupGroup) {
    verifying = new Set([...verifying, g.hash]);
    try {
      const updated = await api.verifyGroup(g.hash);
      groups = groups.map((x) => (x.hash === g.hash ? updated : x));
    } catch (e) {
      error = String(e);
    } finally {
      const next = new Set(verifying);
      next.delete(g.hash);
      verifying = next;
    }
  }

  const anyHashed = $derived(Object.values(store.coverage).some((c) => c.hashed > 0));

  let keep = $state<Record<string, string>>({});
  let cleaning = $state<Set<string>>(new Set());
  let cleaned = $state<Record<string, string>>({});

  function keepKey(g: DupGroup): string {
    return keep[g.hash] ?? `${g.members[0].volume_id}:${g.members[0].file_id}`;
  }

  async function cleanup(g: DupGroup) {
    const [vid, fid] = keepKey(g).split(":").map(Number);
    cleaning = new Set([...cleaning, g.hash]);
    try {
      const job = await ops.duplicateCleanup(g.hash, vid, fid);
      cleaned[g.hash] = `Job #${job.id} started: ${job.files_total} cop${job.files_total === 1 ? "y" : "ies"} to the recycle bin.`;
    } catch (e) {
      error = String(e);
    } finally {
      const next = new Set(cleaning);
      next.delete(g.hash);
      cleaning = next;
    }
  }
</script>

<div class="head">
  <h1>Same file, <em>several shelves.</em></h1>
  <span class="muted">
    {#if summary.groups > 0}
      {count(summary.groups)} groups · {count(summary.files)} files · {bytes(summary.waste)} reclaimable
    {:else}
      Files over the size threshold that share a quick hash across your drives
    {/if}
  </span>
</div>

{#if error}
  <div class="error card">{error}</div>
{/if}

{#if groups.length === 0 && !loading}
  <EmptyState
    title={anyHashed ? "No duplicates found" : "No hashes yet"}
    body={anyHashed
      ? "Every hashed file above the threshold is unique across your catalogued drives."
      : "Duplicates appear once drives have been enriched. Enrichment runs automatically after a scan while the drive is connected; you can also start it from a drive card."}
  />
{:else}
  <section class="card list" aria-label="Duplicate groups">
    <div class="cols muted"><span>File</span><span>Copies</span><span>Each</span><span>Reclaimable</span><span></span></div>
    {#each groups as g (g.hash)}
      {@const tile = KIND_TILE[g.kind] ?? KIND_TILE.other}
      <div class="group" class:open={open.has(g.hash)}>
        <button class="grow-row" onclick={() => toggle(g.hash)} aria-expanded={open.has(g.hash)}>
          <span class="name-cell">
            <span class="tile" style="background: {tile[0]}; color: {tile[1]}">{g.name.split(".").pop()?.toUpperCase().slice(0, 4) ?? "·"}</span>
            <span class="name">{g.name}</span>
          </span>
          <span>{g.count} ×</span>
          <span>{bytes(g.size)}</span>
          <span class="waste">{bytes(g.waste)}</span>
          <span class="state" class:ok={g.verified}>{g.verified ? "verified" : "quick hash"}</span>
        </button>
        {#if open.has(g.hash)}
          <ul>
            {#each g.members as m (m.volume_id + ":" + m.file_id)}
              <li>
                {#if g.verified}
                  <input type="radio" name={"keep-" + g.hash} value={m.volume_id + ":" + m.file_id} checked={keepKey(g) === m.volume_id + ":" + m.file_id} onchange={() => (keep[g.hash] = m.volume_id + ":" + m.file_id)} aria-label="Keep this copy" title="Keep this copy" />
                {/if}
                <span class="dot" class:live={m.volume_online}></span>
                <span class="vol">{m.volume_label}</span>
                <span class="path mono">{m.path ? displayPath(m.path) + "\\" : ""}{m.name}</span>
                <span class="muted">{date(m.mtime)}</span>
                <button class="btn tiny" disabled={!m.volume_online} onclick={() => api.revealItem(m.volume_id, m.file_id, "file")}>Reveal</button>
              </li>
            {/each}
            <li class="actions">
              {#if !g.verified}
                <button class="btn small" disabled={verifying.has(g.hash) || !g.members.some((m) => m.volume_online)} onclick={() => verify(g)}>
                  {verifying.has(g.hash) ? "Hashing…" : "Verify (full hash)"}
                </button>
                <span class="muted">Reads every connected copy end to end. Offline copies verify when their drive is plugged in.</span>
              {:else if cleaned[g.hash]}
                <span class="muted">{cleaned[g.hash]}</span>
              {:else}
                <button class="btn small danger" disabled={cleaning.has(g.hash) || !g.members.every((m) => m.volume_online || keepKey(g) === m.volume_id + ":" + m.file_id)} onclick={() => cleanup(g)}>
                  {cleaning.has(g.hash) ? "Working…" : "Recycle the other copies"}
                </button>
                <span class="muted">All copies are identical. Pick the one to keep; the rest go to the recycle bin (every copy to remove must be connected).</span>
              {/if}
            </li>
          </ul>
        {/if}
      </div>
    {/each}
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
  .error {
    padding: 12px 16px;
    color: var(--danger);
  }
  .list {
    padding: 8px;
    display: flex;
    flex-direction: column;
  }
  .cols,
  .grow-row {
    display: grid;
    grid-template-columns: minmax(0, 1fr) 80px 100px 120px 100px;
    gap: 14px;
    align-items: center;
    padding: 8px 14px;
  }
  .cols {
    font-size: 11px;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.06em;
  }
  .grow-row {
    width: 100%;
    border: 0;
    border-radius: 12px;
    background: transparent;
    color: var(--fg);
    text-align: left;
    font-size: 13px;
    height: 52px;
  }
  .grow-row:hover {
    background: var(--hover);
  }
  .group.open .grow-row {
    background: var(--hover);
    border-radius: 12px 12px 0 0;
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
    padding: 6px 14px 10px 60px;
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
    width: 160px;
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
  .actions {
    margin-top: 4px;
    font-size: 12px;
  }
  .btn.danger {
    background: var(--danger);
    border-color: var(--danger);
    color: #fff;
  }
  input[type="radio"] {
    accent-color: var(--accent);
    margin: 0;
  }
</style>
