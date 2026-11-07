<script lang="ts">
  import { onMount } from "svelte";
  import { api, ops, thumbUrl, FLAG_HAS_THUMB, type SearchHit } from "../api";
  import { bytes, date, displayPath, KIND_TILE, tileText } from "../format";
  import Logo from "../components/Logo.svelte";

  let query = $state("");
  let hits = $state<SearchHit[]>([]);
  let total = $state(0);
  let selected = $state(0);
  let input: HTMLInputElement | undefined = $state();
  let seq = 0;
  let timer: ReturnType<typeof setTimeout> | null = null;

  async function run(q: string) {
    const my = ++seq;
    if (!q.trim()) {
      hits = [];
      total = 0;
      return;
    }
    const r = await api.search(q, { limit: 8 });
    if (my !== seq) return;
    hits = r.hits;
    total = r.total;
    selected = 0;
  }

  $effect(() => {
    const q = query;
    if (timer) clearTimeout(timer);
    timer = setTimeout(() => run(q), 30);
  });

  function reset() {
    query = "";
    hits = [];
    selected = 0;
    input?.focus();
  }

  async function openSelected() {
    const h = hits[selected];
    if (!h) return;
    if (h.volume_online) {
      await api.openItem(h.volume_id, h.item_id, h.item_type);
      await ops.hidePalette();
    } else {
      await ops.openInMain(query);
    }
  }

  function onKey(e: KeyboardEvent) {
    if (e.key === "Escape") {
      e.preventDefault();
      void ops.hidePalette();
    } else if (e.key === "ArrowDown") {
      e.preventDefault();
      selected = Math.min(hits.length - 1, selected + 1);
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      selected = Math.max(0, selected - 1);
    } else if (e.key === "Enter") {
      e.preventDefault();
      if (e.ctrlKey || e.metaKey || e.shiftKey) void ops.openInMain(query);
      else void openSelected();
    }
  }

  onMount(() => {
    input?.focus();
    const un: Array<() => void> = [];
    ops.onPaletteShown(() => {
      reset();
    }).then((u) => un.push(u));
    return () => un.forEach((u) => u());
  });
</script>

<div class="palette" role="dialog" aria-label="Quick search">
  <div class="box">
    <Logo size={26} />
    <input
      bind:this={input}
      bind:value={query}
      onkeydown={onKey}
      placeholder="Search every drive…"
      autocomplete="off"
      spellcheck="false"
      aria-label="Search"
    />
    <kbd>Esc</kbd>
  </div>
  {#if hits.length}
    <ul class="results" role="listbox">
      {#each hits as h, i (h.volume_id + ":" + h.item_type + ":" + h.item_id)}
        {@const tile = KIND_TILE[h.kind] ?? KIND_TILE.other}
        <li>
          <button class="row" class:on={i === selected} role="option" aria-selected={i === selected} onclick={() => { selected = i; void openSelected(); }} onmouseenter={() => (selected = i)}>
            {#if h.flags & FLAG_HAS_THUMB && h.item_type === "file"}
              <img class="thumb" src={thumbUrl(h.volume_id, h.item_id)} alt="" loading="lazy" />
            {:else}
              <span class="tile" style="background: {tile[0]}; color: {tile[1]}">{tileText(h)}</span>
            {/if}
            <span class="text">
              <span class="name">{h.name}</span>
              <span class="muted path">{h.volume_label}{h.path ? ` · ${displayPath(h.path)}` : ""}</span>
            </span>
            <span class="meta muted">{h.is_dir ? "" : bytes(h.size)} · {date(h.mtime)}</span>
            <span class="dot" class:live={h.volume_online} title={h.volume_online ? "Connected" : "Not connected"}></span>
          </button>
        </li>
      {/each}
    </ul>
    <div class="foot muted">
      {total > hits.length ? `${total.toLocaleString()} results · ` : ""}Enter opens · Shift+Enter opens in Storecat · ○ drive not connected
    </div>
  {:else if query.trim()}
    <div class="foot muted">No results. Shift+Enter searches in Storecat.</div>
  {/if}
</div>

<style>
  :global(html), :global(body) { background: transparent !important; }
  .palette {
    margin: 10px;
    background: var(--surface);
    border: 1px solid var(--line-strong);
    border-radius: 18px;
    box-shadow: 0 24px 60px rgba(0, 0, 0, 0.55);
    display: flex;
    flex-direction: column;
    overflow: hidden;
  }
  .box { display: flex; align-items: center; gap: 12px; padding: 14px 18px; }
  input { flex: 1; min-width: 0; border: 0; outline: 0; background: transparent; font-size: 20px; color: #fff; }
  input::placeholder { color: var(--fg-muted); }
  kbd { font-family: var(--font-body); border: 1px solid var(--line-strong); border-radius: 6px; padding: 2px 7px; font-size: 11px; color: var(--fg-muted); }
  .results { list-style: none; margin: 0; padding: 6px; border-top: 1px solid var(--line); display: flex; flex-direction: column; }
  .row { width: 100%; display: flex; align-items: center; gap: 12px; padding: 7px 10px; border: 0; border-radius: 10px; background: transparent; color: var(--fg); text-align: left; min-width: 0; }
  .row.on { background: var(--accent-soft); box-shadow: inset 0 0 0 1px var(--accent-line); }
  .tile, .thumb { width: 34px; height: 34px; border-radius: 8px; flex-shrink: 0; }
  .tile { display: inline-flex; align-items: center; justify-content: center; font-size: 9px; font-weight: 700; letter-spacing: .06em; }
  .thumb { object-fit: cover; }
  .text { flex: 1; min-width: 0; display: flex; flex-direction: column; }
  .name, .path { white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .name { font-weight: 500; font-size: 14px; }
  .path { font-size: 12px; }
  .meta { font-size: 12px; white-space: nowrap; }
  .dot { width: 8px; height: 8px; border-radius: 50%; background: var(--kind-other); flex-shrink: 0; }
  .dot.live { background: var(--teal); }
  .foot { padding: 8px 18px 12px; font-size: 11px; border-top: 1px solid var(--line); }
</style>
