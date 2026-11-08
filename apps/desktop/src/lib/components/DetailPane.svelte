<script lang="ts">
  import { untrack } from "svelte";
  import DriveName from "./DriveName.svelte";
  import { api, ops, thumbUrl, type ItemDetail, type SearchHit } from "../api";
  import { bytes, count, dateTime, displayPath, KIND_LABEL, KIND_TILE, metaRows, tileText } from "../format";

  let { hit, query = "" }: { hit: SearchHit | null; query?: string } = $props();

  let detail = $state<ItemDetail | null>(null);
  let copied = $state(false);
  let error = $state<string | null>(null);
  let thumbFailed = $state(false);
  let newTag = $state("");

  async function addTag() {
    const t = newTag.trim();
    if (!hit || !t || !detail) return;
    try {
      detail.tags = await ops.tagItem(hit.volume_id, hit.item_id, hit.item_type, t);
      newTag = "";
    } catch (e) {
      error = String(e);
    }
  }

  async function removeTag(t: string) {
    if (!hit || !detail) return;
    try {
      detail.tags = await ops.untagItem(hit.volume_id, hit.item_id, hit.item_type, t);
    } catch (e) {
      error = String(e);
    }
  }

  /** Which item is shown: a refreshed copy of the same item doesn't reload the pane. */
  const itemKey = $derived(hit ? `${hit.volume_id}:${hit.item_type}:${hit.item_id}:${hit.mtime}:${hit.size}` : "");

  $effect(() => {
    void itemKey;
    const h = untrack(() => hit);
    detail = null;
    error = null;
    thumbFailed = false;
    if (!h) return;
    let cancelled = false;
    api
      .itemDetail(h.volume_id, h.item_id, h.item_type)
      .then((d) => {
        if (!cancelled) detail = d;
      })
      .catch((e) => {
        if (!cancelled) error = String(e);
      });
    return () => {
      cancelled = true;
    };
  });

  const tile = $derived(hit ? (KIND_TILE[hit.kind] ?? KIND_TILE.other) : KIND_TILE.other);
  const showThumb = $derived(!!detail?.has_thumb && !thumbFailed && hit?.item_type === "file");
  const fullPath = $derived.by(() => {
    if (!hit) return "";
    if (detail?.absolute_path) {
      return hit.item_type === "archive_entry" && detail.archive
        ? `${detail.absolute_path} › ${displayPath(detail.archive.entry_path)}`
        : detail.absolute_path;
    }
    const rel = hit.path ? `${hit.path}/${hit.name}` : hit.name;
    return `${hit.volume_label}:\\${displayPath(rel)}`;
  });
  const rows = $derived(metaRows(detail?.meta));

  /** Excerpt around the first query word found in the text, else the start. */
  const excerpt = $derived.by(() => {
    const t = detail?.text_excerpt;
    if (!t) return null;
    const words = query.toLowerCase().split(/[^\p{L}\p{N}]+/u).filter((w) => w.length > 2 && !w.includes(":"));
    const lower = t.toLowerCase();
    let at = -1;
    for (const w of words) {
      at = lower.indexOf(w);
      if (at >= 0) break;
    }
    const start = at < 0 ? 0 : Math.max(0, at - 120);
    const slice = t.slice(start, start + 420).replace(/\s+/g, " ").trim();
    return (start > 0 ? "…" : "") + slice + (start + 420 < t.length ? "…" : "");
  });

  async function copyPath() {
    try {
      await navigator.clipboard.writeText(detail?.absolute_path ?? fullPath);
      copied = true;
      setTimeout(() => (copied = false), 1200);
    } catch (e) {
      error = String(e);
    }
  }

  async function open() {
    if (!hit) return;
    try {
      await api.openItem(hit.volume_id, hit.item_id, hit.item_type);
    } catch (e) {
      error = String(e);
    }
  }

  async function reveal() {
    if (!hit) return;
    try {
      await api.revealItem(hit.volume_id, hit.item_id, hit.item_type);
    } catch (e) {
      error = String(e);
    }
  }
</script>

<aside class="card pane" aria-label="Details">
  {#if !hit}
    <div class="empty muted">Select a result to see where it lives.</div>
  {:else}
    {#if showThumb}
      <img class="preview" src={thumbUrl(hit.volume_id, hit.item_id)} alt="" onerror={() => (thumbFailed = true)} />
    {:else}
      <div class="art" style="background: {tile[0]}">
        <span class="ext" style="color: {tile[1]}">{tileText(hit)}</span>
      </div>
    {/if}
    <div>
      <div class="name">{hit.name}</div>
      <div class="muted kindline">
        {KIND_LABEL[hit.kind]}{hit.is_dir ? "" : ` · ${bytes(hit.size)}`} · <DriveName id={hit.volume_id} label={hit.volume_label} />
        {#if detail?.meta?.width && detail?.meta?.height} · {detail.meta.width} × {detail.meta.height}{/if}
      </div>
    </div>

    {#if detail?.archive}
      <div class="note">
        Inside <strong>{detail.archive.name}</strong>
        <span class="muted">{detail.archive.path ? displayPath(detail.archive.path) : "\\"}</span>
      </div>
    {/if}

    {#if hit.volume_online}
      <div class="status live"><span class="dot"></span>Connected · ready to open</div>
    {:else}
      <div class="offline">
        <svg width="22" height="22" viewBox="0 0 16 16" fill="none" stroke="var(--accent)" stroke-width="1.6" aria-hidden="true">
          <rect x="2" y="3" width="12" height="4.5" rx="1" /><rect x="2" y="8.5" width="12" height="4.5" rx="1" />
        </svg>
        <span>
          <strong>Not plugged in.</strong><br />
          <DriveName id={hit.volume_id} label={hit.volume_label} />{detail?.volume_location ? ` — ${detail.volume_location}` : ""}
        </span>
      </div>
    {/if}

    <div class="actions">
      <button class="btn primary" disabled={!hit.volume_online} onclick={open}>
        {hit.item_type === "archive_entry" ? "Open archive" : "Open"}
      </button>
      <button class="btn" disabled={!hit.volume_online} onclick={reveal}>Reveal</button>
      <button class="btn" onclick={copyPath}>{copied ? "Copied" : "Copy path"}</button>
    </div>

    <dl>
      <dt class="muted">Location</dt>
      <dd class="mono">{fullPath}</dd>
      <dt class="muted">Modified</dt>
      <dd>{dateTime(hit.mtime)}</dd>
      {#if detail && hit.item_type === "file"}
        <dt class="muted">Created</dt>
        <dd>{dateTime(detail.ctime)}</dd>
      {/if}
      {#if detail?.is_dir}
        <dt class="muted">Contains</dt>
        <dd>{count(detail.child_dirs ?? 0)} folders · {count(detail.child_files ?? 0)} files</dd>
      {/if}
      {#if detail?.archive_entries}
        <dt class="muted">Contains</dt>
        <dd>{count(detail.archive_entries)} entries, searchable</dd>
      {/if}
      {#if detail?.placeholder}
        <dt class="muted">Cloud</dt>
        <dd>Online-only placeholder; contents not on disk</dd>
      {/if}
      {#each rows as [k, v] (k)}
        <dt class="muted">{k}</dt>
        <dd>{v}</dd>
      {/each}
      {#if detail?.full_hash}
        <dt class="muted">BLAKE3</dt>
        <dd class="mono">{detail.full_hash.slice(0, 16)}…</dd>
      {:else if detail?.quick_hash}
        <dt class="muted">Quick hash</dt>
        <dd class="mono">{detail.quick_hash.slice(0, 16)}…</dd>
      {/if}
    </dl>

    {#if detail && hit.item_type !== "archive_entry"}
      <div class="tags">
        {#each detail.tags as t (t)}
          <span class="tagchip">#{t}<button aria-label="Remove tag" onclick={() => removeTag(t)}>×</button></span>
        {/each}
        <form class="tagform" onsubmit={(e) => { e.preventDefault(); void addTag(); }}>
          <input bind:value={newTag} placeholder="+ tag" aria-label="Add tag" maxlength="64" />
        </form>
      </div>
    {/if}

    {#if excerpt}
      <div>
        <div class="muted label">Text · {bytes(detail?.text_bytes ?? 0)} indexed</div>
        <div class="excerpt">{excerpt}</div>
      </div>
    {/if}

    {#if error}
      <div class="error">{error}</div>
    {/if}
  {/if}
</aside>

<style>
  .pane {
    padding: 18px;
    display: flex;
    flex-direction: column;
    gap: 16px;
    min-height: 0;
    overflow-y: auto;
  }
  .empty {
    margin: auto;
    text-align: center;
    padding: 40px 0;
  }
  .art,
  .preview {
    height: 170px;
    border-radius: 12px;
    flex-shrink: 0;
  }
  .art {
    display: flex;
    align-items: flex-end;
    padding: 14px;
  }
  .preview {
    width: 100%;
    object-fit: contain;
    background: rgba(0, 0, 0, 0.35);
  }
  .ext {
    font-family: var(--font-display);
    font-size: 40px;
    font-weight: 700;
    line-height: 1;
  }
  .name {
    font-family: var(--font-display);
    font-size: 18px;
    font-weight: 700;
    line-height: 1.25;
    word-break: break-word;
  }
  .kindline {
    margin-top: 6px;
  }
  .note {
    font-size: 13px;
    padding: 10px 12px;
    border-radius: 10px;
    background: rgba(192, 132, 252, 0.12);
    border: 1px solid rgba(192, 132, 252, 0.35);
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .status.live {
    display: flex;
    gap: 8px;
    align-items: center;
    font-size: 13px;
    color: var(--teal);
  }
  .dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--teal);
  }
  .offline {
    display: flex;
    gap: 12px;
    align-items: center;
    padding: 12px 14px;
    background: var(--accent-soft);
    border: 1px solid var(--accent-line);
    border-radius: 12px;
    font-size: 13px;
    line-height: 1.4;
  }
  .actions {
    display: flex;
    gap: 8px;
    flex-wrap: wrap;
  }
  dl {
    display: grid;
    grid-template-columns: 90px minmax(0, 1fr);
    gap: 8px 12px;
    margin: 0;
    font-size: 13px;
    line-height: 1.4;
  }
  dt {
    text-transform: capitalize;
  }
  dd {
    margin: 0;
    word-break: break-word;
  }
  .mono {
    font-family: var(--font-mono);
    font-size: 12px;
    color: var(--fg-soft);
    word-break: break-all;
  }
  .label {
    font-weight: 600;
    margin-bottom: 4px;
  }
  .excerpt {
    font-size: 13px;
    line-height: 1.5;
    color: var(--fg-soft);
    background: rgba(255, 255, 255, 0.04);
    border-radius: 10px;
    padding: 10px 12px;
    white-space: pre-wrap;
  }
  .error {
    color: var(--danger);
    font-size: 12px;
  }
  .tags {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    align-items: center;
  }
  .tagchip {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    height: 26px;
    padding: 0 6px 0 10px;
    border-radius: 13px;
    background: rgba(94, 224, 200, 0.14);
    color: var(--teal);
    font-size: 12px;
    font-weight: 600;
  }
  .tagchip button {
    all: unset;
    cursor: pointer;
    width: 16px;
    height: 16px;
    border-radius: 50%;
    text-align: center;
    line-height: 16px;
    color: var(--teal);
    opacity: 0.7;
  }
  .tagchip button:hover {
    opacity: 1;
    background: rgba(94, 224, 200, 0.2);
  }
  .tagform input {
    width: 90px;
    height: 26px;
    border: 1px dashed var(--line-strong);
    border-radius: 13px;
    background: transparent;
    padding: 0 10px;
    font-size: 12px;
    outline: 0;
  }
  .tagform input:focus {
    border-color: var(--accent-line);
    width: 140px;
  }
</style>
