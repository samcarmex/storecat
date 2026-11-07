<script lang="ts" generics="T">
  import type { Snippet } from "svelte";

  /** Fixed-row-height virtual list. Renders only what is visible plus a margin. */
  let {
    items,
    rowHeight = 56,
    overscan = 8,
    row,
    onNearEnd,
    class: cls = "",
  }: {
    items: T[];
    rowHeight?: number;
    overscan?: number;
    row: Snippet<[T, number]>;
    onNearEnd?: () => void;
    class?: string;
  } = $props();

  let viewport: HTMLDivElement | undefined = $state();
  let scrollTop = $state(0);
  let height = $state(600);

  const start = $derived(Math.max(0, Math.floor(scrollTop / rowHeight) - overscan));
  const end = $derived(Math.min(items.length, Math.ceil((scrollTop + height) / rowHeight) + overscan));
  const visible = $derived(items.slice(start, end));

  function onScroll() {
    if (!viewport) return;
    scrollTop = viewport.scrollTop;
    if (onNearEnd && viewport.scrollTop + height > items.length * rowHeight - rowHeight * 20) {
      onNearEnd();
    }
  }

  $effect(() => {
    if (!viewport) return;
    const ro = new ResizeObserver(() => {
      height = viewport!.clientHeight;
    });
    ro.observe(viewport);
    height = viewport.clientHeight;
    return () => ro.disconnect();
  });

  export function scrollToIndex(i: number) {
    if (!viewport) return;
    const top = i * rowHeight;
    if (top < viewport.scrollTop) viewport.scrollTop = top;
    else if (top + rowHeight > viewport.scrollTop + height) viewport.scrollTop = top + rowHeight - height;
  }
</script>

<div class="viewport {cls}" bind:this={viewport} onscroll={onScroll}>
  <div class="spacer" style="height: {items.length * rowHeight}px">
    <div class="window" style="transform: translateY({start * rowHeight}px)">
      {#each visible as item, i (start + i)}
        <div class="row" style="height: {rowHeight}px">
          {@render row(item, start + i)}
        </div>
      {/each}
    </div>
  </div>
</div>

<style>
  .viewport {
    overflow-y: auto;
    overflow-x: hidden;
    position: relative;
    contain: strict;
  }
  .spacer {
    position: relative;
    width: 100%;
  }
  .window {
    position: absolute;
    left: 0;
    right: 0;
    top: 0;
    will-change: transform;
  }
  .row {
    display: flex;
    align-items: stretch;
  }
</style>
