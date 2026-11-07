<script lang="ts">
  import { FLAG_HAS_THUMB, FLAG_HAS_TEXT, FLAG_PLACEHOLDER, thumbUrl, type SearchHit } from "../api";
  import { bytes, date, displayPath, KIND_TILE, tileText } from "../format";

  let {
    hit,
    selected = false,
    onselect,
    onopen,
  }: {
    hit: SearchHit;
    selected?: boolean;
    onselect: () => void;
    onopen: () => void;
  } = $props();

  const tile = $derived(KIND_TILE[hit.kind] ?? KIND_TILE.other);
  const hasThumb = $derived((hit.flags & FLAG_HAS_THUMB) !== 0 && hit.item_type === "file");
  let thumbFailed = $state(false);
</script>

<button
  class="row"
  class:on={selected}
  onclick={onselect}
  ondblclick={onopen}
  aria-selected={selected}
  role="option"
>
  <span class="name-cell">
    {#if hasThumb && !thumbFailed}
      <img class="thumb" src={thumbUrl(hit.volume_id, hit.item_id)} alt="" loading="lazy" decoding="async" onerror={() => (thumbFailed = true)} />
    {:else}
      <span class="tile" style="background: {tile[0]}; color: {tile[1]}">{tileText(hit)}</span>
    {/if}
    <span class="text">
      <span class="name">
        {hit.name}
        {#if hit.item_type === "archive_entry"}<span class="tag" title="Inside an archive">zip</span>{/if}
        {#if hit.flags & FLAG_HAS_TEXT}<span class="tag soft" title="Text indexed">T</span>{/if}
        {#if hit.flags & FLAG_PLACEHOLDER}<span class="tag soft" title="Cloud placeholder, not on disk">☁</span>{/if}
      </span>
      <span class="path muted">{hit.path ? displayPath(hit.path) : "\\"}</span>
    </span>
  </span>
  <span class="drive">
    <span class="dot" class:live={hit.volume_online}></span>
    <span class="label">{hit.volume_label}</span>
  </span>
  <span class="size">{hit.is_dir ? "" : bytes(hit.size)}</span>
  <span class="when muted">{date(hit.mtime)}</span>
</button>

<style>
  .row {
    flex: 1;
    display: grid;
    grid-template-columns: minmax(0, 1fr) 170px 86px 110px;
    gap: 14px;
    align-items: center;
    padding: 0 14px;
    border: 0;
    border-radius: 12px;
    background: transparent;
    color: var(--fg);
    text-align: left;
    min-width: 0;
  }
  .row:hover {
    background: var(--hover);
  }
  .row.on {
    background: var(--accent-soft);
    box-shadow: inset 0 0 0 1px var(--accent-line);
  }
  .name-cell {
    display: flex;
    align-items: center;
    gap: 14px;
    min-width: 0;
  }
  .tile,
  .thumb {
    width: 40px;
    height: 40px;
    border-radius: var(--radius-tile);
    flex-shrink: 0;
  }
  .tile {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    font-size: 10px;
    font-weight: 700;
    letter-spacing: 0.06em;
  }
  .thumb {
    object-fit: cover;
    background: rgba(255, 255, 255, 0.05);
  }
  .text {
    display: flex;
    flex-direction: column;
    min-width: 0;
    gap: 1px;
  }
  .name,
  .path,
  .label {
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .name {
    font-weight: 500;
  }
  .tag {
    display: inline-block;
    margin-left: 6px;
    padding: 0 5px;
    border-radius: 4px;
    background: var(--purple);
    color: var(--on-accent);
    font-size: 10px;
    font-weight: 700;
    line-height: 15px;
    vertical-align: 2px;
  }
  .tag.soft {
    background: rgba(255, 255, 255, 0.1);
    color: var(--fg-muted);
  }
  .drive {
    display: flex;
    align-items: center;
    gap: 7px;
    min-width: 0;
    font-size: 13px;
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
  .size {
    font-size: 13px;
    font-variant-numeric: tabular-nums;
  }
</style>
