<script lang="ts" module>
  export interface TreemapTile {
    key: string;
    value: number;
    label: string;
    color: string;
    /** Folders can be opened; files and the "everything else" block cannot. */
    dirId: number | null;
    kind: "dir" | "file" | "rest";
    detail: string;
  }

  type Placed = TreemapTile & { x: number; y: number; w: number; h: number };

  /** Squarified treemap layout (Bruls, Huizing, van Wijk 2000). */
  export function squarify(items: TreemapTile[], x: number, y: number, w: number, h: number): Placed[] {
    const out: Placed[] = [];
    const total = items.reduce((a, i) => a + Math.max(0, i.value), 0);
    if (total <= 0 || w <= 0 || h <= 0) return out;
    const scale = (w * h) / total;
    let rest = items
      .filter((i) => i.value > 0)
      .sort((a, b) => b.value - a.value)
      .map((item) => ({ item, area: item.value * scale }));
    const worst = (row: { area: number }[], side: number) => {
      let sum = 0;
      let max = 0;
      let min = Infinity;
      for (const r of row) {
        sum += r.area;
        max = Math.max(max, r.area);
        min = Math.min(min, r.area);
      }
      const s2 = sum * sum;
      const side2 = side * side;
      return Math.max((side2 * max) / s2, s2 / (side2 * min));
    };
    while (rest.length) {
      const side = Math.min(w, h);
      const row = [rest[0]];
      let i = 1;
      while (i < rest.length && worst([...row, rest[i]], side) <= worst(row, side)) {
        row.push(rest[i]);
        i++;
      }
      rest = rest.slice(i);
      const rowArea = row.reduce((a, r) => a + r.area, 0);
      if (w >= h) {
        const cw = rowArea / h;
        let cy = y;
        for (const r of row) {
          const rh = r.area / cw;
          out.push({ ...r.item, x, y: cy, w: cw, h: rh });
          cy += rh;
        }
        x += cw;
        w -= cw;
      } else {
        const rh = rowArea / w;
        let cx = x;
        for (const r of row) {
          const rw = r.area / rh;
          out.push({ ...r.item, x: cx, y, w: rw, h: rh });
          cx += rw;
        }
        y += rh;
        h -= rh;
      }
    }
    return out;
  }
</script>

<script lang="ts">
  let {
    tiles,
    onOpen,
    height = 420,
  }: { tiles: TreemapTile[]; onOpen: (dirId: number) => void; height?: number } = $props();

  let width = $state(0);

  /** Dark ink on light tiles, light ink on dark ones. */
  function ink(hex: string): string {
    const m = /^#?([0-9a-f]{2})([0-9a-f]{2})([0-9a-f]{2})$/i.exec(hex);
    if (!m) return "#0f1117";
    const [r, g, b] = m.slice(1).map((x) => parseInt(x, 16) / 255);
    return 0.2126 * r + 0.7152 * g + 0.0722 * b > 0.4 ? "#0f1117" : "#f2f3f5";
  }
  let hover = $state<TreemapTile | null>(null);
  const placed = $derived(squarify(tiles, 0, 0, width, height));
</script>

<div class="map" style="height: {height}px" bind:clientWidth={width} role="group" aria-label="Folder treemap" onmouseleave={() => (hover = null)}>
  {#each placed as t (t.key)}
    {@const big = t.w > 74 && t.h > 38}
    {#if t.dirId !== null}
      <button
        class="tile dir"
        style="left:{t.x}px; top:{t.y}px; width:{t.w}px; height:{t.h}px; --c:{t.color}; color:{ink(t.color)}"
        title="{t.label} · {t.detail}"
        onclick={() => onOpen(t.dirId!)}
        onmouseenter={() => (hover = t)}
        onfocus={() => (hover = t)}
      >
        {#if big}<span class="name">{t.label}</span><span class="size">{t.detail.split(" · ")[0]}</span>{/if}
      </button>
    {:else}
      <div
        class="tile {t.kind}"
        role="img"
        aria-label="{t.label}, {t.detail}"
        style="left:{t.x}px; top:{t.y}px; width:{t.w}px; height:{t.h}px; --c:{t.color}"
        title="{t.label} · {t.detail}"
        onmouseenter={() => (hover = t)}
      >
        {#if big}<span class="name">{t.label}</span><span class="size">{t.detail.split(" · ")[0]}</span>{/if}
      </div>
    {/if}
  {/each}
</div>
<div class="info" aria-live="polite">
  {#if hover}
    <strong>{hover.label}</strong>
    <span class="muted">{hover.detail}</span>
    {#if hover.dirId !== null}<span class="hint">Click to open</span>{/if}
  {:else}
    <span class="muted">Hover a block for details. Click a folder to open it.</span>
  {/if}
</div>

<style>
  .map {
    position: relative;
    width: 100%;
    border-radius: 12px;
    overflow: hidden;
    background: rgba(255, 255, 255, 0.03);
  }
  .tile {
    position: absolute;
    box-sizing: border-box;
    border: 1px solid var(--bg);
    padding: 6px 8px;
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    justify-content: flex-start;
    gap: 2px;
    overflow: hidden;
    text-align: left;
    font: inherit;
    color: #0f1117;
    background: color-mix(in srgb, var(--c) 78%, #0f1117);
    border-radius: 6px;
    transition: filter 0.12s;
  }
  .tile.file {
    background: color-mix(in srgb, var(--c) 38%, #171a22);
    color: var(--fg);
  }
  .tile.rest {
    background: repeating-linear-gradient(135deg, rgba(255, 255, 255, 0.06) 0 6px, rgba(255, 255, 255, 0.02) 6px 12px);
    color: var(--fg-muted);
  }
  .tile.dir {
    cursor: pointer;
  }
  .tile:hover,
  .tile:focus-visible {
    filter: brightness(1.15);
    outline: none;
    z-index: 1;
    box-shadow: inset 0 0 0 2px rgba(255, 255, 255, 0.85);
  }
  .name {
    font-family: var(--font-display);
    font-weight: 700;
    font-size: 13px;
    line-height: 1.15;
    max-width: 100%;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .size {
    font-size: 11px;
    opacity: 0.8;
    font-variant-numeric: tabular-nums;
  }
  .info {
    display: flex;
    gap: 12px;
    align-items: baseline;
    min-height: 20px;
    font-size: 13px;
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
  }
  .hint {
    margin-left: auto;
    color: var(--accent);
    font-size: 12px;
  }
</style>
