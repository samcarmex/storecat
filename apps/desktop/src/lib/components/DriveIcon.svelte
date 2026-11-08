<!--
  Line drawings of drive types, shown on a drive's card when it has no photo. Drawn for
  Storecat (no brand artwork): they inherit `color`, and faces are filled at low opacity.
-->
<script lang="ts">
  import type { DriveKind } from "../format";

  let { kind, size = 64 }: { kind: DriveKind; size?: number } = $props();
</script>

<svg viewBox="0 0 64 64" width={size} height={size} fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
  {#if kind === "hdd"}
    <!-- 3.5" internal drive with its lid off: platter, spindle and head arm. -->
    <rect class="face" x="10" y="5" width="44" height="54" rx="5" />
    <circle cx="32" cy="27" r="15" />
    <circle cx="32" cy="27" r="3.5" />
    <path d="M47 50 L36.5 32" />
    <circle class="solid" cx="47" cy="50" r="3.5" />
    <circle class="solid" cx="15" cy="10" r="1.3" stroke="none" />
    <circle class="solid" cx="49" cy="10" r="1.3" stroke="none" />
    <circle class="solid" cx="15" cy="54" r="1.3" stroke="none" />
  {:else if kind === "external-hdd"}
    <!-- Desktop USB drive standing up, with vents, a light and its cable. -->
    <rect class="face" x="14" y="4" width="36" height="50" rx="6" />
    <path d="M22 11 h20 M22 16 h20 M22 21 h20" stroke-width="2" opacity="0.55" />
    <circle class="solid" cx="32" cy="45" r="2.4" stroke="none" />
    <path d="M32 54 v3 c0 3 2 4 5 4 h11" />
  {:else if kind === "ssd"}
    <!-- 2.5" SATA SSD: flat case, label and connector. -->
    <rect class="face" x="6" y="14" width="46" height="36" rx="4" />
    <rect x="12" y="20" width="24" height="14" rx="2" stroke-width="2" />
    <path d="M14 42 h12" stroke-width="2" opacity="0.55" />
    <path d="M52 22 h5 v8 h-5 M52 34 h5 v8 h-5" stroke-width="2" />
  {:else if kind === "portable-ssd"}
    <!-- Pocket SSD: a small rounded slab with its USB-C cable. -->
    <rect class="face" x="5" y="12" width="40" height="40" rx="10" />
    <path d="M15 24 h14" stroke-width="2" opacity="0.55" />
    <circle class="solid" cx="35" cy="42" r="2.2" stroke="none" />
    <rect x="45" y="28" width="5" height="8" rx="1.5" stroke-width="2" />
    <path d="M50 32 h3 a5 5 0 0 1 5 5 v19" />
  {:else if kind === "nvme"}
    <!-- M.2 stick: board, chips, screw notch and gold fingers. -->
    <rect class="face" x="4" y="22" width="56" height="20" rx="2.5" />
    <path d="M4 29 a3 3 0 0 1 0 6" />
    <rect x="12" y="26.5" width="11" height="11" rx="1.5" stroke-width="2" />
    <rect x="27" y="26.5" width="11" height="11" rx="1.5" stroke-width="2" />
    <rect x="42" y="28.5" width="6" height="7" rx="1" stroke-width="2" />
    <path d="M53 25 v14 M56.5 25 v14" stroke-width="2" opacity="0.7" />
  {:else if kind === "usb-stick"}
    <!-- Flash drive: plug on top, body, lanyard loop. -->
    <rect x="24" y="4" width="16" height="15" rx="2" />
    <path d="M28.5 9 h2 M33.5 9 h2" stroke-width="2.5" />
    <rect class="face" x="19" y="19" width="26" height="40" rx="7" />
    <circle cx="32" cy="51" r="3" stroke-width="2" />
  {:else if kind === "sd-card"}
    <!-- SD card: clipped corner, contacts, lock switch. -->
    <path class="face" d="M24 5 H45 a5 5 0 0 1 5 5 V54 a5 5 0 0 1 -5 5 H19 a5 5 0 0 1 -5 -5 V15 Z" />
    <path d="M25 12 v8 M31 12 v8 M37 12 v8 M43 12 v8" stroke-width="2.2" opacity="0.7" />
    <path d="M14 30 h-3 v8 h3" stroke-width="2" />
  {:else if kind === "disc"}
    <circle class="face" cx="32" cy="32" r="27" />
    <circle cx="32" cy="32" r="9" />
    <circle cx="32" cy="32" r="2.5" />
    <path d="M32 13 a19 19 0 0 1 19 19" stroke-width="2" opacity="0.55" />
  {:else if kind === "network"}
    <!-- Two-bay NAS with its network cable. -->
    <rect class="face" x="12" y="6" width="40" height="44" rx="5" />
    <rect x="18" y="13" width="28" height="9" rx="2" stroke-width="2" />
    <rect x="18" y="27" width="28" height="9" rx="2" stroke-width="2" />
    <circle class="solid" cx="22" cy="43" r="1.8" stroke="none" />
    <circle class="solid" cx="28" cy="43" r="1.8" stroke="none" />
    <path d="M32 50 v6 M18 58 h28 M18 56 v4 M46 56 v4" />
  {:else if kind === "cloud"}
    <path class="face" d="M19 48 a11 11 0 0 1 -1 -22 a15 15 0 0 1 28.5 -3 a12.5 12.5 0 0 1 -0.5 25 Z" />
  {:else if kind === "folder"}
    <path class="face" d="M6 17 a5 5 0 0 1 5 -5 h13 l6 6 h23 a5 5 0 0 1 5 5 v25 a5 5 0 0 1 -5 5 H11 a5 5 0 0 1 -5 -5 Z" />
    <path d="M6 26 h52" stroke-width="2" opacity="0.55" />
  {:else}
    <!-- Any other drive: a plain enclosure with a light. -->
    <rect class="face" x="6" y="18" width="52" height="28" rx="6" />
    <path d="M14 32 h20" stroke-width="2" opacity="0.55" />
    <circle class="solid" cx="48" cy="32" r="2.5" stroke="none" />
  {/if}
</svg>

<style>
  .face {
    fill: currentColor;
    fill-opacity: 0.1;
  }
  .solid {
    fill: currentColor;
  }
</style>
