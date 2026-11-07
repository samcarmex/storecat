<script lang="ts">
  import { ops, volumePhotoUrl, type Role, type Volume } from "../api";
  import type { AppStore } from "../store.svelte";
  import { ago, busLabel, bytes, count, duration, HEALTH_COLOR, HEALTH_LABEL, MEDIA_COLOR, MEDIA_LABEL } from "../format";

  let { volume: v, store }: { volume: Volume; store: AppStore } = $props();

  const ROLES: { id: Role; label: string }[] = [
    { id: "unassigned", label: "No role" },
    { id: "working", label: "Working" },
    { id: "media", label: "Media" },
    { id: "archive", label: "Archive" },
    { id: "backup", label: "Backup" },
    { id: "system", label: "System" },
  ];

  let confirmRemove = $state(false);
  let editing = $state(false);
  let menu = $state(false);
  let draftLabel = $state("");
  let draftLocation = $state("");
  let draftNotes = $state("");
  let exported = $state<string | null>(null);

  const progress = $derived(store.progress[v.id]);
  const enrich = $derived(store.enrich[v.id]);
  const cov = $derived(store.coverage[v.id]);
  const scanning = $derived(store.isScanning(v.id));
  const enriching = $derived(store.isEnriching(v.id));
  const color = $derived(MEDIA_COLOR[v.media_type]);
  const used = $derived(Math.max(0, v.capacity - v.free));
  const usedPct = $derived(v.capacity > 0 ? Math.min(100, (used / v.capacity) * 100) : 0);
  const freeText = $derived(v.capacity > 0 ? bytes(v.free) : bytes(v.total_bytes));
  const pending = $derived(cov ? Math.max(0, cov.files - cov.done) : 0);
  const covPct = $derived(cov && cov.files > 0 ? Math.round((cov.done / cov.files) * 100) : 0);
  const profile = $derived([v.bus ? busLabel(v.bus) : "", v.model].filter(Boolean).join(" · "));
  /** Another catalogued volume that already contains this one (its files appear twice). */
  const insideOf = $derived.by(() => {
    const norm = (p: string) => p.replaceAll("\\", "/").replace(/\/+$/, "").toLowerCase();
    const mine = v.mount_path ?? v.last_mount_path;
    if (!mine) return null;
    const me = norm(mine);
    return (
      store.volumes.find((o) => {
        if (o.id === v.id) return false;
        if (o.source === "device" && !o.online) return false;
        const op = o.mount_path ?? o.last_mount_path;
        return !!op && me.startsWith(norm(op) + "/");
      }) ?? null
    );
  });

  function startEdit() {
    draftLabel = v.label;
    draftLocation = v.location;
    draftNotes = v.notes;
    editing = true;
  }

  async function saveEdit() {
    editing = false;
    await store.updateVolume(v.id, { label: draftLabel.trim() || v.label, location: draftLocation, notes: draftNotes });
  }

  function askRemove() {
    menu = false;
    confirmRemove = true;
    setTimeout(() => (confirmRemove = false), 4000);
  }

  async function doExport() {
    menu = false;
    const p = await store.exportVolume(v.id);
    if (p) {
      exported = p;
      setTimeout(() => (exported = null), 6000);
    }
  }
</script>

<article class="card" style="--c: {color}">
  <header>
    <span class="type">{MEDIA_LABEL[v.media_type]}</span>
    <select class="role" value={v.role} onchange={(e) => store.updateVolume(v.id, { role: (e.currentTarget as HTMLSelectElement).value as Role })} aria-label="Drive role">
      {#each ROLES as r (r.id)}
        <option value={r.id}>{r.label}</option>
      {/each}
    </select>
    <span class="status muted">
      <span class="dot" class:live={v.online}></span>
      {v.online ? "Connected" : `Seen ${ago(v.last_seen_at)}`}
    </span>
  </header>

  {#if v.photo && !editing}
    <img class="photo" src={volumePhotoUrl(v.id) + "?t=" + v.last_seen_at} alt="" />
  {/if}

  {#if editing}
    <div class="edit">
      <input bind:value={draftLabel} placeholder="Name" aria-label="Drive name" />
      <input bind:value={draftLocation} placeholder="Where it lives (top drawer, grey case…)" aria-label="Physical location" />
      <input bind:value={draftNotes} placeholder="Notes" aria-label="Notes" />
      <div class="row">
        <button class="btn primary small" onclick={saveEdit}>Save</button>
        <button class="btn small" onclick={() => (editing = false)}>Cancel</button>
      </div>
    </div>
  {:else}
    <button class="title" onclick={startEdit} title="Edit name, location and notes">
      <span class="name">{v.label}</span>
      {#if v.mount_path}<span class="muted mount">{v.mount_path}</span>{:else if v.location}<span class="muted mount">{v.location}</span>{/if}
    </button>
    <div class="big">
      <span class="num">{freeText}</span>
      <span class="muted">
        {#if v.capacity > 0}free of {bytes(v.capacity)}{:else}catalogued{/if}
        · {count(v.file_count)} files
      </span>
    </div>
    <div class="bar"><div style="width: {usedPct}%"></div></div>
    {#if profile || v.health !== "unknown"}
      <div class="profile muted">
        {#if profile}<span class="model" title={profile}>{profile}</span>{/if}
        {#if v.health !== "unknown"}
          <span class="health" style="--h: {HEALTH_COLOR[v.health]}" title="S.M.A.R.T. status reported by the operating system{v.health_at ? `, checked ${ago(v.health_at)}` : ''}">
            {HEALTH_LABEL[v.health]}
          </span>
        {/if}
      </div>
    {/if}
    {#if insideOf}
      <div class="overlap">Inside {insideOf.label}: these files are catalogued twice.</div>
    {/if}
  {/if}

  {#if progress && (scanning || progress.phase === "done" || progress.phase === "failed")}
    <div class="scan" class:failed={progress.phase === "failed"}>
      {#if progress.phase === "walking"}
        <span class="spin"></span>
        <span>Scanning · {count(progress.files)} files · {bytes(progress.bytes)} · {duration(progress.elapsed_ms)}</span>
        <span class="muted cur">{progress.current || "…"}</span>
      {:else if progress.phase === "committing"}
        <span class="spin"></span><span>Writing index…</span>
      {:else if progress.phase === "done"}
        <span>Scanned · {count(progress.files)} files in {duration(progress.elapsed_ms)}</span>
      {:else if progress.phase === "failed"}
        <span>Scan failed: {progress.message}</span>
      {/if}
    </div>
  {:else if enrich && (enriching || enrich.phase !== "running")}
    <div class="scan" class:failed={enrich.phase === "failed"}>
      {#if enrich.phase === "running"}
        <span class="spin"></span>
        <span>Enriching {count(enrich.done)} / {count(enrich.total)} · {count(enrich.thumbs)} thumbs · {count(enrich.texts)} texts</span>
        <span class="muted cur">{enrich.current || "…"}</span>
      {:else if enrich.phase === "done"}
        <span>Enriched · {count(enrich.done)} files in {duration(enrich.elapsed_ms)}</span>
      {:else if enrich.phase === "paused"}
        <span>Enrichment paused · resumes when connected</span>
      {:else if enrich.phase === "failed"}
        <span>Enrichment failed: {enrich.message}</span>
      {/if}
    </div>
  {:else}
    <div class="meta muted">
      {#if v.last_scan_at}
        Scanned {ago(v.last_scan_at)}
        {#if cov && cov.files > 0}
          · {covPct}% enriched · {count(cov.thumbs)} thumbs · {count(cov.texts)} texts
        {/if}
      {:else}
        Not scanned yet
      {/if}
    </div>
  {/if}

  {#if exported}
    <div class="muted small-note">Exported to {exported}</div>
  {/if}

  <footer>
    <button class="btn small" disabled={!v.online || scanning || enriching} onclick={() => store.startScan(v.id)} title={v.online ? "" : "Connect the drive to rescan"}>
      {v.last_scan_at ? "Rescan" : "Scan"}
    </button>
    {#if enriching}
      <button class="btn small ghost" onclick={() => store.cancelEnrich(v.id)}>Pause</button>
    {:else if v.online && v.last_scan_at && pending > 0 && !scanning}
      <button class="btn small" onclick={() => store.startEnrich(v.id)} title="Hashes, metadata, thumbnails, text">
        Enrich {count(pending)}
      </button>
    {/if}
    <span class="grow"></span>
    {#if confirmRemove}
      <button class="btn small danger" onclick={() => store.removeVolume(v.id)}>Confirm remove</button>
    {:else}
      <div class="menu-wrap">
        <button class="btn small ghost" aria-label="More" onclick={() => (menu = !menu)}>···</button>
        {#if menu}
          <div class="menu" role="menu">
            <button role="menuitem" onclick={async () => { menu = false; await ops.setVolumePhoto(v.id); await store.refresh(); }}>{v.photo ? "Change photo…" : "Add a photo of the drive…"}</button>
            {#if v.photo}
              <button role="menuitem" onclick={async () => { menu = false; await ops.clearVolumePhoto(v.id); await store.refresh(); }}>Remove photo</button>
            {/if}
            <button role="menuitem" onclick={doExport} disabled={!v.last_scan_at}>Export catalogue…</button>
            <button role="menuitem" onclick={() => { menu = false; store.startEnrich(v.id); }} disabled={!v.online || !v.last_scan_at || enriching}>Enrich now</button>
            <button role="menuitem" class="danger-text" onclick={askRemove} disabled={scanning || enriching}>Remove from catalogue</button>
          </div>
        {/if}
      </div>
    {/if}
  </footer>
</article>

<style>
  .card {
    padding: 16px 18px;
    display: flex;
    flex-direction: column;
    gap: 10px;
    background: color-mix(in srgb, var(--c) 6%, var(--surface));
    border-color: color-mix(in srgb, var(--c) 22%, transparent);
    position: relative;
  }
  header {
    display: flex;
    align-items: center;
    gap: 10px;
  }
  .type {
    font-family: var(--font-display);
    font-size: 12px;
    font-weight: 700;
    letter-spacing: 0.08em;
    color: var(--c);
  }
  .role {
    background: transparent;
    border: 1px solid var(--line-strong);
    border-radius: 10px;
    color: var(--fg-muted);
    font: inherit;
    font-size: 11px;
    padding: 2px 6px;
    outline: 0;
  }
  .role option {
    background: var(--surface-2);
  }
  .status {
    margin-left: auto;
    display: inline-flex;
    align-items: center;
    gap: 6px;
    white-space: nowrap;
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
  .photo {
    width: 100%;
    height: 110px;
    object-fit: cover;
    border-radius: 10px;
  }
  .title {
    all: unset;
    cursor: text;
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-width: 0;
  }
  .title:hover .name {
    text-decoration: underline dotted;
    text-underline-offset: 4px;
  }
  .name {
    font-family: var(--font-display);
    font-size: 20px;
    font-weight: 700;
    line-height: 1.2;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .mount {
    font-size: 12px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .big {
    display: flex;
    align-items: baseline;
    gap: 8px;
    flex-wrap: wrap;
  }
  .num {
    font-family: var(--font-display);
    font-size: 28px;
    font-weight: 700;
    color: var(--c);
    line-height: 1;
  }
  .bar {
    height: 8px;
    border-radius: 4px;
    background: rgba(255, 255, 255, 0.08);
    overflow: hidden;
  }
  .bar div {
    height: 100%;
    background: var(--c);
    border-radius: 4px;
    transition: width 0.4s;
  }
  .meta,
  .scan {
    font-size: 12px;
    min-height: 18px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .scan {
    display: flex;
    align-items: center;
    gap: 8px;
    color: var(--fg);
  }
  .scan.failed {
    color: var(--danger);
    white-space: normal;
  }
  .cur {
    overflow: hidden;
    text-overflow: ellipsis;
    min-width: 0;
  }
  .small-note {
    font-size: 11px;
    word-break: break-all;
  }
  .spin {
    width: 12px;
    height: 12px;
    border-radius: 50%;
    border: 2px solid rgba(255, 255, 255, 0.15);
    border-top-color: var(--accent);
    animation: spin 0.8s linear infinite;
    flex-shrink: 0;
  }
  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }
  footer {
    display: flex;
    gap: 8px;
    align-items: center;
    margin-top: 4px;
  }
  .grow {
    flex: 1;
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
  .btn.danger {
    background: var(--danger);
    border-color: var(--danger);
    color: #fff;
  }
  .menu-wrap {
    position: relative;
  }
  .menu {
    position: absolute;
    right: 0;
    bottom: 36px;
    min-width: 200px;
    background: var(--surface-2);
    border: 1px solid var(--line-strong);
    border-radius: 12px;
    padding: 6px;
    display: flex;
    flex-direction: column;
    z-index: 5;
    box-shadow: 0 12px 30px rgba(0, 0, 0, 0.4);
  }
  .menu button {
    all: unset;
    cursor: pointer;
    padding: 8px 10px;
    border-radius: 8px;
    font-size: 13px;
  }
  .menu button:hover {
    background: var(--hover);
  }
  .menu button[disabled] {
    opacity: 0.4;
    cursor: default;
  }
  .danger-text {
    color: var(--danger);
  }
  .edit {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .edit input {
    background: rgba(0, 0, 0, 0.25);
    border: 1px solid var(--line-strong);
    border-radius: 8px;
    padding: 7px 10px;
    font-size: 13px;
    outline: 0;
  }
  .edit input:focus {
    border-color: var(--accent-line);
  }
  .edit .row {
    display: flex;
    gap: 6px;
  }
  .profile {
    display: flex;
    align-items: center;
    gap: 8px;
    min-width: 0;
  }
  .model {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    min-width: 0;
  }
  .health {
    margin-left: auto;
    flex: none;
    color: var(--h);
    border: 1px solid color-mix(in srgb, var(--h) 45%, transparent);
    border-radius: var(--radius-pill);
    padding: 1px 8px;
    font-size: 11px;
    font-weight: 600;
  }
  .overlap {
    font-size: 12px;
    color: var(--amber);
  }
</style>
