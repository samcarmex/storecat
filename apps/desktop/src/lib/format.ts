import type { FileMeta, Kind, MediaType } from "./api";

export function bytes(n: number, digits = 1): string {
  if (!Number.isFinite(n) || n < 0) return "–";
  const units = ["B", "KB", "MB", "GB", "TB", "PB"];
  let v = n;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i++;
  }
  return i === 0 ? `${n} B` : `${v.toFixed(v >= 100 ? 0 : digits)} ${units[i]}`;
}

export function count(n: number): string {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(n >= 10_000_000 ? 0 : 1)} M`;
  if (n >= 10_000) return `${Math.round(n / 1000)} k`;
  return n.toLocaleString();
}

const dateFmt = new Intl.DateTimeFormat(undefined, { day: "numeric", month: "short", year: "numeric" });
const dateTimeFmt = new Intl.DateTimeFormat(undefined, {
  day: "numeric", month: "short", year: "numeric", hour: "2-digit", minute: "2-digit",
});

export function date(unix: number | null | undefined): string {
  if (!unix) return "–";
  return dateFmt.format(new Date(unix * 1000));
}

export function dateTime(unix: number | null | undefined): string {
  if (!unix) return "–";
  return dateTimeFmt.format(new Date(unix * 1000));
}

export function ago(unix: number | null | undefined): string {
  if (!unix) return "never";
  const s = Math.max(0, Math.floor(Date.now() / 1000) - unix);
  if (s < 60) return "just now";
  if (s < 3600) return `${Math.floor(s / 60)} min ago`;
  if (s < 86400) return `${Math.floor(s / 3600)} h ago`;
  if (s < 86400 * 30) return `${Math.floor(s / 86400)} d ago`;
  return date(unix);
}

export function duration(ms: number): string {
  if (ms < 1000) return `${Math.round(ms)} ms`;
  const s = ms / 1000;
  if (s < 90) return `${s.toFixed(1)} s`;
  const m = Math.floor(s / 60);
  if (m < 90) return `${m} min ${Math.round(s - m * 60)} s`;
  const h = Math.floor(m / 60);
  return `${h} h ${m - h * 60} min`;
}

/** Media duration as h:mm:ss or m:ss. */
export function clock(ms: number): string {
  const total = Math.round(ms / 1000);
  const h = Math.floor(total / 3600);
  const m = Math.floor((total % 3600) / 60);
  const s = total % 60;
  return h > 0
    ? `${h}:${String(m).padStart(2, "0")}:${String(s).padStart(2, "0")}`
    : `${m}:${String(s).padStart(2, "0")}`;
}

export const KIND_LABEL: Record<Kind, string> = {
  other: "File", video: "Video", image: "Image", audio: "Audio", document: "Document",
  archive: "Archive", code: "Code", disk_image: "Disk image", font: "Font", design: "Design",
  dir: "Folder",
};

/** Tile tint per kind: background rgba + foreground. */
export const KIND_TILE: Record<Kind, [string, string]> = {
  video: ["rgba(255,106,61,.18)", "#ffb39b"],
  image: ["rgba(245,196,81,.18)", "#ffe2a0"],
  audio: ["rgba(142,155,255,.18)", "#c3caff"],
  document: ["rgba(94,224,200,.18)", "#9bf0df"],
  archive: ["rgba(192,132,252,.18)", "#ddc1ff"],
  code: ["rgba(142,155,255,.14)", "#aeb6ff"],
  disk_image: ["rgba(192,132,252,.14)", "#ccb0ff"],
  font: ["rgba(245,196,81,.14)", "#f0d79a"],
  design: ["rgba(255,106,61,.14)", "#ffc2ae"],
  dir: ["rgba(255,255,255,.07)", "#c9cdd6"],
  other: ["rgba(255,255,255,.06)", "#9aa0ad"],
};

/** Solid colour per kind, for the treemap and the content-type bar. */
export const KIND_COLOR: Record<Kind, string> = {
  video: "#ff6a3d",
  image: "#f5c451",
  audio: "#8e9bff",
  document: "#5ee0c8",
  archive: "#c084fc",
  code: "#7dd3fc",
  disk_image: "#f472b6",
  font: "#fda4af",
  design: "#a3e635",
  dir: "#c9cdd6",
  other: "#5b6170",
};

export const HEALTH_LABEL: Record<string, string> = {
  healthy: "Healthy",
  warning: "Warning",
  unhealthy: "Failing",
  unknown: "Unknown",
};

export const HEALTH_COLOR: Record<string, string> = {
  healthy: "var(--teal)",
  warning: "var(--amber)",
  unhealthy: "var(--danger)",
  unknown: "var(--fg-muted)",
};

/** Human bus name: "usb" -> "USB", "nvme" -> "NVMe". */
export function busLabel(bus: string): string {
  const m: Record<string, string> = {
    usb: "USB", sata: "SATA", nvme: "NVMe", sd: "SD card", network: "Network", optical: "Optical",
    virtual: "Virtual disk", thunderbolt: "Thunderbolt", firewire: "FireWire", sas: "SAS",
    scsi: "SCSI", raid: "RAID", spaces: "Storage Spaces", iscsi: "iSCSI", mmc: "MMC", ufs: "UFS",
    ata: "ATA", atapi: "ATAPI", fibre: "Fibre Channel",
  };
  return m[bus] ?? bus.toUpperCase();
}

export const MEDIA_LABEL: Record<MediaType, string> = {
  nvme: "NVMe", ssd: "SSD", hdd: "HDD", network: "NET", cloud: "CLOUD", optical: "DISC",
  unknown: "DRIVE",
};

export const MEDIA_COLOR: Record<MediaType, string> = {
  nvme: "var(--teal)", ssd: "var(--teal)", hdd: "var(--amber)", network: "var(--indigo)",
  cloud: "var(--purple)", optical: "var(--purple)", unknown: "var(--fg-muted)",
};

export function tileText(hit: { ext: string; is_dir: boolean; kind: Kind }): string {
  if (hit.is_dir) return "DIR";
  if (hit.ext) return hit.ext.toUpperCase().slice(0, 4);
  return "·";
}

/** Display path with native separators. */
export function displayPath(rel: string): string {
  return navigator.platform.startsWith("Win") ? rel.replaceAll("/", "\\") : rel;
}

/** Metadata as label/value pairs, most useful first, for the detail pane. */
export function metaRows(m: FileMeta | null | undefined): [string, string][] {
  if (!m) return [];
  const rows: [string, string][] = [];
  if (m.width && m.height) rows.push(["Dimensions", `${m.width} × ${m.height}`]);
  if (m.duration_ms) rows.push(["Duration", clock(m.duration_ms)]);
  const codecs = [m.video_codec, m.audio_codec].filter(Boolean).join(" / ");
  if (codecs) rows.push(["Codec", codecs.toUpperCase()]);
  if (m.fps) rows.push(["Frame rate", `${Number(m.fps.toFixed(2))} fps`]);
  if (m.bitrate_kbps) rows.push(["Bitrate", m.bitrate_kbps >= 1000 ? `${(m.bitrate_kbps / 1000).toFixed(1)} Mb/s` : `${m.bitrate_kbps} kb/s`]);
  if (m.sample_rate) rows.push(["Audio", `${(m.sample_rate / 1000).toFixed(1)} kHz${m.channels ? ` · ${m.channels === 1 ? "mono" : m.channels === 2 ? "stereo" : `${m.channels} ch`}` : ""}`]);
  if (m.title) rows.push(["Title", m.title]);
  if (m.artist) rows.push(["Artist", m.artist]);
  if (m.album) rows.push(["Album", `${m.album}${m.year ? ` (${m.year})` : ""}`]);
  if (m.genre) rows.push(["Genre", m.genre]);
  if (m.camera_make || m.camera_model) {
    const make = m.camera_make ?? "";
    const model = m.camera_model ?? "";
    rows.push(["Camera", model.toLowerCase().startsWith(make.toLowerCase()) ? model : `${make} ${model}`.trim()]);
  }
  if (m.lens) rows.push(["Lens", m.lens]);
  const exp = [
    m.focal_length_mm ? `${Number(m.focal_length_mm.toFixed(1))} mm` : null,
    m.f_number ? `f/${Number(m.f_number.toFixed(1))}` : null,
    m.exposure ? `${m.exposure} s` : null,
    m.iso ? `ISO ${m.iso}` : null,
  ].filter(Boolean);
  if (exp.length) rows.push(["Exposure", exp.join(" · ")]);
  if (m.taken_at) rows.push(["Taken", m.taken_at.replace(/^(\d{4}):(\d{2}):(\d{2})/, "$1-$2-$3")]);
  if (m.gps) rows.push(["Location", `${m.gps[0].toFixed(5)}, ${m.gps[1].toFixed(5)}`]);
  if (m.pages) rows.push(["Pages", String(m.pages)]);
  if (m.words) rows.push(["Words", m.words.toLocaleString()]);
  if (m.author) rows.push(["Author", m.author]);
  if (m.subject) rows.push(["Subject", m.subject]);
  if (m.doc_created) rows.push(["Doc created", m.doc_created]);
  if (m.software) rows.push(["Software", m.software]);
  for (const [k, v] of Object.entries(m.extra ?? {})) rows.push([k.replace(/_/g, " "), v]);
  return rows;
}
