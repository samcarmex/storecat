//! Plain data types shared by the catalogue, the CLI and the desktop app.
//! Everything here serialises with serde so it can cross the Tauri bridge unchanged.

use serde::{Deserialize, Serialize};

pub type VolumeId = i64;

/// What a drive physically is. Detected where possible, editable by the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaType {
    Nvme,
    Ssd,
    Hdd,
    Network,
    Cloud,
    Optical,
    Unknown,
}

impl MediaType {
    pub fn as_str(self) -> &'static str {
        match self {
            MediaType::Nvme => "nvme",
            MediaType::Ssd => "ssd",
            MediaType::Hdd => "hdd",
            MediaType::Network => "network",
            MediaType::Cloud => "cloud",
            MediaType::Optical => "optical",
            MediaType::Unknown => "unknown",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "nvme" => MediaType::Nvme,
            "ssd" => MediaType::Ssd,
            "hdd" => MediaType::Hdd,
            "network" => MediaType::Network,
            "cloud" => MediaType::Cloud,
            "optical" => MediaType::Optical,
            _ => MediaType::Unknown,
        }
    }
}

/// Drive health as the operating system reports it (S.M.A.R.T. summarised by the OS).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Health {
    Healthy,
    Warning,
    Unhealthy,
    #[default]
    Unknown,
}

impl Health {
    pub fn as_str(self) -> &'static str {
        match self {
            Health::Healthy => "healthy",
            Health::Warning => "warning",
            Health::Unhealthy => "unhealthy",
            Health::Unknown => "unknown",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s.to_ascii_lowercase().as_str() {
            "healthy" => Health::Healthy,
            "warning" => Health::Warning,
            "unhealthy" => Health::Unhealthy,
            _ => Health::Unknown,
        }
    }
}

/// What the user uses a drive for. Drives the storage suggestions in Phase 4.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Unassigned,
    Working,
    Archive,
    Media,
    Backup,
    System,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Unassigned => "unassigned",
            Role::Working => "working",
            Role::Archive => "archive",
            Role::Media => "media",
            Role::Backup => "backup",
            Role::System => "system",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "working" => Role::Working,
            "archive" => Role::Archive,
            "media" => Role::Media,
            "backup" => Role::Backup,
            "system" => Role::System,
            _ => Role::Unassigned,
        }
    }
}

/// How a volume was added: a whole device, or an arbitrary folder / network path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VolumeSource {
    Device,
    Folder,
}

impl VolumeSource {
    pub fn as_str(self) -> &'static str {
        match self {
            VolumeSource::Device => "device",
            VolumeSource::Folder => "folder",
        }
    }
    pub fn parse(s: &str) -> Self {
        if s == "folder" {
            VolumeSource::Folder
        } else {
            VolumeSource::Device
        }
    }
}

/// What an index document / catalogue item is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemType {
    File = 0,
    Dir = 1,
    /// An entry inside an archive; `item_id` is the `archive_entries` row id.
    ArchiveEntry = 2,
}

impl ItemType {
    pub fn from_u8(v: u8) -> ItemType {
        match v {
            1 => ItemType::Dir,
            2 => ItemType::ArchiveEntry,
            _ => ItemType::File,
        }
    }
}

/// Bit flags stored on every index document and search hit.
pub mod flags {
    pub const HAS_THUMB: u64 = 1;
    pub const HAS_TEXT: u64 = 2;
    pub const PLACEHOLDER: u64 = 4;
    pub const HAS_META: u64 = 8;
    pub const HASHED: u64 = 16;
    /// A file an operating system left behind (`._name`, `.DS_Store`, `Thumbs.db`...).
    /// Hidden from search unless the query asks for `is:leftover`.
    pub const OS_LEFTOVER: u64 = 32;
    /// In a system, program or app-managed folder (`Windows`, `AppData`, caches, build
    /// output). Ranked below other matches.
    pub const SYSTEM_PLACE: u64 = 64;
    /// In a person's own folders (`Users/<name>/Documents`, `Desktop`, `Pictures`...).
    /// Ranked above other matches.
    pub const USER_PLACE: u64 = 128;
}

/// A volume the platform layer can see right now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DetectedVolume {
    /// Stable identity: survives drive-letter changes and (where the filesystem exposes a
    /// serial) moving between machines.
    pub identity: String,
    pub mount_path: String,
    pub label: String,
    pub fs: String,
    pub capacity: u64,
    pub free: u64,
    pub media_type: MediaType,
    pub removable: bool,
    /// Volume GUID on Windows, UUID elsewhere, when known.
    pub guid: Option<String>,
    /// How the drive is attached: usb, sata, nvme, sd, network, optical, virtual, ...
    #[serde(default)]
    pub bus: String,
    /// Device model as reported by the drive (e.g. "Samsung SSD 870 EVO 2TB").
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub health: Health,
}

/// A catalogued volume as the UI sees it: persisted fields plus live overlay.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Volume {
    pub id: VolumeId,
    pub identity: String,
    pub source: VolumeSource,
    pub label: String,
    pub fs: String,
    pub capacity: u64,
    pub free: u64,
    pub media_type: MediaType,
    pub role: Role,
    pub notes: String,
    pub location: String,
    pub last_scan_at: Option<i64>,
    pub last_seen_at: Option<i64>,
    pub last_mount_path: Option<String>,
    pub file_count: u64,
    pub dir_count: u64,
    pub total_bytes: u64,
    pub scan_seconds: f64,
    pub created_at: i64,
    /// A photo of the physical drive is stored (`volumes/<id>.photo.jpg`).
    pub photo: bool,
    /// Drive profile, refreshed whenever the drive is connected.
    pub bus: String,
    pub model: String,
    pub health: Health,
    /// When health was last read (unix seconds).
    pub health_at: Option<i64>,
    /// Read trouble Storecat saw itself (unreadable blocks, reads that hung), which a USB
    /// enclosure's "healthy" S.M.A.R.T. summary doesn't clear. Stays until the user clears it.
    pub fault: Option<String>,
    pub fault_at: Option<i64>,
    /// Live: connected right now.
    pub online: bool,
    /// Live: where it is mounted right now, if online.
    pub mount_path: Option<String>,
}

/// Bytes and files of one content type on a drive.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KindBytes {
    pub kind: Kind,
    pub files: u64,
    pub bytes: u64,
}

/// A folder with its total size (everything below it), for the treemap.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FolderNode {
    pub id: i64,
    pub name: String,
    pub path: String,
    pub bytes: u64,
    pub files: u64,
    pub dirs: u64,
    /// Newest modification time anywhere below this folder.
    pub newest: i64,
    /// Content type that takes the most bytes below this folder.
    pub top_kind: Kind,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FolderCrumb {
    pub id: i64,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileLeaf {
    pub id: i64,
    pub name: String,
    pub size: u64,
    pub kind: Kind,
    pub mtime: i64,
}

/// One level of the folder treemap.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FolderTree {
    pub volume_id: VolumeId,
    pub node: FolderNode,
    /// From the drive root down to `node`.
    pub crumbs: Vec<FolderCrumb>,
    pub children: Vec<FolderNode>,
    /// The largest files directly in this folder.
    pub files: Vec<FileLeaf>,
    /// Everything not listed above (small files and the tail of subfolders).
    pub rest_bytes: u64,
    pub rest_count: u64,
}

/// Editable fields of a volume.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VolumePatch {
    pub label: Option<String>,
    pub media_type: Option<MediaType>,
    pub role: Option<Role>,
    pub notes: Option<String>,
    pub location: Option<String>,
}

/// Coarse content category, derived from the extension. Stored as a small integer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Other = 0,
    Video = 1,
    Image = 2,
    Audio = 3,
    Document = 4,
    Archive = 5,
    Code = 6,
    DiskImage = 7,
    Font = 8,
    Design = 9,
    Dir = 10,
}

impl Kind {
    pub fn from_u8(v: u8) -> Kind {
        match v {
            1 => Kind::Video,
            2 => Kind::Image,
            3 => Kind::Audio,
            4 => Kind::Document,
            5 => Kind::Archive,
            6 => Kind::Code,
            7 => Kind::DiskImage,
            8 => Kind::Font,
            9 => Kind::Design,
            10 => Kind::Dir,
            _ => Kind::Other,
        }
    }

    pub fn parse(s: &str) -> Option<Kind> {
        Some(match s {
            "video" | "videos" | "movie" | "movies" => Kind::Video,
            "image" | "images" | "photo" | "photos" | "picture" | "pictures" => Kind::Image,
            "audio" | "music" | "sound" | "sounds" => Kind::Audio,
            "document" | "documents" | "doc" | "docs" => Kind::Document,
            "archive" | "archives" | "zip" => Kind::Archive,
            "code" | "source" => Kind::Code,
            "disk" | "disk_image" | "iso" => Kind::DiskImage,
            "font" | "fonts" => Kind::Font,
            "design" => Kind::Design,
            "dir" | "folder" | "folders" | "directory" => Kind::Dir,
            "other" => Kind::Other,
            _ => return None,
        })
    }
}

/// Lifecycle of a scan, reported through a callback and as Tauri events.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "phase")]
pub enum ScanPhase {
    Walking,
    Committing,
    Done,
    Failed { message: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanProgress {
    pub volume_id: VolumeId,
    #[serde(flatten)]
    pub phase: ScanPhase,
    pub files: u64,
    pub dirs: u64,
    pub bytes: u64,
    pub errors: u64,
    pub elapsed_ms: u64,
    pub current: String,
    /// Committing: files brought into the search index so far, of `index_total`.
    #[serde(default)]
    pub indexed: u64,
    #[serde(default)]
    pub index_total: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ScanStats {
    pub files: u64,
    pub dirs: u64,
    pub bytes: u64,
    pub errors: u64,
    pub skipped_symlinks: u64,
    pub elapsed_ms: u64,
}

/// One search result row. Light on purpose: thousands of these cross the bridge.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchHit {
    pub volume_id: VolumeId,
    pub item_id: i64,
    pub item_type: ItemType,
    pub is_dir: bool,
    /// See [`flags`].
    pub flags: u64,
    pub name: String,
    /// Directory path relative to the volume root, `/`-separated, empty for root.
    pub path: String,
    pub ext: String,
    pub kind: Kind,
    pub size: u64,
    pub mtime: i64,
    pub score: f32,
    pub volume_label: String,
    pub volume_online: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SortBy {
    /// Best match first. A query with no words to rank by is listed by name instead.
    #[default]
    Relevance,
    ModifiedDesc,
    ModifiedAsc,
    SizeDesc,
    SizeAsc,
    NameAsc,
    NameDesc,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchOptions {
    pub limit: usize,
    pub offset: usize,
    pub sort: SortBy,
    /// Restrict to these volumes (in addition to any `drive:` filter in the query).
    pub volume_ids: Option<Vec<VolumeId>>,
}

impl Default for SearchOptions {
    fn default() -> Self {
        Self {
            limit: 200,
            offset: 0,
            sort: SortBy::Relevance,
            volume_ids: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResponse {
    pub hits: Vec<SearchHit>,
    pub total: usize,
    pub elapsed_ms: f64,
    /// The query with filters stripped, for the UI's "no results for …" line.
    pub text: String,
    /// The order the hits are in (a filter-only "best match" query comes back by name).
    #[serde(default)]
    pub sort: SortBy,
}

/// Everything the detail pane shows for one item.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ItemDetail {
    pub volume_id: VolumeId,
    pub item_id: i64,
    pub item_type: ItemType,
    pub is_dir: bool,
    pub name: String,
    pub path: String,
    pub ext: String,
    pub kind: Kind,
    pub size: u64,
    pub mtime: i64,
    pub ctime: i64,
    pub attrs: u32,
    /// Absolute path on this machine when the volume is online.
    pub absolute_path: Option<String>,
    pub volume_label: String,
    pub volume_online: bool,
    pub volume_location: String,
    /// True when the file is a cloud placeholder (online-only), from attributes.
    pub placeholder: bool,
    /// For directories: direct counts.
    pub child_dirs: Option<u64>,
    pub child_files: Option<u64>,
    /// Type-specific metadata (see `storecat_extract::FileMeta`), when extracted.
    pub meta: Option<serde_json::Value>,
    /// First part of the extracted text, when any.
    pub text_excerpt: Option<String>,
    pub text_bytes: u64,
    pub has_thumb: bool,
    pub quick_hash: Option<String>,
    pub full_hash: Option<String>,
    /// For archive entries: the archive this entry lives in.
    pub archive: Option<ArchiveRef>,
    /// For archives: how many entries were listed.
    pub archive_entries: Option<u64>,
    pub tags: Vec<String>,
}

impl ItemDetail {
    /// `dir/path/name`, the key tags are stored under (stable across rescans).
    pub fn catalogue_path(&self) -> String {
        if self.path.is_empty() {
            self.name.clone()
        } else {
            format!("{}/{}", self.path, self.name)
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchiveRef {
    pub file_id: i64,
    pub name: String,
    pub path: String,
    pub entry_path: String,
}

// ----- enrichment ---------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "phase")]
pub enum EnrichPhase {
    /// Waiting for another drive's enrichment to finish (one runs at a time).
    Queued,
    Running,
    /// Stopped because the drive went away or the user asked; resumes next time.
    Paused,
    Done,
    Failed {
        message: String,
    },
    /// Stopped because the drive failed to read; not resumed automatically.
    Stopped {
        message: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnrichProgress {
    pub volume_id: VolumeId,
    #[serde(flatten)]
    pub phase: EnrichPhase,
    /// 0 while planning the read order, 1 in the quick pass (hashes, metadata, photo
    /// thumbnails, archives), 2 in the detail pass (document text, video thumbnails).
    /// `done`/`total` count files in the current pass.
    #[serde(default)]
    pub pass: u8,
    pub done: u64,
    pub total: u64,
    pub hashed: u64,
    pub metas: u64,
    pub thumbs: u64,
    pub texts: u64,
    pub archives: u64,
    pub elapsed_ms: u64,
    pub current: String,
}

impl EnrichProgress {
    /// A progress event with no counts yet, e.g. `Queued`.
    pub fn bare(volume_id: VolumeId, phase: EnrichPhase) -> Self {
        Self {
            volume_id,
            phase,
            pass: 0,
            done: 0,
            total: 0,
            hashed: 0,
            metas: 0,
            thumbs: 0,
            texts: 0,
            archives: 0,
            elapsed_ms: 0,
            current: String::new(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EnrichStats {
    pub processed: u64,
    pub hashed: u64,
    pub metas: u64,
    pub thumbs: u64,
    pub texts: u64,
    pub archives: u64,
    pub errors: u64,
    pub elapsed_ms: u64,
    pub paused: bool,
    /// Why the run stopped itself: the drive failed to read (also stored on the volume).
    pub fault: Option<String>,
}

/// A duplicate group being hashed in full ("Verify"), for its progress bar.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VerifyProgress {
    pub hash: String,
    pub bytes_done: u64,
    pub bytes_total: u64,
    pub files_done: u64,
    pub files_total: u64,
    /// Name of the copy being read.
    pub current: String,
    pub done: bool,
}

/// Per-volume enrichment coverage, for the drive card.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EnrichCoverage {
    pub files: u64,
    pub done: u64,
    pub thumbs: u64,
    pub texts: u64,
    pub hashed: u64,
}

// ----- duplicates ---------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DupMember {
    pub volume_id: VolumeId,
    pub volume_label: String,
    pub volume_online: bool,
    pub file_id: i64,
    pub name: String,
    pub path: String,
    pub mtime: i64,
    pub full_hash: Option<String>,
    /// In a folder an application manages (a package cache, build output, app data).
    /// Storecat never offers to delete these.
    #[serde(default)]
    pub app_managed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DupGroup {
    /// Hex quick hash; the group key.
    pub hash: String,
    pub size: u64,
    pub count: u64,
    /// Bytes that could be reclaimed by keeping one of your copies (copies in app-managed
    /// folders don't count: they are never removed).
    pub waste: u64,
    /// Copies outside app-managed folders.
    #[serde(default)]
    pub user_copies: u64,
    /// The copy Storecat would keep: outside temporary and download folders, on a
    /// connected drive, shortest path. `None` when there are no copies of yours.
    #[serde(default)]
    pub suggested_keep: Option<(VolumeId, i64)>,
    pub kind: Kind,
    pub name: String,
    /// All members have a full hash and they agree.
    pub verified: bool,
    pub members: Vec<DupMember>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DupSummary {
    pub groups: u64,
    pub files: u64,
    pub waste: u64,
}

/// How many catalogued files have an extension (for the search filter chips).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtCount {
    pub ext: String,
    pub files: u64,
}

/// Which duplicate groups to list.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DupScope {
    /// Groups with at least two copies outside app-managed folders.
    #[default]
    Mine,
    /// Every group, app caches and build output included.
    All,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DupSort {
    #[default]
    Waste,
    Size,
    Count,
    Name,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DupQuery {
    #[serde(default)]
    pub scope: DupScope,
    #[serde(default)]
    pub sort: DupSort,
    #[serde(default = "yes")]
    pub desc: bool,
    pub limit: usize,
    #[serde(default)]
    pub offset: usize,
    /// Words that must appear in the file name or one of its folders.
    #[serde(default)]
    pub text: String,
    /// Only these kinds (empty: all).
    #[serde(default)]
    pub kinds: Vec<Kind>,
    /// Only groups with a copy on this drive.
    #[serde(default)]
    pub volume_id: Option<VolumeId>,
    /// Only groups that can be cleaned up now: the copy to keep and at least one copy to
    /// remove are on connected drives.
    #[serde(default)]
    pub ready_only: bool,
}

impl DupQuery {
    /// Whether a group passes the filters (the scope is applied separately).
    pub fn keeps(&self, g: &DupGroup) -> bool {
        if !self.kinds.is_empty() && !self.kinds.contains(&g.kind) {
            return false;
        }
        if let Some(vid) = self.volume_id {
            if !g.members.iter().any(|m| m.volume_id == vid) {
                return false;
            }
        }
        if self.ready_only {
            let Some(keep) = g.suggested_keep else {
                return false;
            };
            let keeper_online = g
                .members
                .iter()
                .any(|m| (m.volume_id, m.file_id) == keep && m.volume_online);
            let removable = g
                .members
                .iter()
                .any(|m| (m.volume_id, m.file_id) != keep && !m.app_managed && m.volume_online);
            if !keeper_online || !removable {
                return false;
            }
        }
        let words: Vec<String> = self
            .text
            .split_whitespace()
            .map(|w| w.to_lowercase())
            .collect();
        words.iter().all(|w| {
            g.members.iter().any(|m| {
                m.name.to_lowercase().contains(w.as_str())
                    || m.path.to_lowercase().contains(w.as_str())
            })
        })
    }
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DupPage {
    pub groups: Vec<DupGroup>,
    /// Totals over every group in scope, not just this page.
    pub summary: DupSummary,
}

/// One duplicate group to clean up, keeping one copy.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DupPick {
    pub hash: String,
    pub keep_volume_id: VolumeId,
    pub keep_file_id: i64,
}

// ----- tools ----------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FfmpegStatus {
    pub available: bool,
    pub ffmpeg: Option<String>,
    pub ffprobe: Option<String>,
    pub version: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadProgress {
    pub tool: String,
    pub downloaded: u64,
    pub total: Option<u64>,
    pub done: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CatalogStats {
    pub index_rebuilding: bool,
    /// While the index rebuilds: items indexed so far, of about `index_rebuild_total`.
    #[serde(default)]
    pub index_rebuild_done: u64,
    #[serde(default)]
    pub index_rebuild_total: u64,
    pub volumes: u64,
    pub online: u64,
    pub files: u64,
    pub dirs: u64,
    pub bytes: u64,
    pub capacity: u64,
    pub free_online: u64,
}

pub fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

// ----- operations (Phase 3) -------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobKind {
    /// Copy files, verify, leave originals.
    Copy,
    /// Copy, verify, then delete originals.
    Move,
    /// Send files to the recycle bin / trash.
    Delete,
}

impl JobKind {
    pub fn as_str(self) -> &'static str {
        match self {
            JobKind::Copy => "copy",
            JobKind::Move => "move",
            JobKind::Delete => "delete",
        }
    }
    pub fn parse(s: &str) -> Self {
        match s {
            "move" => JobKind::Move,
            "delete" => JobKind::Delete,
            _ => JobKind::Copy,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConflictPolicy {
    Skip,
    Rename,
    Overwrite,
}

impl ConflictPolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            ConflictPolicy::Skip => "skip",
            ConflictPolicy::Rename => "rename",
            ConflictPolicy::Overwrite => "overwrite",
        }
    }
    pub fn parse(s: &str) -> Self {
        match s {
            "skip" => ConflictPolicy::Skip,
            "overwrite" => ConflictPolicy::Overwrite,
            _ => ConflictPolicy::Rename,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    Planned,
    Running,
    Paused,
    Done,
    Failed,
    Cancelled,
}

impl JobStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            JobStatus::Planned => "planned",
            JobStatus::Running => "running",
            JobStatus::Paused => "paused",
            JobStatus::Done => "done",
            JobStatus::Failed => "failed",
            JobStatus::Cancelled => "cancelled",
        }
    }
    pub fn parse(s: &str) -> Self {
        match s {
            "running" => JobStatus::Running,
            "paused" => JobStatus::Paused,
            "done" => JobStatus::Done,
            "failed" => JobStatus::Failed,
            "cancelled" => JobStatus::Cancelled,
            _ => JobStatus::Planned,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemStatus {
    Pending,
    /// Bytes written to the destination; not yet verified (or verification off).
    Copied,
    /// Destination hash matches the source.
    Verified,
    /// Finished, including source deletion for moves.
    Done,
    Skipped,
    Failed,
}

impl ItemStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            ItemStatus::Pending => "pending",
            ItemStatus::Copied => "copied",
            ItemStatus::Verified => "verified",
            ItemStatus::Done => "done",
            ItemStatus::Skipped => "skipped",
            ItemStatus::Failed => "failed",
        }
    }
    pub fn parse(s: &str) -> Self {
        match s {
            "copied" => ItemStatus::Copied,
            "verified" => ItemStatus::Verified,
            "done" => ItemStatus::Done,
            "skipped" => ItemStatus::Skipped,
            "failed" => ItemStatus::Failed,
            _ => ItemStatus::Pending,
        }
    }
}

/// What the user asked for; turned into a journaled job by `Catalog::create_job`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobSpec {
    pub kind: JobKind,
    pub name: String,
    /// Absolute source paths: files or folders. Folders are copied as a folder.
    pub sources: Vec<String>,
    /// Absolute destination folder (copy/move). Ignored for delete.
    pub dest_root: String,
    pub conflict: ConflictPolicy,
    pub verify: bool,
    /// Delete jobs only, one per source file (or empty): a condition the file must still
    /// meet when the job reaches it, or it is left alone. See `ops::CHECK_SAME_AS` and
    /// `ops::CHECK_APPLEDOUBLE`.
    #[serde(default)]
    pub checks: Vec<String>,
}

/// Dry-run result.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct JobPlan {
    pub files: u64,
    pub bytes: u64,
    pub folders: u64,
    pub conflicts: u64,
    pub missing_sources: Vec<String>,
    pub dest_free: Option<u64>,
    pub enough_space: bool,
    /// First few source → destination pairs, for the preview.
    pub sample: Vec<(String, String)>,
    pub dest_volume_id: Option<VolumeId>,
    pub source_volume_id: Option<VolumeId>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Job {
    pub id: i64,
    pub kind: JobKind,
    pub name: String,
    pub status: JobStatus,
    pub source_volume_id: Option<VolumeId>,
    pub dest_volume_id: Option<VolumeId>,
    pub dest_root: String,
    pub conflict: ConflictPolicy,
    pub verify: bool,
    pub files_total: u64,
    pub bytes_total: u64,
    pub files_done: u64,
    pub bytes_done: u64,
    pub files_failed: u64,
    pub files_skipped: u64,
    pub created_at: i64,
    pub started_at: Option<i64>,
    pub finished_at: Option<i64>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobItem {
    pub id: i64,
    pub job_id: i64,
    pub source: String,
    pub dest: String,
    pub size: u64,
    pub mtime: i64,
    pub status: ItemStatus,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobProgress {
    pub job_id: i64,
    pub status: JobStatus,
    pub files_done: u64,
    pub files_total: u64,
    pub bytes_done: u64,
    pub bytes_total: u64,
    pub files_failed: u64,
    pub current: String,
    pub bytes_per_sec: u64,
    pub elapsed_ms: u64,
}

// ----- annotations ----------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tag {
    pub id: i64,
    pub name: String,
    pub count: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedSearch {
    pub id: i64,
    pub name: String,
    pub query: String,
    pub created_at: i64,
}
