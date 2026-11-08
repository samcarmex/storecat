<script lang="ts">
  /**
   * A drive's name. Hovering it shows the drive's photo, when it has one, so it's easy to
   * tell which physical drive is meant.
   */
  import { volumePhotoUrl } from "../api";
  import { drivePhotos } from "../drivePhotos.svelte";

  let { id, label, class: cls = "" }: { id: number; label: string; class?: string } = $props();

  const SIZE = 168;
  const DELAY = 350;

  let el = $state<HTMLSpanElement | null>(null);
  let at = $state<{ left: number; top: number } | null>(null);
  let timer: ReturnType<typeof setTimeout> | null = null;

  const token = $derived(drivePhotos[id]);

  function enter() {
    if (token === undefined) return;
    timer = setTimeout(place, DELAY);
  }

  function leave() {
    if (timer) clearTimeout(timer);
    timer = null;
    at = null;
  }

  function place() {
    if (!el) return;
    const r = el.getBoundingClientRect();
    const h = SIZE + 30;
    const top = r.top - h - 8 >= 8 ? r.top - h - 8 : r.bottom + 8;
    const left = Math.max(8, Math.min(window.innerWidth - SIZE - 8, r.left + r.width / 2 - SIZE / 2));
    at = { left, top };
    window.addEventListener("scroll", leave, { capture: true, once: true });
  }

  /** Render the card at the end of <body>, clear of scrolling and transformed parents. */
  function portal(node: HTMLElement) {
    document.body.appendChild(node);
    return { destroy: () => node.remove() };
  }
</script>

<span
  class="dn {cls}"
  class:has-photo={token !== undefined}
  bind:this={el}
  onpointerenter={enter}
  onpointerleave={leave}
  role="presentation"
>{label}</span>

{#if at && token !== undefined}
  <div class="drive-photo" use:portal style="left: {at.left}px; top: {at.top}px; width: {SIZE}px" role="tooltip">
    <img src={volumePhotoUrl(id) + "?t=" + token} alt="Photo of {label}" width={SIZE} height={SIZE} />
    <span>{label}</span>
  </div>
{/if}

<style>
  .dn {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .drive-photo {
    position: fixed;
    z-index: 1000;
    pointer-events: none;
    background: var(--surface-2, #1f232d);
    border: 1px solid var(--line-strong);
    border-radius: 12px;
    padding: 6px;
    box-shadow: 0 10px 30px rgba(0, 0, 0, 0.45);
    display: flex;
    flex-direction: column;
    gap: 4px;
    animation: pop 0.12s ease-out;
  }
  .drive-photo img {
    width: 100%;
    height: auto;
    aspect-ratio: 1;
    object-fit: cover;
    border-radius: 8px;
    display: block;
    background: var(--hover);
  }
  .drive-photo span {
    font-size: 12px;
    color: var(--fg-soft);
    text-align: center;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    padding: 0 2px 2px;
  }
  @keyframes pop {
    from {
      opacity: 0;
      transform: translateY(4px);
    }
  }
</style>
