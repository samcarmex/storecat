/**
 * Typed wrappers over the Tauri commands. In a plain browser (design review via
 * `npm run dev`) every call resolves to an empty result so the UI still renders.
 */
import { convertFileSrc, invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";

export type MediaType = "nvme" | "ssd" | "hdd" | "network" | "cloud" | "optical" | "unknown";
export type Role = "unassigned" | "working" | "archive" | "media" | "backup" | "system";
export type Kind =
  | "other" | "video" | "image" | "audio" | "document" | "archive"
  | "code" | "disk_image" | "font" | "design" | "dir";
export type ItemType = "file" | "dir" | "archive_entry";
export type Health = "healthy" | "warning" | "unhealthy" | "unknown";

export const FLAG_HAS_THUMB = 1;
export const FLAG_HAS_TEXT = 2;
export const FLAG_PLACEHOLDER = 4;
export const FLAG_HAS_META = 8;
export const FLAG_HASHED = 16;

export interface Volume {
  id: number;
  identity: string;
  source: "device" | "folder";
  label: string;
  fs: string;
  capacity: number;
  free: number;
  media_type: MediaType;
  role: Role;
  notes: string;
  location: string;
  last_scan_at: number | null;
  last_seen_at: number | null;
  last_mount_path: string | null;
  file_count: number;
  dir_count: number;
  total_bytes: number;
  scan_seconds: number;
  created_at: number;
  photo: boolean;
  /** How the drive is attached: usb, sata, nvme, sd, network, optical, virtual, ... */
  bus: string;
  model: string;
  health: Health;
  health_at: number | null;
  online: boolean;
  mount_path: string | null;
}

export interface DetectedVolume {
  identity: string;
  mount_path: string;
  label: string;
  fs: string;
  capacity: number;
  free: number;
  media_type: MediaType;
  removable: boolean;
  guid: string | null;
  bus: string;
  model: string;
  health: Health;
}

export interface VolumePatch {
  label?: string;
  media_type?: MediaType;
  role?: Role;
  notes?: string;
  location?: string;
}

export type ScanPhase =
  | { phase: "walking" }
  | { phase: "committing" }
  | { phase: "done" }
  | { phase: "failed"; message: string };

export type ScanProgress = ScanPhase & {
  volume_id: number;
  files: number;
  dirs: number;
  bytes: number;
  errors: number;
  elapsed_ms: number;
  current: string;
  /** Committing: files brought into the search index so far, of index_total. */
  indexed?: number;
  index_total?: number;
};

export type EnrichPhase =
  | { phase: "queued" }
  | { phase: "running" }
  | { phase: "paused" }
  | { phase: "done" }
  | { phase: "failed"; message: string };

export type EnrichProgress = EnrichPhase & {
  volume_id: number;
  /** 0 = planning the read order, 1 = quick pass, 2 = text and video pass */
  pass: number;
  /** files done / to do in the current pass */
  done: number;
  total: number;
  hashed: number;
  metas: number;
  thumbs: number;
  texts: number;
  archives: number;
  elapsed_ms: number;
  current: string;
};

export interface EnrichCoverage {
  files: number;
  done: number;
  thumbs: number;
  texts: number;
  hashed: number;
}

export interface SearchHit {
  volume_id: number;
  item_id: number;
  item_type: ItemType;
  is_dir: boolean;
  flags: number;
  name: string;
  path: string;
  ext: string;
  kind: Kind;
  size: number;
  mtime: number;
  score: number;
  volume_label: string;
  volume_online: boolean;
}

export type SortBy =
  | "relevance"
  | "modified_desc"
  | "modified_asc"
  | "size_desc"
  | "size_asc"
  | "name_asc"
  | "name_desc";

export interface SearchOptions {
  limit: number;
  offset: number;
  sort: SortBy;
  volume_ids: number[] | null;
}

export interface SearchResponse {
  hits: SearchHit[];
  total: number;
  elapsed_ms: number;
  text: string;
  /** The order the hits are in (a filter-only "relevance" query comes back by name). */
  sort: SortBy;
}

export interface ExtCount {
  ext: string;
  files: number;
}

export interface FileMeta {
  width?: number;
  height?: number;
  orientation?: number;
  duration_ms?: number;
  bitrate_kbps?: number;
  video_codec?: string;
  audio_codec?: string;
  fps?: number;
  sample_rate?: number;
  channels?: number;
  title?: string;
  artist?: string;
  album?: string;
  year?: number;
  genre?: string;
  track?: number;
  camera_make?: string;
  camera_model?: string;
  lens?: string;
  iso?: number;
  f_number?: number;
  exposure?: string;
  focal_length_mm?: number;
  taken_at?: string;
  gps?: [number, number];
  pages?: number;
  words?: number;
  author?: string;
  subject?: string;
  software?: string;
  doc_created?: string;
  doc_modified?: string;
  extra?: Record<string, string>;
}

export interface ArchiveRef {
  file_id: number;
  name: string;
  path: string;
  entry_path: string;
}

export interface ItemDetail {
  volume_id: number;
  item_id: number;
  item_type: ItemType;
  is_dir: boolean;
  name: string;
  path: string;
  ext: string;
  kind: Kind;
  size: number;
  mtime: number;
  ctime: number;
  attrs: number;
  absolute_path: string | null;
  volume_label: string;
  volume_online: boolean;
  volume_location: string;
  placeholder: boolean;
  child_dirs: number | null;
  child_files: number | null;
  meta: FileMeta | null;
  text_excerpt: string | null;
  text_bytes: number;
  has_thumb: boolean;
  quick_hash: string | null;
  full_hash: string | null;
  archive: ArchiveRef | null;
  archive_entries: number | null;
  tags: string[];
}

export interface CatalogStats {
  index_rebuilding: boolean;
  index_rebuild_done?: number;
  index_rebuild_total?: number;
  volumes: number;
  online: number;
  files: number;
  dirs: number;
  bytes: number;
  capacity: number;
  free_online: number;
}

export interface DupMember {
  volume_id: number;
  volume_label: string;
  volume_online: boolean;
  file_id: number;
  name: string;
  path: string;
  mtime: number;
  full_hash: string | null;
  /** In a folder an app manages (caches, build output, app data): never removed. */
  app_managed: boolean;
}

export interface DupGroup {
  hash: string;
  size: number;
  count: number;
  waste: number;
  /** Copies outside app-managed folders. */
  user_copies: number;
  /** [volume_id, file_id] of the copy Storecat would keep. */
  suggested_keep: [number, number] | null;
  kind: Kind;
  name: string;
  verified: boolean;
  members: DupMember[];
}

export interface DupSummary {
  groups: number;
  files: number;
  waste: number;
}

export type DupScope = "mine" | "all";
export type DupSort = "waste" | "size" | "count" | "name";

export interface DupQuery {
  scope: DupScope;
  sort: DupSort;
  desc: boolean;
  limit: number;
  offset: number;
  /** Words that must appear in a copy's name or folder. */
  text?: string;
  kinds?: Kind[];
  volume_id?: number | null;
  /** Only groups that can be cleaned up now. */
  ready_only?: boolean;
}

export interface DupPage {
  groups: DupGroup[];
  summary: DupSummary;
}

export interface DupPick {
  hash: string;
  keep_volume_id: number;
  keep_file_id: number;
}

export interface FfmpegStatus {
  available: boolean;
  ffmpeg: string | null;
  ffprobe: string | null;
  version: string | null;
}

export interface DownloadProgress {
  tool: string;
  downloaded: number;
  total: number | null;
  done: boolean;
  error: string | null;
}

export interface BuildInfo {
  app_version: string;
  core_version: string;
  profile: string;
  platform: string;
  data_dir: string;
  index_docs: number;
  extractors: string[];
}

export function isTauri(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

/** True inside the quick-search palette window (same bundle, different window label). */
export function isPaletteWindow(): boolean {
  if (typeof location !== "undefined" && location.hash === "#palette") return true;
  if (!isTauri()) return false;
  try {
    return getCurrentWindow().label === "palette";
  } catch {
    return false;
  }
}

export async function showMainWindow(): Promise<void> {
  const win = getCurrentWindow();
  await win.show();
  await win.setFocus();
}

/** URL of a file's thumbnail, served by the app's `thumb` protocol. */
export function thumbUrl(volumeId: number, fileId: number): string {
  if (!isTauri()) return "";
  // The path is percent-encoded by convertFileSrc, so avoid characters it would escape.
  return convertFileSrc(`${volumeId}-${fileId}`, "thumb");
}

async function call<T>(cmd: string, args: Record<string, unknown> = {}, fallback: T): Promise<T> {
  if (!isTauri()) return fallback;
  return invoke<T>(cmd, args);
}

const EMPTY_STATS: CatalogStats = {
  index_rebuilding: false, volumes: 0, online: 0, files: 0, dirs: 0, bytes: 0, capacity: 0, free_online: 0,
};

export const api = {
  buildInfo: () =>
    call<BuildInfo>("build_info", {}, {
      app_version: "dev", core_version: "dev", profile: "browser", platform: "browser",
      data_dir: "", index_docs: 0, extractors: [],
    }),
  listVolumes: () => call<Volume[]>("list_volumes", {}, []),
  listDetected: () => call<DetectedVolume[]>("list_detected", {}, []),
  catalogStats: () => call<CatalogStats>("catalog_stats", {}, EMPTY_STATS),
  addDetected: (identity: string) => invoke<Volume>("add_detected", { identity }),
  addFolder: (path: string, label?: string) => invoke<Volume>("add_folder", { path, label }),
  pickAndAddFolder: () => invoke<Volume | null>("pick_and_add_folder"),
  startScan: (volumeId: number) => invoke<void>("start_scan", { volumeId }),
  scanning: () => call<number[]>("scanning", {}, []),
  removeVolume: (volumeId: number) => invoke<void>("remove_volume", { volumeId }),
  updateVolume: (volumeId: number, patch: VolumePatch) =>
    invoke<Volume>("update_volume", { volumeId, patch }),
  search: (query: string, options: Partial<SearchOptions> = {}) =>
    call<SearchResponse>(
      "search",
      { query, options: { limit: 300, offset: 0, sort: "relevance", volume_ids: null, ...options } },
      { hits: [], total: 0, elapsed_ms: 0, text: query, sort: options.sort ?? "relevance" },
    ),
  itemDetail: (volumeId: number, itemId: number, itemType: ItemType) =>
    invoke<ItemDetail>("item_detail", { volumeId, itemId, itemType }),
  openItem: (volumeId: number, itemId: number, itemType: ItemType) =>
    invoke<void>("open_item", { volumeId, itemId, itemType }),
  revealItem: (volumeId: number, itemId: number, itemType: ItemType) =>
    invoke<void>("reveal_item", { volumeId, itemId, itemType }),
  getSetting: (key: string) => call<string | null>("get_setting", { key }, null),
  setSetting: (key: string, value: string) => invoke<void>("set_setting", { key, value }),

  startEnrich: (volumeId: number) => invoke<void>("start_enrich", { volumeId }),
  cancelEnrich: (volumeId: number) => invoke<void>("cancel_enrich", { volumeId }),
  enriching: () => call<number[]>("enriching", {}, []),
  enrichQueued: () => call<number[]>("enrich_queued", {}, []),
  enrichCoverage: (volumeId: number) =>
    call<EnrichCoverage>("enrich_coverage", { volumeId }, { files: 0, done: 0, thumbs: 0, texts: 0, hashed: 0 }),
  resetEnrichment: (volumeId: number, keepHashes: boolean) =>
    invoke<void>("reset_enrichment", { volumeId, keepHashes }),

  duplicates: (query: DupQuery) => invoke<DupPage>("duplicates", { query }),
  extensionCounts: (kind: Kind) => call<ExtCount[]>("extension_counts", { kind }, []),
  duplicateSummary: () => call<DupSummary>("duplicate_summary", {}, { groups: 0, files: 0, waste: 0 }),
  verifyGroup: (hash: string) => invoke<DupGroup>("verify_group", { hash }),

  ffmpegStatus: () =>
    call<FfmpegStatus>("ffmpeg_status", {}, { available: false, ffmpeg: null, ffprobe: null, version: null }),
  downloadFfmpeg: () => invoke<FfmpegStatus>("download_ffmpeg"),
  exportVolume: (volumeId: number) => invoke<string | null>("export_volume", { volumeId }),
  importVolume: () => invoke<Volume | null>("import_volume"),

  onScanProgress(handler: (p: ScanProgress) => void): Promise<UnlistenFn> {
    if (!isTauri()) return Promise.resolve(() => {});
    return listen<ScanProgress>("scan-progress", (e) => handler(e.payload));
  },
  onEnrichProgress(handler: (p: EnrichProgress) => void): Promise<UnlistenFn> {
    if (!isTauri()) return Promise.resolve(() => {});
    return listen<EnrichProgress>("enrich-progress", (e) => handler(e.payload));
  },
  onToolDownload(handler: (p: DownloadProgress) => void): Promise<UnlistenFn> {
    if (!isTauri()) return Promise.resolve(() => {});
    return listen<DownloadProgress>("tool-download", (e) => handler(e.payload));
  },
  onVolumesChanged(handler: () => void): Promise<UnlistenFn> {
    if (!isTauri()) return Promise.resolve(() => {});
    return listen("volumes-changed", () => handler());
  },
};

// ----- Phase 3 -----------------------------------------------------------------------------

export type JobKind = "copy" | "move" | "delete";
export type ConflictPolicy = "skip" | "rename" | "overwrite";
export type JobStatus = "planned" | "running" | "paused" | "done" | "failed" | "cancelled";
export type ItemStatus = "pending" | "copied" | "verified" | "done" | "skipped" | "failed";

export interface JobSpec {
  kind: JobKind;
  name: string;
  sources: string[];
  dest_root: string;
  conflict: ConflictPolicy;
  verify: boolean;
  /** Delete jobs: a condition each file must still meet when it is reached. */
  checks?: string[];
}

export interface JobPlan {
  files: number;
  bytes: number;
  folders: number;
  conflicts: number;
  missing_sources: string[];
  dest_free: number | null;
  enough_space: boolean;
  sample: [string, string][];
  dest_volume_id: number | null;
  source_volume_id: number | null;
}

export interface Job {
  id: number;
  kind: JobKind;
  name: string;
  status: JobStatus;
  source_volume_id: number | null;
  dest_volume_id: number | null;
  dest_root: string;
  conflict: ConflictPolicy;
  verify: boolean;
  files_total: number;
  bytes_total: number;
  files_done: number;
  bytes_done: number;
  files_failed: number;
  files_skipped: number;
  created_at: number;
  started_at: number | null;
  finished_at: number | null;
  error: string | null;
}

export interface JobItem {
  id: number;
  job_id: number;
  source: string;
  dest: string;
  size: number;
  mtime: number;
  status: ItemStatus;
  error: string | null;
}

export interface JobProgress {
  job_id: number;
  status: JobStatus;
  files_done: number;
  files_total: number;
  bytes_done: number;
  bytes_total: number;
  files_failed: number;
  current: string;
  bytes_per_sec: number;
  elapsed_ms: number;
}

export interface Tag {
  id: number;
  name: string;
  count: number;
}

export interface SavedSearch {
  id: number;
  name: string;
  query: string;
  created_at: number;
}

export interface ShellStatus {
  tray_mode: boolean;
  hotkey_enabled: boolean;
  hotkey: string;
  autostart: boolean;
}

export function volumePhotoUrl(volumeId: number): string {
  if (!isTauri()) return "";
  return convertFileSrc(`v-${volumeId}`, "thumb");
}

export const ops = {
  shouldShowMain: () => call<boolean>("should_show_main", {}, true),
  planJob: (spec: JobSpec) => invoke<JobPlan>("plan_job", { spec }),
  createJob: (spec: JobSpec, start = true) => invoke<Job>("create_job", { spec, start }),
  runJob: (jobId: number) => invoke<void>("run_job", { jobId }),
  pauseJob: (jobId: number) => invoke<void>("pause_job", { jobId }),
  cancelJob: (jobId: number) => invoke<void>("cancel_job", { jobId }),
  deleteJob: (jobId: number) => invoke<void>("delete_job", { jobId }),
  jobs: () => call<Job[]>("jobs", {}, []),
  jobItems: (jobId: number, status?: ItemStatus, limit = 500) =>
    call<JobItem[]>("job_items", { jobId, status: status ?? null, limit }, []),
  runningJobs: () => call<number[]>("running_jobs", {}, []),
  sweepSpec: (destMount: string, kind: JobKind) => invoke<JobSpec>("sweep_spec", { destMount, kind }),
  pickFolders: () => invoke<string[]>("pick_folders"),
  pickFiles: () => invoke<string[]>("pick_files"),
  /** Keep one copy per group and recycle the other copies of yours, in one job. */
  duplicateCleanup: (picks: DupPick[]) => invoke<Job>("duplicate_cleanup", { picks }),

  tags: () => call<Tag[]>("tags", {}, []),
  tagItem: (volumeId: number, itemId: number, itemType: ItemType, tag: string) =>
    invoke<string[]>("tag_item", { volumeId, itemId, itemType, tag }),
  untagItem: (volumeId: number, itemId: number, itemType: ItemType, tag: string) =>
    invoke<string[]>("untag_item", { volumeId, itemId, itemType, tag }),
  savedSearches: () => call<SavedSearch[]>("saved_searches", {}, []),
  saveSearch: (name: string, query: string) => invoke<SavedSearch>("save_search", { name, query }),
  deleteSavedSearch: (id: number) => invoke<void>("delete_saved_search", { id }),
  setVolumePhoto: (volumeId: number) => invoke<boolean>("set_volume_photo", { volumeId }),
  clearVolumePhoto: (volumeId: number) => invoke<void>("clear_volume_photo", { volumeId }),

  shellStatus: () =>
    call<ShellStatus>("shell_status", {}, { tray_mode: false, hotkey_enabled: false, hotkey: "Ctrl+Shift+Space", autostart: false }),
  setTrayMode: (enabled: boolean) => invoke<void>("set_tray_mode", { enabled }),
  setHotkeyEnabled: (enabled: boolean) => invoke<void>("set_hotkey_enabled", { enabled }),
  setAutostart: (enabled: boolean) => invoke<boolean>("set_autostart", { enabled }),
  hidePalette: () => invoke<void>("hide_palette"),
  togglePalette: () => invoke<void>("toggle_palette"),
  openInMain: (query?: string) => invoke<void>("open_in_main", { query: query ?? null }),

  onJobProgress(handler: (p: JobProgress) => void): Promise<UnlistenFn> {
    if (!isTauri()) return Promise.resolve(() => {});
    return listen<JobProgress>("job-progress", (e) => handler(e.payload));
  },
  onJobsChanged(handler: () => void): Promise<UnlistenFn> {
    if (!isTauri()) return Promise.resolve(() => {});
    return listen("jobs-changed", () => handler());
  },
  onSetQuery(handler: (q: string) => void): Promise<UnlistenFn> {
    if (!isTauri()) return Promise.resolve(() => {});
    return listen<string>("set-query", (e) => handler(e.payload));
  },
  onPaletteShown(handler: () => void): Promise<UnlistenFn> {
    if (!isTauri()) return Promise.resolve(() => {});
    return listen("palette-shown", () => handler());
  },
};

// ----- Phase 4: storage overview and suggestions ---------------------------------------

export interface KindBytes {
  kind: Kind;
  files: number;
  bytes: number;
}

export interface FolderNode {
  id: number;
  name: string;
  path: string;
  bytes: number;
  files: number;
  dirs: number;
  newest: number;
  top_kind: Kind;
}

export interface FileLeaf {
  id: number;
  name: string;
  size: number;
  kind: Kind;
  mtime: number;
}

export interface FolderTree {
  volume_id: number;
  node: FolderNode;
  crumbs: { id: number; name: string }[];
  children: FolderNode[];
  files: FileLeaf[];
  rest_bytes: number;
  rest_count: number;
}

export type Severity = "urgent" | "warning" | "tip" | "info";

export type SuggestionAction =
  | { type: "job"; spec: JobSpec; needs: number[] }
  | { type: "view"; view: string; volume_id: number | null; query: string | null; dir_id: number | null };

export interface Suggestion {
  key: string;
  rule: string;
  severity: Severity;
  title: string;
  body: string;
  bytes: number;
  volume_id: number | null;
  action: SuggestionAction | null;
  path: string | null;
}

export interface RuleSettings {
  enabled: boolean;
  cold_days: number;
  cold_min_gb: number;
  sweep_days: number;
  sweep_min_gb: number;
  full_percent: number;
  duplicates_min_gb: number;
  stale_days: number;
}

export const storage = {
  folderTree: (volumeId: number, dirId: number | null = null) =>
    invoke<FolderTree>("folder_tree", { volumeId, dirId }),
  kindBreakdown: (volumeId: number) => call<KindBytes[]>("kind_breakdown", { volumeId }, []),
  suggestions: () => call<Suggestion[]>("suggestions", {}, []),
  dismiss: (key: string, days: number | null = null) =>
    invoke<void>("dismiss_suggestion", { key, days }),
  reset: () => invoke<void>("reset_suggestions"),
  ruleSettings: () => call<RuleSettings | null>("rule_settings", {}, null),
  setRuleSettings: (rules: RuleSettings) => invoke<void>("set_rule_settings", { rules }),
};
