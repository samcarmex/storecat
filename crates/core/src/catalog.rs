//! The `Catalog`: one object the desktop app and the CLI both drive.
//!
//! It owns the master database, the search index, the duplicates index, and knows which
//! volumes are connected right now (fed by the platform layer, which this crate does not
//! depend on).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::thread;
use std::time::Instant;

use crossbeam_channel::bounded;

use crate::db::dupes::DupesDb;
use crate::db::master::{MasterDb, NewVolume};
use crate::db::volume::{FileRecord, ThumbDb, VolumeDb, VolumeDbWriter, STAGE_ARCHIVE};
use crate::enrich::{flags_for, hex};
use crate::error::{Error, Result};
use crate::index::{query, DocInput, SearchIndex};
use crate::model::*;
use crate::paths::DataDirs;
use crate::scan::{self, Counters, DirBatch, ScanOptions};

pub struct Catalog {
    pub dirs: DataDirs,
    pub(crate) master: Mutex<MasterDb>,
    pub(crate) index: SearchIndex,
    pub(crate) dupes: Mutex<DupesDb>,
    detected: RwLock<Vec<DetectedVolume>>,
    scanning: Mutex<HashSet<VolumeId>>,
    pub(crate) enriching: Mutex<HashMap<VolumeId, Arc<AtomicBool>>>,
    pub(crate) scan_gate: Mutex<()>,
    /// Held while live changes are applied, and by enrichment while it updates the index,
    /// so enrichment never re-adds a document for a file that just went away.
    pub(crate) live_gate: Mutex<()>,
    index_rebuilding: AtomicBool,
    rebuild_done: std::sync::atomic::AtomicU64,
    rebuild_total: std::sync::atomic::AtomicU64,
    upgraded: Mutex<HashSet<VolumeId>>,
    pub(crate) running_jobs: Mutex<HashMap<i64, Arc<AtomicBool>>>,
    pub(crate) cancelled_jobs: Mutex<HashSet<i64>>,
    /// Folder volumes (local folders, network shares): reachable?, free, capacity. Filled
    /// by `refresh_folders` off the UI path so an unreachable NAS never blocks a call.
    pub(crate) folder_status: RwLock<HashMap<VolumeId, FolderStatus>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FolderStatus {
    pub online: bool,
    pub free: u64,
    pub capacity: u64,
}

/// Settings keys.
pub mod settings {
    pub const AUTO_RESCAN: &str = "auto_rescan";
    pub const AUTO_ENRICH: &str = "auto_enrich";
    pub const COPY_TO_DRIVE: &str = "copy_catalog_to_drive";
    pub const HASH_MIN_BYTES: &str = "hash_min_bytes";
    /// Watch connected drives and catalogued folders for changes while the app runs.
    pub const LIVE_WATCH: &str = "live_watch";
}

impl Catalog {
    pub fn open(dirs: DataDirs) -> Result<Arc<Self>> {
        dirs.ensure()?;
        let master = MasterDb::open(&dirs.master_db())?;
        let (index, fresh) = SearchIndex::open_or_create(&dirs.index_dir())?;
        let dupes = DupesDb::open(&dirs.dupes_db())?;
        let cat = Arc::new(Self {
            dirs,
            master: Mutex::new(master),
            index,
            dupes: Mutex::new(dupes),
            detected: RwLock::new(Vec::new()),
            scanning: Mutex::new(HashSet::new()),
            enriching: Mutex::new(HashMap::new()),
            scan_gate: Mutex::new(()),
            live_gate: Mutex::new(()),
            index_rebuilding: AtomicBool::new(false),
            rebuild_done: std::sync::atomic::AtomicU64::new(0),
            rebuild_total: std::sync::atomic::AtomicU64::new(0),
            upgraded: Mutex::new(HashSet::new()),
            running_jobs: Mutex::new(HashMap::new()),
            cancelled_jobs: Mutex::new(HashSet::new()),
            folder_status: RwLock::new(HashMap::new()),
        });
        // A rebuild that was interrupted (process exit mid-way) leaves its marker behind.
        let marker = cat.dirs.index_dir().join(".rebuild-pending");
        if fresh || marker.exists() {
            let has_data = cat
                .master()
                .list_volumes()?
                .iter()
                .any(|v| cat.dirs.volume_db(v.id).exists());
            if has_data {
                let _ = std::fs::write(&marker, b"rebuilding");
                let c = Arc::clone(&cat);
                cat.index_rebuilding.store(true, Ordering::Relaxed);
                thread::Builder::new()
                    .name("storecat-reindex".into())
                    .spawn(move || {
                        match c.rebuild_index() {
                            Ok(()) => {
                                let _ = std::fs::remove_file(
                                    c.dirs.index_dir().join(".rebuild-pending"),
                                );
                            }
                            Err(e) => log::error!("index rebuild failed: {e}"),
                        }
                        c.index_rebuilding.store(false, Ordering::Relaxed);
                    })?;
            } else {
                let _ = std::fs::remove_file(&marker);
            }
        }
        Ok(cat)
    }

    /// Block until a startup index rebuild (if any) has finished. The CLI calls this
    /// before exiting so a rebuild is never cut short.
    pub fn wait_for_index(&self) {
        while self.index_rebuilding() {
            thread::sleep(std::time::Duration::from_millis(100));
        }
    }

    pub(crate) fn master(&self) -> std::sync::MutexGuard<'_, MasterDb> {
        self.master.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Open a volume's catalogue read-only, upgrading a Phase 1 file first (once).
    pub(crate) fn volume_db(&self, id: VolumeId) -> Result<VolumeDb> {
        let path = self.dirs.volume_db(id);
        if !path.exists() {
            return Err(Error::Other("volume has not been scanned yet".into()));
        }
        let upgraded = {
            let set = self.upgraded.lock().unwrap_or_else(|e| e.into_inner());
            set.contains(&id)
        };
        if !upgraded {
            VolumeDb::ensure_schema(&path)?;
            self.upgraded
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(id);
        }
        VolumeDb::open(&path)
    }

    pub fn index_rebuilding(&self) -> bool {
        self.index_rebuilding.load(Ordering::Relaxed)
    }

    // ----- live volume state -------------------------------------------------------

    /// Feed the current set of connected volumes. Returns true when anything changed.
    pub fn refresh_detected(&self, detected: Vec<DetectedVolume>) -> Result<bool> {
        let changed = {
            let cur = self.detected.read().unwrap_or_else(|e| e.into_inner());
            *cur != detected
        };
        if changed {
            let master = self.master();
            for d in &detected {
                if let Some(v) = master.find_by_identity(&d.identity)? {
                    master.touch_seen(v.id, d)?;
                }
            }
            drop(master);
            let mut cur = self.detected.write().unwrap_or_else(|e| e.into_inner());
            *cur = detected;
        }
        Ok(changed)
    }

    pub fn detected(&self) -> Vec<DetectedVolume> {
        self.detected
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn detected_uncatalogued(&self) -> Result<Vec<DetectedVolume>> {
        let known: HashSet<String> = self
            .master()
            .list_volumes()?
            .into_iter()
            .map(|v| v.identity)
            .collect();
        Ok(self
            .detected()
            .into_iter()
            .filter(|d| !known.contains(&d.identity))
            .collect())
    }

    fn overlay(&self, mut v: Volume) -> Volume {
        match v.source {
            VolumeSource::Device => {
                let detected = self.detected.read().unwrap_or_else(|e| e.into_inner());
                if let Some(d) = detected.iter().find(|d| d.identity == v.identity) {
                    v.online = true;
                    v.mount_path = Some(d.mount_path.clone());
                    v.free = d.free;
                    if d.capacity > 0 {
                        v.capacity = d.capacity;
                    }
                }
            }
            VolumeSource::Folder => {
                let cached = self
                    .folder_status
                    .read()
                    .unwrap_or_else(|e| e.into_inner())
                    .get(&v.id)
                    .copied();
                if let Some(p) = v.last_mount_path.clone() {
                    let online = match cached {
                        Some(st) => {
                            if st.capacity > 0 {
                                v.free = st.free;
                                v.capacity = st.capacity;
                            }
                            st.online
                        }
                        // Never touch a network path on this thread: an unreachable
                        // server can stall for many seconds. The watcher fills the cache.
                        None if crate::storage::is_network_path(&p, v.media_type) => false,
                        None => Path::new(&p).is_dir(),
                    };
                    if online {
                        v.online = true;
                        v.mount_path = Some(p);
                    }
                }
            }
        }
        v
    }

    pub fn volumes(&self) -> Result<Vec<Volume>> {
        let rows = self.master().list_volumes()?;
        Ok(rows.into_iter().map(|v| self.overlay(v)).collect())
    }

    pub fn volume(&self, id: VolumeId) -> Result<Volume> {
        let v = self
            .master()
            .get_volume(id)?
            .ok_or(Error::VolumeNotFound(id))?;
        Ok(self.overlay(v))
    }

    pub fn stats(&self) -> Result<CatalogStats> {
        let vols = self.volumes()?;
        let mut s = CatalogStats {
            index_rebuilding: self.index_rebuilding(),
            index_rebuild_done: self.rebuild_done.load(Ordering::Relaxed),
            index_rebuild_total: self.rebuild_total.load(Ordering::Relaxed),
            volumes: vols.len() as u64,
            ..Default::default()
        };
        let nested: HashSet<VolumeId> = crate::suggest::overlaps(&vols)
            .into_iter()
            .map(|(inner, _)| inner)
            .collect();
        for v in &vols {
            if nested.contains(&v.id) {
                continue;
            }
            s.files += v.file_count;
            s.dirs += v.dir_count;
            s.bytes += v.total_bytes;
            s.capacity += v.capacity;
            if v.online {
                s.online += 1;
                s.free_online += v.free;
            }
        }
        Ok(s)
    }

    // ----- adding / editing / removing ----------------------------------------------

    pub fn add_detected(&self, identity: &str) -> Result<Volume> {
        let d = self
            .detected()
            .into_iter()
            .find(|d| d.identity == identity)
            .ok_or(Error::VolumeOffline)?;
        let master = self.master();
        if let Some(v) = master.find_by_identity(identity)? {
            drop(master);
            return self.volume(v.id);
        }
        let id = master.insert_volume(&NewVolume::from_detected(&d))?;
        master.touch_seen(id, &d)?;
        drop(master);
        self.volume(id)
    }

    pub fn add_folder(&self, path: &Path, label: Option<&str>) -> Result<Volume> {
        let canonical = normalise_folder(path)?;
        let identity = format!("path:{}", canonical.to_lowercase());
        let master = self.master();
        if let Some(v) = master.find_by_identity(&identity)? {
            drop(master);
            return self.volume(v.id);
        }
        let default_label = Path::new(&canonical)
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| canonical.clone());
        let media = if canonical.starts_with("\\\\") || canonical.starts_with("//") {
            MediaType::Network
        } else {
            MediaType::Unknown
        };
        let id = master.insert_volume(&NewVolume {
            identity: &identity,
            source: VolumeSource::Folder,
            label: label.unwrap_or(&default_label),
            fs: "",
            capacity: 0,
            free: 0,
            media_type: media,
            mount_path: &canonical,
        })?;
        drop(master);
        self.volume(id)
    }

    pub fn update_volume(&self, id: VolumeId, patch: &VolumePatch) -> Result<Volume> {
        self.master().update_volume(id, patch)?;
        self.volume(id)
    }

    pub fn remove_volume(&self, id: VolumeId) -> Result<()> {
        if self.is_scanning(id) || self.is_enriching(id) {
            return Err(Error::ScanInProgress);
        }
        self.index.delete_volume(id)?;
        self.index.commit()?;
        self.dupes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove_volume(id)?;
        self.master().delete_volume(id)?;
        for p in [
            self.dirs.volume_db(id),
            self.dirs.volume_db_tmp(id),
            self.dirs.thumbs_db(id),
            self.dirs.thumbs_db_tmp(id),
            self.dirs.volume_photo(id),
        ] {
            let _ = std::fs::remove_file(&p);
            let _ = std::fs::remove_file(format!("{}-wal", p.display()));
            let _ = std::fs::remove_file(format!("{}-shm", p.display()));
        }
        Ok(())
    }

    // ----- scanning -------------------------------------------------------------------

    pub fn is_scanning(&self, id: VolumeId) -> bool {
        self.scanning
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains(&id)
    }

    /// A scan of this volume started but never finished (the app was closed, crashed or
    /// lost power): its half-built catalogue is still lying around.
    pub fn scan_interrupted(&self, id: VolumeId) -> bool {
        !self.is_scanning(id) && self.dirs.volume_db_tmp(id).exists()
    }

    pub fn scanning_ids(&self) -> Vec<VolumeId> {
        self.scanning
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .copied()
            .collect()
    }

    /// Catalogue a volume from scratch, carrying enrichment over for unchanged files.
    /// Blocking: run it on a worker thread.
    pub fn scan(
        &self,
        id: VolumeId,
        opts: &ScanOptions,
        progress: &(dyn Fn(ScanProgress) + Sync),
    ) -> Result<ScanStats> {
        let volume = self.volume(id)?;
        let root = volume.mount_path.clone().ok_or(Error::VolumeOffline)?;
        if self.is_enriching(id) {
            self.cancel_enrich(id);
            // Give the enrichment loop a moment to finish its batch.
            for _ in 0..200 {
                if !self.is_enriching(id) {
                    break;
                }
                thread::sleep(std::time::Duration::from_millis(50));
            }
        }
        {
            let mut scanning = self.scanning.lock().unwrap_or_else(|e| e.into_inner());
            if !scanning.insert(id) {
                return Err(Error::ScanInProgress);
            }
        }
        let _gate = self.scan_gate.lock().unwrap_or_else(|e| e.into_inner());
        let result = self.scan_inner(id, &volume, &root, opts, progress);
        self.scanning
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&id);
        match &result {
            Ok(stats) => progress(ScanProgress {
                volume_id: id,
                phase: ScanPhase::Done,
                files: stats.files,
                dirs: stats.dirs,
                bytes: stats.bytes,
                errors: stats.errors,
                elapsed_ms: stats.elapsed_ms,
                current: String::new(),
                indexed: 0,
                index_total: 0,
            }),
            Err(e) => progress(ScanProgress {
                volume_id: id,
                phase: ScanPhase::Failed {
                    message: e.to_string(),
                },
                files: 0,
                dirs: 0,
                bytes: 0,
                errors: 0,
                elapsed_ms: 0,
                current: String::new(),
                indexed: 0,
                index_total: 0,
            }),
        }
        result
    }

    fn scan_inner(
        &self,
        id: VolumeId,
        volume: &Volume,
        root: &str,
        opts: &ScanOptions,
        progress: &(dyn Fn(ScanProgress) + Sync),
    ) -> Result<ScanStats> {
        let started = Instant::now();
        let scan_id = self.master().begin_scan(id)?;
        let tmp_path = self.dirs.volume_db_tmp(id);
        let live_path = self.dirs.volume_db(id);
        let thumbs_tmp = self.dirs.thumbs_db_tmp(id);
        let thumbs_live = self.dirs.thumbs_db(id);
        let _ = std::fs::remove_file(&thumbs_tmp);

        let run = || -> Result<ScanStats> {
            let mut writer = VolumeDbWriter::create(&tmp_path)?;
            writer.set_info("root", root)?;
            writer.set_info("scanned_at", &now_unix().to_string())?;
            writer.set_info("identity", &volume.identity)?;
            writer.set_info("label", &volume.label)?;
            writer.set_info("source", volume.source.as_str())?;
            writer.set_info("capacity", &volume.capacity.to_string())?;
            writer.set_info("fs", &volume.fs)?;
            writer.set_info("media_type", volume.media_type.as_str())?;

            self.index.delete_volume(id)?;

            let counters = Arc::new(Counters::default());
            let (tx, rx) = bounded::<DirBatch>(4096);
            let db_thread = thread::Builder::new()
                .name("storecat-dbwriter".into())
                .spawn(move || -> Result<VolumeDbWriter> {
                    for batch in rx {
                        writer.write_batch(&batch)?;
                    }
                    Ok(writer)
                })?;

            let index = &self.index;
            let sink_tx = tx;
            let sink_counters = Arc::clone(&counters);
            let last_report = Mutex::new(Instant::now());
            let sink = |batch: DirBatch| {
                if let Err(e) = index.add_batch(id, &batch) {
                    log::error!("index add failed: {e}");
                }
                if let Ok(mut last) = last_report.try_lock() {
                    if last.elapsed().as_millis() >= 200 {
                        *last = Instant::now();
                        progress(ScanProgress {
                            volume_id: id,
                            phase: ScanPhase::Walking,
                            files: sink_counters.files.load(Ordering::Relaxed),
                            dirs: sink_counters.dirs.load(Ordering::Relaxed),
                            bytes: sink_counters.bytes.load(Ordering::Relaxed),
                            errors: sink_counters.errors.load(Ordering::Relaxed),
                            elapsed_ms: started.elapsed().as_millis() as u64,
                            current: sink_counters.current(),
                            indexed: 0,
                            index_total: 0,
                        });
                    }
                }
                let _ = sink_tx.send(batch);
            };

            let walk_ms = scan::walk(Path::new(root), opts, &counters, &sink);
            drop(sink_tx);
            let writer = db_thread
                .join()
                .map_err(|_| Error::Other("db writer thread panicked".into()))??;
            let walk_ms = walk_ms?;

            progress(ScanProgress {
                volume_id: id,
                phase: ScanPhase::Committing,
                files: counters.files.load(Ordering::Relaxed),
                dirs: counters.dirs.load(Ordering::Relaxed),
                bytes: counters.bytes.load(Ordering::Relaxed),
                errors: counters.errors.load(Ordering::Relaxed),
                elapsed_ms: walk_ms,
                current: String::new(),
                indexed: 0,
                index_total: 0,
            });
            writer.finish()?;

            // Carry enrichment over from the previous catalogue for unchanged files.
            let mut carried = 0u64;
            let tag_map = self.master().tag_map(id)?;
            if live_path.exists() {
                let db = VolumeDb::open_rw(&tmp_path)?;
                carried = db.carry_over(&live_path, Some(&thumbs_live), Some(&thumbs_tmp))?;
                if carried > 0 {
                    let ids = db.enriched_file_ids()?;
                    let index_total = ids.len() as u64;
                    let mut last_report = Instant::now();
                    for (n, fid) in ids.into_iter().enumerate() {
                        if last_report.elapsed() >= std::time::Duration::from_millis(250) {
                            last_report = Instant::now();
                            progress(ScanProgress {
                                volume_id: id,
                                phase: ScanPhase::Committing,
                                files: counters.files.load(Ordering::Relaxed),
                                dirs: counters.dirs.load(Ordering::Relaxed),
                                bytes: counters.bytes.load(Ordering::Relaxed),
                                errors: counters.errors.load(Ordering::Relaxed),
                                elapsed_ms: started.elapsed().as_millis() as u64,
                                current: String::new(),
                                indexed: n as u64,
                                index_total,
                            });
                        }
                        let Some(f) = db.file(fid)? else { continue };
                        let stages = db.stages(fid)?;
                        let meta = db.meta_json(fid)?;
                        let text = db.text(fid)?;
                        let entries: Vec<(i64, storecat_extract::ArchiveEntry)> =
                            if stages & STAGE_ARCHIVE != 0 {
                                db.entries_for(fid)?
                                    .into_iter()
                                    .map(|e| {
                                        (
                                            e.id,
                                            storecat_extract::ArchiveEntry {
                                                path: e.path,
                                                size: e.size,
                                                mtime: e.mtime,
                                                is_dir: e.is_dir,
                                            },
                                        )
                                    })
                                    .collect()
                            } else {
                                Vec::new()
                            };
                        let tags = tag_map
                            .get(&crate::enrich::item_path(&f))
                            .map(|v| v.as_slice())
                            .unwrap_or(&[]);
                        self.index_enriched_file(
                            id,
                            &f,
                            meta.as_deref(),
                            text.as_deref(),
                            stages,
                            &entries,
                            tags,
                        )?;
                    }
                }
            }
            // Tags live by path in master.db, so they survive rescans; re-attach them to
            // files that were not re-indexed above (no enrichment yet) and to folders.
            if !tag_map.is_empty() {
                let db = VolumeDb::open_rw(&tmp_path)?;
                for (path, tags) in &tag_map {
                    if let Some(f) = db.file_by_path(path)? {
                        if db.stages(f.id)? == 0 {
                            self.index_enriched_file(id, &f, None, None, 0, &[], tags)?;
                        }
                    } else if let Some(d) = db.dir_by_path(path)? {
                        self.index.replace_doc(&DocInput {
                            volume_id: id,
                            item_id: d.id,
                            item_type: ItemType::Dir,
                            name: &d.name,
                            path: crate::index::parent_of(&d.path),
                            ext: "",
                            kind: Kind::Dir,
                            size: 0,
                            mtime: d.mtime,
                            flags: 0,
                            parent_key: 0,
                            content: None,
                            meta_text: None,
                            tags,
                        })?;
                    }
                }
            }
            self.index.commit()?;

            // Swap the new catalogue (and thumbnails) into place.
            for p in [&live_path, &thumbs_live] {
                let _ = std::fs::remove_file(format!("{}-wal", p.display()));
                let _ = std::fs::remove_file(format!("{}-shm", p.display()));
                if p.exists() {
                    std::fs::remove_file(p)?;
                }
            }
            std::fs::rename(&tmp_path, &live_path)?;
            if thumbs_tmp.exists() {
                std::fs::rename(&thumbs_tmp, &thumbs_live)?;
            }
            log::info!("volume {id}: carried enrichment for {carried} unchanged files");

            Ok(ScanStats {
                files: counters.files.load(Ordering::Relaxed),
                dirs: counters.dirs.load(Ordering::Relaxed),
                bytes: counters.bytes.load(Ordering::Relaxed),
                errors: counters.errors.load(Ordering::Relaxed),
                skipped_symlinks: counters.symlinks.load(Ordering::Relaxed),
                elapsed_ms: started.elapsed().as_millis() as u64,
            })
        };

        match run() {
            Ok(stats) => {
                self.master().finish_scan(scan_id, id, &stats)?;
                if let Ok(db) = self.volume_db(id) {
                    let min = self.hash_min_bytes();
                    let _ = self.refresh_dupes_for(id, &db, min);
                }
                self.copy_catalog_to_drive(volume, root);
                Ok(stats)
            }
            Err(e) => {
                let _ = self.index.rollback();
                let _ = std::fs::remove_file(&tmp_path);
                let _ = std::fs::remove_file(&thumbs_tmp);
                let _ = self.master().fail_scan(scan_id, &e.to_string());
                Err(e)
            }
        }
    }

    pub fn hash_min_bytes(&self) -> u64 {
        self.setting(settings::HASH_MIN_BYTES)
            .ok()
            .flatten()
            .and_then(|s| s.parse().ok())
            .unwrap_or(1024 * 1024)
    }

    pub fn setting_enabled(&self, key: &str, default: bool) -> bool {
        self.setting(key)
            .ok()
            .flatten()
            .map(|v| v != "false" && v != "0")
            .unwrap_or(default)
    }

    /// Put a copy of the catalogue on the drive itself (`<root>/.storecat/`). Best effort:
    /// read-only media and permission errors are simply skipped. The drive the OS runs
    /// from is skipped too: it never travels to another computer, and a folder at the
    /// root of `C:\` is not something people expect an app to create.
    fn copy_catalog_to_drive(&self, volume: &Volume, root: &str) {
        if volume.source != VolumeSource::Device
            || crate::paths::is_system_drive(root)
            || !self.setting_enabled(settings::COPY_TO_DRIVE, true)
        {
            return;
        }
        let dir = Path::new(root).join(".storecat");
        if std::fs::create_dir_all(&dir).is_err() {
            return;
        }
        let dest = dir.join("catalog.db");
        match std::fs::copy(self.dirs.volume_db(volume.id), &dest) {
            Ok(_) => {
                let info = serde_json::json!({
                    "app": "Storecat",
                    "identity": volume.identity,
                    "label": volume.label,
                    "scanned_at": now_unix(),
                    "files": volume.file_count,
                });
                let _ = std::fs::write(dir.join("storecat.json"), info.to_string());
                log::info!("copied catalogue to {}", dest.display());
            }
            Err(e) => log::debug!("could not copy catalogue to drive: {e}"),
        }
    }

    // ----- index rebuild ------------------------------------------------------------

    /// Rebuild the whole search index from the per-volume databases.
    pub fn rebuild_index(&self) -> Result<()> {
        let started = Instant::now();
        let vols = self.master().list_volumes()?;
        self.rebuild_done.store(0, Ordering::Relaxed);
        self.rebuild_total.store(
            vols.iter().map(|v| v.file_count + v.dir_count).sum(),
            Ordering::Relaxed,
        );
        for v in vols {
            if self.dirs.volume_db(v.id).exists() {
                self.index_volume_from_db(v.id)?;
            }
        }
        self.index.commit()?;
        log::info!("index rebuilt in {:.1}s", started.elapsed().as_secs_f64());
        Ok(())
    }

    /// (Re)index one volume from its database, enrichment included.
    pub fn index_volume_from_db(&self, id: VolumeId) -> Result<()> {
        let db = self.volume_db(id)?;
        let tag_map = self.master().tag_map(id)?;
        self.index.delete_volume(id)?;
        let mut after = 0i64;
        loop {
            let dirs = db.dirs_after(after, 5000)?;
            if dirs.is_empty() {
                break;
            }
            for d in &dirs {
                after = d.id;
                if d.id == 0 {
                    continue;
                }
                self.rebuild_done.fetch_add(1, Ordering::Relaxed);
                self.index.add_doc(&DocInput {
                    volume_id: id,
                    item_id: d.id,
                    item_type: ItemType::Dir,
                    name: &d.name,
                    path: crate::index::parent_of(&d.path),
                    ext: "",
                    kind: Kind::Dir,
                    size: 0,
                    mtime: d.mtime,
                    flags: 0,
                    parent_key: 0,
                    content: None,
                    meta_text: None,
                    tags: tag_map.get(&d.path).map(|v| v.as_slice()).unwrap_or(&[]),
                })?;
            }
        }
        let mut after = 0i64;
        loop {
            let files = db.files_after(after, 5000)?;
            if files.is_empty() {
                break;
            }
            for f in &files {
                after = f.id;
                self.rebuild_done.fetch_add(1, Ordering::Relaxed);
                self.index_file_from_db(id, &db, f, &tag_map)?;
            }
        }
        Ok(())
    }

    /// (Re)index one file from its volume database, enrichment included.
    pub(crate) fn index_file_from_db(
        &self,
        id: VolumeId,
        db: &VolumeDb,
        f: &FileRecord,
        tag_map: &HashMap<String, Vec<String>>,
    ) -> Result<()> {
        let stages = db.stages(f.id)?;
        let tags = tag_map
            .get(&crate::enrich::item_path(f))
            .map(|v| v.as_slice())
            .unwrap_or(&[]);
        if stages == 0 {
            self.index.add_doc(&DocInput {
                volume_id: id,
                item_id: f.id,
                item_type: ItemType::File,
                name: &f.name,
                path: &f.path,
                ext: &f.ext,
                kind: f.kind,
                size: f.size,
                mtime: f.mtime,
                flags: flags_for(0, f.attrs),
                parent_key: 0,
                content: None,
                meta_text: None,
                tags,
            })?;
            return Ok(());
        }
        let meta = db.meta_json(f.id)?;
        let text = db.text(f.id)?;
        let entries: Vec<(i64, storecat_extract::ArchiveEntry)> = if stages & STAGE_ARCHIVE != 0 {
            db.entries_for(f.id)?
                .into_iter()
                .map(|e| {
                    (
                        e.id,
                        storecat_extract::ArchiveEntry {
                            path: e.path,
                            size: e.size,
                            mtime: e.mtime,
                            is_dir: e.is_dir,
                        },
                    )
                })
                .collect()
        } else {
            Vec::new()
        };
        self.index_enriched_file(
            id,
            f,
            meta.as_deref(),
            text.as_deref(),
            stages,
            &entries,
            tags,
        )?;
        Ok(())
    }

    /// Enrichment saves rows every second but commits the search index less often. If the
    /// app stopped in between (power cut, crash), re-index the files saved since the index
    /// last caught up. Cheap when there is nothing to do.
    pub fn repair_index(&self, id: VolumeId) -> Result<usize> {
        if !self.dirs.volume_db(id).exists() {
            return Ok(0);
        }
        let db = self.volume_db(id)?;
        let Some(since) = db
            .info(crate::enrich::INDEX_SYNCED_AT)?
            .and_then(|v| v.parse::<i64>().ok())
        else {
            return Ok(0);
        };
        let ids = db.enriched_since(since)?;
        if !ids.is_empty() {
            let tag_map = self.master().tag_map(id)?;
            for fid in &ids {
                if let Some(f) = db.file(*fid)? {
                    self.index_file_from_db(id, &db, &f, &tag_map)?;
                }
            }
            self.index.commit()?;
            log::info!(
                "re-indexed {} files of volume {id} after an interrupted run",
                ids.len()
            );
        }
        crate::enrich::mark_index_synced(&VolumeDb::open_rw(&self.dirs.volume_db(id))?)?;
        Ok(ids.len())
    }

    /// `repair_index` for every catalogued volume; run once at startup.
    pub fn repair_indexes(&self) {
        let Ok(volumes) = self.volumes() else {
            return;
        };
        for v in volumes {
            if self.is_enriching(v.id) || self.is_scanning(v.id) {
                continue;
            }
            if let Err(e) = self.repair_index(v.id) {
                log::warn!("index repair of volume {} failed: {e}", v.id);
            }
        }
    }

    // ----- search ---------------------------------------------------------------------

    pub fn search(&self, input: &str, opts: &SearchOptions) -> Result<SearchResponse> {
        let parsed = query::parse(input);
        let volumes = self.volumes()?;
        let by_id: HashMap<VolumeId, &Volume> = volumes.iter().map(|v| (v.id, v)).collect();

        let mut filter: Option<Vec<VolumeId>> = opts.volume_ids.clone();
        if !parsed.drives.is_empty() {
            let matched: Vec<VolumeId> = volumes
                .iter()
                .filter(|v| {
                    let hay = format!("{} {} {}", v.label, v.location, v.notes).to_lowercase();
                    parsed.drives.iter().any(|d| hay.contains(d.as_str()))
                })
                .map(|v| v.id)
                .collect();
            filter = Some(match filter {
                Some(existing) => existing
                    .into_iter()
                    .filter(|id| matched.contains(id))
                    .collect(),
                None => matched,
            });
            if filter.as_ref().map(|f| f.is_empty()).unwrap_or(false) {
                return Ok(SearchResponse {
                    hits: vec![],
                    total: 0,
                    elapsed_ms: 0.0,
                    text: parsed.text,
                    sort: opts.sort,
                });
            }
        }
        if parsed.is_empty() && filter.is_none() {
            return Ok(SearchResponse {
                hits: vec![],
                total: 0,
                elapsed_ms: 0.0,
                text: parsed.text,
                sort: opts.sort,
            });
        }

        // Nothing to rank by (just filters such as ext:mp3): list by name, not at random.
        let sort = if opts.sort == SortBy::Relevance
            && parsed.terms.is_empty()
            && parsed.phrases.is_empty()
        {
            SortBy::NameAsc
        } else {
            opts.sort
        };
        let raw = self
            .index
            .search(&parsed, filter.as_deref(), sort, opts.limit, opts.offset)?;
        let hits = raw
            .hits
            .into_iter()
            .map(|h| {
                let v = by_id.get(&(h.volume_id as i64));
                SearchHit {
                    volume_id: h.volume_id as i64,
                    item_id: h.item_id as i64,
                    item_type: h.item_type,
                    is_dir: h.item_type == ItemType::Dir,
                    flags: h.flags,
                    name: h.name,
                    path: h.path,
                    ext: h.ext,
                    kind: h.kind,
                    size: h.size,
                    mtime: h.mtime,
                    score: h.score,
                    volume_label: v.map(|v| v.label.clone()).unwrap_or_default(),
                    volume_online: v.map(|v| v.online).unwrap_or(false),
                }
            })
            .collect();
        Ok(SearchResponse {
            hits,
            total: raw.total,
            elapsed_ms: raw.elapsed_ms,
            text: parsed.text,
            sort,
        })
    }

    // ----- item detail ----------------------------------------------------------------

    pub fn item_detail(
        &self,
        volume_id: VolumeId,
        item_id: i64,
        item_type: ItemType,
    ) -> Result<ItemDetail> {
        let v = self.volume(volume_id)?;
        let db = self.volume_db(volume_id)?;
        let mut d = ItemDetail {
            volume_id,
            item_id,
            item_type,
            is_dir: item_type == ItemType::Dir,
            name: String::new(),
            path: String::new(),
            ext: String::new(),
            kind: Kind::Other,
            size: 0,
            mtime: 0,
            ctime: 0,
            attrs: 0,
            absolute_path: None,
            volume_label: v.label.clone(),
            volume_online: v.online,
            volume_location: v.location.clone(),
            placeholder: false,
            child_dirs: None,
            child_files: None,
            meta: None,
            text_excerpt: None,
            text_bytes: 0,
            has_thumb: false,
            quick_hash: None,
            full_hash: None,
            archive: None,
            archive_entries: None,
            tags: Vec::new(),
        };
        match item_type {
            ItemType::Dir => {
                let dir = db
                    .dir(item_id)?
                    .ok_or_else(|| Error::Other("directory not in catalogue".into()))?;
                let counts = db.child_counts(dir.id)?;
                d.path = crate::index::parent_of(&dir.path).to_string();
                d.name = dir.name;
                d.kind = Kind::Dir;
                d.mtime = dir.mtime;
                d.child_dirs = Some(counts.0);
                d.child_files = Some(counts.1);
                d.absolute_path = v.mount_path.as_ref().map(|m| join_abs(m, &d.path, &d.name));
            }
            ItemType::File => {
                let f = db
                    .file(item_id)?
                    .ok_or_else(|| Error::Other("file not in catalogue".into()))?;
                d.name = f.name;
                d.path = f.path;
                d.ext = f.ext;
                d.kind = f.kind;
                d.size = f.size;
                d.mtime = f.mtime;
                d.ctime = f.ctime;
                d.attrs = f.attrs;
                d.placeholder = scan::is_placeholder(f.attrs);
                d.absolute_path = v.mount_path.as_ref().map(|m| join_abs(m, &d.path, &d.name));
                d.meta = db
                    .meta_json(item_id)?
                    .and_then(|j| serde_json::from_str(&j).ok());
                if let Some(t) = db.text(item_id)? {
                    d.text_bytes = t.len() as u64;
                    let mut cut = t.len().min(1200);
                    while !t.is_char_boundary(cut) {
                        cut -= 1;
                    }
                    d.text_excerpt = Some(t[..cut].to_string());
                }
                let (q, full) = db.hashes(item_id)?;
                d.quick_hash = q.map(|b| hex(&b));
                d.full_hash = full.map(|b| hex(&b));
                d.has_thumb = db.has_thumb_flag(item_id)?;
                if d.kind == Kind::Archive {
                    let n = db.archive_entry_count(item_id)?;
                    if n > 0 {
                        d.archive_entries = Some(n);
                    }
                }
            }
            ItemType::ArchiveEntry => {
                let e = db
                    .archive_entry(item_id)?
                    .ok_or_else(|| Error::Other("archive entry not in catalogue".into()))?;
                let archive = db
                    .file(e.file_id)?
                    .ok_or_else(|| Error::Other("archive not in catalogue".into()))?;
                let (dir, base) = match e.path.rfind('/') {
                    Some(i) => (e.path[..i].to_string(), e.path[i + 1..].to_string()),
                    None => (String::new(), e.path.clone()),
                };
                d.name = base;
                d.ext = crate::kind::split_ext(&d.name);
                d.kind = if e.is_dir {
                    Kind::Dir
                } else {
                    crate::kind::kind_for_file(&d.ext, e.size)
                };
                d.size = e.size;
                d.mtime = e.mtime;
                let archive_rel = if archive.path.is_empty() {
                    archive.name.clone()
                } else {
                    format!("{}/{}", archive.path, archive.name)
                };
                d.path = if dir.is_empty() {
                    archive_rel.clone()
                } else {
                    format!("{archive_rel}/{dir}")
                };
                d.absolute_path = v
                    .mount_path
                    .as_ref()
                    .map(|m| join_abs(m, &archive.path, &archive.name));
                d.archive = Some(ArchiveRef {
                    file_id: archive.id,
                    name: archive.name,
                    path: archive.path,
                    entry_path: e.path,
                });
            }
        }
        if item_type != ItemType::ArchiveEntry {
            d.tags = self.master().item_tags(volume_id, &d.catalogue_path())?;
        }
        Ok(d)
    }

    // ----- tags, saved searches, photos --------------------------------------------------

    pub fn tags(&self) -> Result<Vec<Tag>> {
        self.master().tags()
    }

    /// Add a tag to a file or folder and refresh its index document.
    pub fn tag_item(
        &self,
        volume_id: VolumeId,
        item_id: i64,
        item_type: ItemType,
        tag: &str,
    ) -> Result<Vec<String>> {
        let tag = tag.trim();
        if tag.is_empty() || tag.len() > 64 || item_type == ItemType::ArchiveEntry {
            return Err(Error::Other(
                "tags are 1–64 characters on files and folders".into(),
            ));
        }
        let d = self.item_detail(volume_id, item_id, item_type)?;
        self.master()
            .tag_item(volume_id, &d.catalogue_path(), tag)?;
        self.reindex_tagged(volume_id, item_id, item_type)
    }

    pub fn untag_item(
        &self,
        volume_id: VolumeId,
        item_id: i64,
        item_type: ItemType,
        tag: &str,
    ) -> Result<Vec<String>> {
        let d = self.item_detail(volume_id, item_id, item_type)?;
        self.master()
            .untag_item(volume_id, &d.catalogue_path(), tag)?;
        self.reindex_tagged(volume_id, item_id, item_type)
    }

    fn reindex_tagged(
        &self,
        volume_id: VolumeId,
        item_id: i64,
        item_type: ItemType,
    ) -> Result<Vec<String>> {
        let db = self.volume_db(volume_id)?;
        let tags = match item_type {
            ItemType::File => {
                let f = db
                    .file(item_id)?
                    .ok_or_else(|| Error::Other("file not in catalogue".into()))?;
                let tags = self
                    .master()
                    .item_tags(volume_id, &crate::enrich::item_path(&f))?;
                let stages = db.stages(f.id)?;
                let meta = db.meta_json(f.id)?;
                let text = db.text(f.id)?;
                let entries: Vec<(i64, storecat_extract::ArchiveEntry)> =
                    if stages & STAGE_ARCHIVE != 0 {
                        db.entries_for(f.id)?
                            .into_iter()
                            .map(|e| {
                                (
                                    e.id,
                                    storecat_extract::ArchiveEntry {
                                        path: e.path,
                                        size: e.size,
                                        mtime: e.mtime,
                                        is_dir: e.is_dir,
                                    },
                                )
                            })
                            .collect()
                    } else {
                        Vec::new()
                    };
                self.index_enriched_file(
                    volume_id,
                    &f,
                    meta.as_deref(),
                    text.as_deref(),
                    stages,
                    &entries,
                    &tags,
                )?;
                tags
            }
            ItemType::Dir => {
                let d = db
                    .dir(item_id)?
                    .ok_or_else(|| Error::Other("directory not in catalogue".into()))?;
                let tags = self.master().item_tags(volume_id, &d.path)?;
                self.index.replace_doc(&DocInput {
                    volume_id,
                    item_id: d.id,
                    item_type: ItemType::Dir,
                    name: &d.name,
                    path: crate::index::parent_of(&d.path),
                    ext: "",
                    kind: Kind::Dir,
                    size: 0,
                    mtime: d.mtime,
                    flags: 0,
                    parent_key: 0,
                    content: None,
                    meta_text: None,
                    tags: &tags,
                })?;
                tags
            }
            ItemType::ArchiveEntry => Vec::new(),
        };
        self.index.commit()?;
        Ok(tags)
    }

    pub fn saved_searches(&self) -> Result<Vec<SavedSearch>> {
        self.master().saved_searches()
    }

    pub fn save_search(&self, name: &str, query: &str) -> Result<SavedSearch> {
        if name.trim().is_empty() || query.trim().is_empty() {
            return Err(Error::Other(
                "a saved search needs a name and a query".into(),
            ));
        }
        let id = self.master().save_search(name, query)?;
        Ok(SavedSearch {
            id,
            name: name.trim().to_string(),
            query: query.trim().to_string(),
            created_at: now_unix(),
        })
    }

    pub fn delete_saved_search(&self, id: i64) -> Result<()> {
        self.master().delete_saved_search(id)
    }

    /// Store a photo of the physical drive (resized to 512 px) for the drive card.
    pub fn set_volume_photo(&self, id: VolumeId, image_path: &Path) -> Result<()> {
        self.volume(id)?;
        let name = image_path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let ext = crate::kind::split_ext(&name);
        let thumb = storecat_extract::make_thumbnail(image_path, &ext, 512)
            .ok_or_else(|| Error::Other("could not read that image".into()))?;
        std::fs::write(self.dirs.volume_photo(id), thumb.jpeg)?;
        self.master().set_photo_flag(id, true)
    }

    pub fn clear_volume_photo(&self, id: VolumeId) -> Result<()> {
        let _ = std::fs::remove_file(self.dirs.volume_photo(id));
        self.master().set_photo_flag(id, false)
    }

    pub fn volume_photo(&self, id: VolumeId) -> Option<Vec<u8>> {
        std::fs::read(self.dirs.volume_photo(id)).ok()
    }

    /// Absolute path of an item if its volume is online. For archive entries, the archive.
    pub fn absolute_path(
        &self,
        volume_id: VolumeId,
        item_id: i64,
        item_type: ItemType,
    ) -> Result<PathBuf> {
        let d = self.item_detail(volume_id, item_id, item_type)?;
        d.absolute_path
            .map(PathBuf::from)
            .ok_or(Error::VolumeOffline)
    }

    pub fn thumbnail(&self, volume_id: VolumeId, file_id: i64) -> Result<Option<Vec<u8>>> {
        let p = self.dirs.thumbs_db(volume_id);
        if !p.exists() {
            return Ok(None);
        }
        ThumbDb::open_ro(&p)?.get(file_id)
    }

    // ----- duplicates -------------------------------------------------------------------

    /// The most common extensions among catalogued files of a kind, across every drive.
    pub fn extension_counts(&self, kind: Kind, limit: usize) -> Result<Vec<ExtCount>> {
        let mut totals: HashMap<String, u64> = HashMap::new();
        for v in self.volumes()? {
            if !self.dirs.volume_db(v.id).exists() || self.is_scanning(v.id) {
                continue;
            }
            let db = match VolumeDb::open_rw(&self.dirs.volume_db(v.id)) {
                Ok(db) => db,
                Err(_) => self.volume_db(v.id)?,
            };
            for (k, ext, n) in db.extension_counts()? {
                if k == kind as u8 {
                    *totals.entry(ext).or_default() += n;
                }
            }
        }
        let mut out: Vec<ExtCount> = totals
            .into_iter()
            .map(|(ext, files)| ExtCount { ext, files })
            .collect();
        out.sort_by(|a, b| b.files.cmp(&a.files).then(a.ext.cmp(&b.ext)));
        out.truncate(limit);
        Ok(out)
    }

    /// Totals for your duplicates (copies in app-managed folders left out).
    pub fn duplicate_summary(&self) -> Result<DupSummary> {
        Ok(self
            .duplicate_page(&DupQuery {
                scope: DupScope::Mine,
                sort: DupSort::Waste,
                desc: true,
                limit: 0,
                offset: 0,
                ..Default::default()
            })?
            .summary)
    }

    /// Duplicate groups in scope, sorted and paged, with totals over the whole scope.
    pub fn duplicate_page(&self, q: &DupQuery) -> Result<DupPage> {
        let raw = self
            .dupes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .all_groups(self.hash_min_bytes())?;
        let volumes = self.volumes()?;
        let mut dbs: HashMap<VolumeId, VolumeDb> = HashMap::new();
        let mut groups = Vec::with_capacity(raw.len());
        for g in raw {
            if let Some(group) =
                self.resolve_group(&g.hash, g.size, &g.members, &volumes, &mut dbs)?
            {
                if (q.scope == DupScope::All || group.user_copies >= 2) && q.keeps(&group) {
                    groups.push(group);
                }
            }
        }
        let summary = DupSummary {
            groups: groups.len() as u64,
            files: groups
                .iter()
                .map(|g| match q.scope {
                    DupScope::Mine => g.user_copies,
                    DupScope::All => g.count,
                })
                .sum(),
            waste: groups.iter().map(|g| g.waste).sum(),
        };
        groups.sort_by(|a, b| {
            let o = match q.sort {
                DupSort::Waste => a.waste.cmp(&b.waste).then(a.size.cmp(&b.size)),
                DupSort::Size => a.size.cmp(&b.size).then(a.waste.cmp(&b.waste)),
                DupSort::Count => a.count.cmp(&b.count).then(a.waste.cmp(&b.waste)),
                DupSort::Name => a
                    .name
                    .to_lowercase()
                    .cmp(&b.name.to_lowercase())
                    .then(b.waste.cmp(&a.waste)),
            };
            if q.desc {
                o.reverse()
            } else {
                o
            }
        });
        let groups = groups.into_iter().skip(q.offset).take(q.limit).collect();
        Ok(DupPage { groups, summary })
    }

    pub fn duplicates(&self, limit: usize, offset: usize) -> Result<Vec<DupGroup>> {
        let raw = self
            .dupes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .groups(self.hash_min_bytes(), limit, offset)?;
        let volumes = self.volumes()?;
        let mut dbs: HashMap<VolumeId, VolumeDb> = HashMap::new();
        let mut out = Vec::with_capacity(raw.len());
        for g in raw {
            if let Some(group) =
                self.resolve_group(&g.hash, g.size, &g.members, &volumes, &mut dbs)?
            {
                out.push(group);
            }
        }
        Ok(out)
    }

    pub(crate) fn duplicate_group(
        &self,
        hash: &[u8],
        volumes: Option<&[Volume]>,
    ) -> Result<Option<DupGroup>> {
        let members = self
            .dupes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .members(hash)?;
        if members.is_empty() {
            return Ok(None);
        }
        let owned;
        let volumes = match volumes {
            Some(v) => v,
            None => {
                owned = self.volumes()?;
                &owned
            }
        };
        let mut dbs = HashMap::new();
        let size = {
            let (vid, fid) = members[0];
            self.db_for(vid, &mut dbs)?
                .and_then(|db| db.file(fid).ok().flatten())
                .map(|f| f.size)
                .unwrap_or(0)
        };
        self.resolve_group(hash, size, &members, volumes, &mut dbs)
    }

    fn db_for<'a>(
        &self,
        vid: VolumeId,
        dbs: &'a mut HashMap<VolumeId, VolumeDb>,
    ) -> Result<Option<&'a VolumeDb>> {
        if let std::collections::hash_map::Entry::Vacant(e) = dbs.entry(vid) {
            if !self.dirs.volume_db(vid).exists() {
                return Ok(None);
            }
            e.insert(self.volume_db(vid)?);
        }
        Ok(dbs.get(&vid))
    }

    fn resolve_group(
        &self,
        hash: &[u8],
        size: u64,
        members: &[(VolumeId, i64)],
        volumes: &[Volume],
        dbs: &mut HashMap<VolumeId, VolumeDb>,
    ) -> Result<Option<DupGroup>> {
        let mut out: Vec<DupMember> = Vec::with_capacity(members.len());
        let mut kind = Kind::Other;
        let mut name = String::new();
        for (vid, fid) in members {
            let Some(db) = self.db_for(*vid, dbs)? else {
                continue;
            };
            let Some(f) = db.file(*fid)? else { continue };
            let (_, full) = db.hashes(*fid)?;
            let v = volumes.iter().find(|v| v.id == *vid);
            if name.is_empty() {
                name = f.name.clone();
                kind = f.kind;
            }
            out.push(DupMember {
                volume_id: *vid,
                volume_label: v.map(|v| v.label.clone()).unwrap_or_default(),
                volume_online: v.map(|v| v.online).unwrap_or(false),
                file_id: *fid,
                app_managed: crate::places::is_app_managed(&f.path),
                name: f.name,
                path: f.path,
                mtime: f.mtime,
                full_hash: full.map(|b| hex(&b)),
            });
        }
        if out.len() < 2 {
            return Ok(None);
        }
        let user_copies = out.iter().filter(|m| !m.app_managed).count() as u64;
        let suggested_keep = out
            .iter()
            .filter(|m| !m.app_managed)
            .min_by_key(|m| keep_rank(m))
            .map(|m| (m.volume_id, m.file_id));
        // Name the group after the copy that would be kept, not a "(1)" copy.
        if let Some(k) = suggested_keep {
            if let Some(m) = out.iter().find(|m| (m.volume_id, m.file_id) == k) {
                name = m.name.clone();
            }
        }
        let verified = out.iter().all(|m| m.full_hash.is_some())
            && out.windows(2).all(|w| w[0].full_hash == w[1].full_hash);
        let count = out.len() as u64;
        Ok(Some(DupGroup {
            hash: hex(hash),
            size,
            count,
            waste: size * user_copies.saturating_sub(1),
            user_copies,
            suggested_keep,
            kind,
            name,
            verified,
            members: out,
        }))
    }

    // ----- export / import -----------------------------------------------------------------

    /// Copy a volume's catalogue (and thumbnails) to `dest_dir`. Returns the catalogue path.
    pub fn export_volume(&self, id: VolumeId, dest_dir: &Path) -> Result<PathBuf> {
        let v = self.volume(id)?;
        let src = self.dirs.volume_db(id);
        if !src.exists() {
            return Err(Error::Other("volume has not been scanned yet".into()));
        }
        std::fs::create_dir_all(dest_dir)?;
        let safe: String = v
            .label
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        let dest = dest_dir.join(format!("storecat-{}-{}.db", safe, id));
        std::fs::copy(&src, &dest)?;
        let thumbs = self.dirs.thumbs_db(id);
        if thumbs.exists() {
            let _ = std::fs::copy(
                &thumbs,
                dest_dir.join(format!("storecat-{}-{}.thumbs.db", safe, id)),
            );
        }
        Ok(dest)
    }

    /// Import a catalogue file produced by `export_volume` or found in a drive's
    /// `.storecat` folder. Matches an existing volume by identity, else registers one.
    pub fn import_volume(&self, src: &Path) -> Result<Volume> {
        let db = VolumeDb::open(src)?;
        let identity = db
            .info("identity")?
            .ok_or_else(|| Error::Other("not a Storecat catalogue (no identity)".into()))?;
        let label = db.info("label")?.unwrap_or_else(|| "Imported".into());
        let source = VolumeSource::parse(&db.info("source")?.unwrap_or_default());
        let root = db.info("root")?.unwrap_or_default();
        let capacity: u64 = db
            .info("capacity")?
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let fs = db.info("fs")?.unwrap_or_default();
        let media = MediaType::parse(&db.info("media_type")?.unwrap_or_default());
        let (files, dirs) = db.counts()?;
        let bytes: i64 =
            db.conn()
                .query_row("SELECT COALESCE(SUM(size), 0) FROM files", [], |r| r.get(0))?;
        drop(db);

        let id = {
            let master = self.master();
            match master.find_by_identity(&identity)? {
                Some(v) => v.id,
                None => master.insert_volume(&NewVolume {
                    identity: &identity,
                    source,
                    label: &label,
                    fs: &fs,
                    capacity,
                    free: 0,
                    media_type: media,
                    mount_path: &root,
                })?,
            }
        };
        if self.is_scanning(id) || self.is_enriching(id) {
            return Err(Error::ScanInProgress);
        }
        let live = self.dirs.volume_db(id);
        let _ = std::fs::remove_file(&live);
        std::fs::copy(src, &live)?;
        // Thumbnails travel as a sibling file when present.
        let thumbs_src = PathBuf::from(format!(
            "{}.thumbs.db",
            src.to_string_lossy().trim_end_matches(".db")
        ));
        let thumbs_live = self.dirs.thumbs_db(id);
        let _ = std::fs::remove_file(&thumbs_live);
        if thumbs_src.exists() {
            let _ = std::fs::copy(&thumbs_src, &thumbs_live);
        }
        self.index_volume_from_db(id)?;
        self.index.commit()?;
        let stats = ScanStats {
            files,
            dirs,
            bytes: bytes as u64,
            ..Default::default()
        };
        let scan_id = self.master().begin_scan(id)?;
        self.master().finish_scan(scan_id, id, &stats)?;
        if let Ok(db) = VolumeDb::open(&live) {
            let _ = self.refresh_dupes_for(id, &db, self.hash_min_bytes());
        }
        self.volume(id)
    }

    pub fn index_doc_count(&self) -> u64 {
        self.index.num_docs()
    }

    pub fn setting(&self, key: &str) -> Result<Option<String>> {
        self.master().setting(key)
    }

    pub fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        self.master().set_setting(key, value)
    }

    pub fn ffmpeg_status(&self) -> FfmpegStatus {
        crate::tools::ffmpeg_status(&self.dirs)
    }

    /// Forget enrichment for a volume so the next run redoes it (hashes optionally kept).
    pub fn reset_enrichment(&self, id: VolumeId, keep_hashes: bool) -> Result<()> {
        if self.is_scanning(id) || self.is_enriching(id) {
            return Err(Error::ScanInProgress);
        }
        let p = self.dirs.volume_db(id);
        if p.exists() {
            VolumeDb::open_rw(&p)?.reset_enrichment(keep_hashes)?;
            let _ = std::fs::remove_file(self.dirs.thumbs_db(id));
            self.index_volume_from_db(id)?;
            self.index.commit()?;
        }
        Ok(())
    }
}

/// Which of your copies to keep: one on a connected drive, outside temporary, download
/// and recycle folders, without a "(1)" or "- Copy" in its name, then the shortest path.
fn keep_rank(m: &DupMember) -> (bool, u8, bool, usize, usize) {
    let segs: Vec<String> = m
        .path
        .split(['/', '\\'])
        .map(|s| s.to_ascii_lowercase())
        .collect();
    let has = |names: &[&str]| segs.iter().any(|s| names.contains(&s.as_str()));
    let place = if has(&["$recycle.bin", ".trash", ".trashes"]) {
        3
    } else if has(&["temp", "tmp"]) {
        2
    } else if has(&["downloads"]) {
        1
    } else {
        0
    };
    let lower = m.name.to_ascii_lowercase();
    let copy_name = lower.contains(" - copy")
        || lower.starts_with("copy of ")
        || (1..10).any(|n| lower.contains(&format!(" ({n})")));
    (
        !m.volume_online,
        place,
        copy_name,
        segs.len(),
        m.path.len() + m.name.len(),
    )
}

/// Build an absolute path from a mount point, a `/`-separated relative dir and a name.
pub fn join_abs(mount: &str, rel_dir: &str, name: &str) -> String {
    let mut p = PathBuf::from(mount);
    for part in rel_dir.split('/').filter(|s| !s.is_empty()) {
        p.push(part);
    }
    if !name.is_empty() {
        p.push(name);
    }
    p.to_string_lossy().into_owned()
}

fn normalise_folder(path: &Path) -> Result<String> {
    if !path.is_dir() {
        return Err(Error::Other(format!(
            "{} is not an accessible folder",
            path.display()
        )));
    }
    let canon = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let mut s = canon.to_string_lossy().into_owned();
    if let Some(rest) = s.strip_prefix("\\\\?\\UNC\\") {
        s = format!("\\\\{rest}");
    } else if let Some(rest) = s.strip_prefix("\\\\?\\") {
        s = rest.to_string();
    }
    while s.len() > 3 && (s.ends_with('\\') || s.ends_with('/')) {
        s.pop();
    }
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::enrich::EnrichOptions;

    fn two_thread_scan() -> ScanOptions {
        ScanOptions {
            threads: 2,
            ..Default::default()
        }
    }

    #[test]
    fn end_to_end_scan_and_search() {
        let data = tempfile::tempdir().unwrap();
        let tree = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tree.path().join("Clients/ACME")).unwrap();
        std::fs::write(
            tree.path().join("Clients/ACME/Invoice_2023.pdf"),
            vec![1u8; 2048],
        )
        .unwrap();
        std::fs::write(tree.path().join("Clients/notes.txt"), b"x").unwrap();
        std::fs::write(tree.path().join("holiday.MOV"), vec![0u8; 4096]).unwrap();

        let cat = Catalog::open(DataDirs::at(data.path())).unwrap();
        let v = cat.add_folder(tree.path(), Some("Test tree")).unwrap();
        assert!(v.online);
        let stats = cat.scan(v.id, &two_thread_scan(), &|_p| {}).unwrap();
        assert_eq!(stats.files, 3);
        assert_eq!(stats.dirs, 3);

        let r = cat.search("invoice", &SearchOptions::default()).unwrap();
        assert_eq!(r.total, 1);
        assert_eq!(r.hits[0].volume_label, "Test tree");
        let r = cat.search("acme", &SearchOptions::default()).unwrap();
        assert_eq!(r.total, 2);
        let r = cat.search("kind:video", &SearchOptions::default()).unwrap();
        assert_eq!(r.hits[0].name, "holiday.MOV");
        let r = cat
            .search("drive:nonexistent invoice", &SearchOptions::default())
            .unwrap();
        assert_eq!(r.total, 0);

        let hit = &cat
            .search("invoice", &SearchOptions::default())
            .unwrap()
            .hits[0];
        let d = cat
            .item_detail(hit.volume_id, hit.item_id, hit.item_type)
            .unwrap();
        assert_eq!(d.size, 2048);
        assert_eq!(d.path, "Clients/ACME");
        assert!(Path::new(d.absolute_path.as_ref().unwrap()).exists());

        std::fs::remove_file(tree.path().join("holiday.MOV")).unwrap();
        let stats = cat.scan(v.id, &two_thread_scan(), &|_p| {}).unwrap();
        assert_eq!(stats.files, 2);
        let r = cat.search("kind:video", &SearchOptions::default()).unwrap();
        assert_eq!(r.total, 0);

        cat.remove_volume(v.id).unwrap();
        assert_eq!(cat.volumes().unwrap().len(), 0);
        let r = cat.search("invoice", &SearchOptions::default()).unwrap();
        assert_eq!(r.total, 0);
    }

    /// Files enriched before Storecat read embedded artwork get just the thumbnail later,
    /// keeping the metadata and text they already had in search.
    #[test]
    fn older_enrichment_gets_artwork_without_losing_text() {
        let data = tempfile::tempdir().unwrap();
        let tree = tempfile::tempdir().unwrap();
        let mut png = Vec::new();
        image::RgbImage::from_pixel(320, 240, image::Rgb([200, 40, 40]))
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        {
            let f = std::fs::File::create(tree.path().join("deck.pptx")).unwrap();
            let mut z = zip::ZipWriter::new(f);
            let o = zip::write::SimpleFileOptions::default();
            z.start_file("[Content_Types].xml", o).unwrap();
            std::io::Write::write_all(&mut z, b"<Types/>").unwrap();
            z.start_file("ppt/slides/slide1.xml", o).unwrap();
            std::io::Write::write_all(
                &mut z,
                b"<p:sld><a:t>quarterly roadmap for the zeppelin launch</a:t></p:sld>",
            )
            .unwrap();
            z.start_file("docProps/thumbnail.jpeg", o).unwrap();
            std::io::Write::write_all(&mut z, &png).unwrap();
            z.finish().unwrap();
        }
        let cat = Catalog::open(DataDirs::at(data.path())).unwrap();
        let v = cat.add_folder(tree.path(), Some("Old")).unwrap();
        cat.scan(v.id, &two_thread_scan(), &|_| {}).unwrap();
        // An "older version": everything but thumbnails.
        let old = EnrichOptions {
            threads: 2,
            thumbs: false,
            ..Default::default()
        };
        cat.enrich(v.id, &old, &|_| {}).unwrap();
        let hit = |q: &str| cat.search(q, &SearchOptions::default()).unwrap().hits;
        let id = hit("zeppelin")[0].item_id;
        assert!(cat.thumbnail(v.id, id).unwrap().is_none());
        assert_eq!(
            cat.upgrade_enrichment(v.id).unwrap(),
            0,
            "already this format"
        );

        // Pretend the catalogue predates artwork support.
        VolumeDb::open_rw(&cat.dirs.volume_db(v.id))
            .unwrap()
            .set_info("enrich_format", "1")
            .unwrap();
        assert_eq!(cat.upgrade_enrichment(v.id).unwrap(), 1);
        let cov = cat.enrich_coverage(v.id).unwrap();
        assert!(cov.done < cov.files, "the drive shows work to do");

        let opts = EnrichOptions {
            threads: 2,
            ..Default::default()
        };
        let stats = cat.enrich(v.id, &opts, &|_| {}).unwrap();
        assert_eq!(stats.thumbs, 1);
        assert_eq!(stats.hashed, 0, "nothing else is redone");
        assert!(cat.thumbnail(v.id, id).unwrap().is_some());
        assert_eq!(hit("zeppelin").len(), 1, "document text still searchable");
        let cov = cat.enrich_coverage(v.id).unwrap();
        assert_eq!(cov.done, cov.files);
    }

    #[test]
    fn enrichment_text_thumbs_dupes_and_carry_over() {
        let data = tempfile::tempdir().unwrap();
        let tree = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tree.path().join("docs")).unwrap();
        std::fs::write(
            tree.path().join("docs/report.txt"),
            "Quarterly report. The unpaid balance is carried forward.\n",
        )
        .unwrap();
        let img = image::RgbImage::from_fn(640, 480, |x, y| {
            image::Rgb([(x % 255) as u8, (y % 255) as u8, 90])
        });
        img.save(tree.path().join("photo.png")).unwrap();
        // Two identical big files + one different, above the 1 MiB hash threshold.
        let blob: Vec<u8> = (0..2_000_000u32).map(|i| (i % 251) as u8).collect();
        std::fs::write(tree.path().join("clip_a.bin"), &blob).unwrap();
        std::fs::write(tree.path().join("docs/clip_copy.bin"), &blob).unwrap();
        let mut other = blob.clone();
        other[1000] ^= 0xff;
        std::fs::write(tree.path().join("other.bin"), &other).unwrap();
        // An archive.
        {
            let f = std::fs::File::create(tree.path().join("bundle.zip")).unwrap();
            let mut z = zip::ZipWriter::new(f);
            let o = zip::write::SimpleFileOptions::default();
            z.start_file("inner/secret_recipe.txt", o).unwrap();
            std::io::Write::write_all(&mut z, b"salt").unwrap();
            z.finish().unwrap();
        }

        let cat = Catalog::open(DataDirs::at(data.path())).unwrap();
        let v = cat.add_folder(tree.path(), Some("Enrich")).unwrap();
        cat.scan(v.id, &two_thread_scan(), &|_| {}).unwrap();
        let opts = EnrichOptions {
            threads: 2,
            ..Default::default()
        };
        let stats = cat.enrich(v.id, &opts, &|_| {}).unwrap();
        assert_eq!(stats.processed, 6);
        assert!(stats.texts >= 1, "text extracted");
        assert_eq!(stats.thumbs, 1, "png thumbnail");
        assert_eq!(stats.hashed, 3, "three files over 1 MiB");
        assert_eq!(stats.archives, 1);

        // Content search and flags.
        let r = cat.search("unpaid", &SearchOptions::default()).unwrap();
        assert_eq!(r.hits[0].name, "report.txt");
        let r = cat.search("has:thumb", &SearchOptions::default()).unwrap();
        assert_eq!(r.hits[0].name, "photo.png");
        assert!(cat.thumbnail(v.id, r.hits[0].item_id).unwrap().is_some());
        let r = cat.search("recipe", &SearchOptions::default()).unwrap();
        assert_eq!(r.hits[0].item_type, ItemType::ArchiveEntry);
        let d = cat
            .item_detail(v.id, r.hits[0].item_id, ItemType::ArchiveEntry)
            .unwrap();
        assert_eq!(d.archive.as_ref().unwrap().name, "bundle.zip");
        assert_eq!(d.path, "bundle.zip/inner");

        // Duplicates.
        let groups = cat.duplicates(10, 0).unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].count, 2);
        assert!(!groups[0].verified);
        let g = cat.verify_group(&groups[0].hash).unwrap();
        assert!(g.verified);
        assert_eq!(cat.duplicate_summary().unwrap().waste, 2_000_000);

        // Rescan keeps enrichment for unchanged files and re-enriches nothing.
        std::fs::write(tree.path().join("new.txt"), "brand new words here").unwrap();
        cat.scan(v.id, &two_thread_scan(), &|_| {}).unwrap();
        let r = cat.search("unpaid", &SearchOptions::default()).unwrap();
        assert_eq!(r.total, 1, "text survives a rescan");
        let r = cat.search("recipe", &SearchOptions::default()).unwrap();
        assert_eq!(r.total, 1, "archive entries survive a rescan");
        let cov = cat.enrich_coverage(v.id).unwrap();
        assert_eq!(cov.done, 6);
        let stats = cat.enrich(v.id, &opts, &|_| {}).unwrap();
        assert_eq!(stats.processed, 1, "only the new file");
        let r = cat.search("brand new", &SearchOptions::default()).unwrap();
        assert_eq!(r.hits[0].name, "new.txt");
        let groups = cat.duplicates(10, 0).unwrap();
        assert_eq!(groups.len(), 1);
        assert!(groups[0].verified, "full hashes carried over");

        // Export, remove, import round-trip.
        let out = tempfile::tempdir().unwrap();
        let exported = cat.export_volume(v.id, out.path()).unwrap();
        cat.remove_volume(v.id).unwrap();
        assert_eq!(
            cat.search("unpaid", &SearchOptions::default())
                .unwrap()
                .total,
            0
        );
        let imported = cat.import_volume(&exported).unwrap();
        assert_eq!(imported.label, "Enrich");
        assert_eq!(imported.file_count, 7);
        assert_eq!(
            cat.search("unpaid", &SearchOptions::default())
                .unwrap()
                .total,
            1
        );
        assert!(cat.thumbnail(imported.id, 0).is_ok());
    }

    /// Hard-drive mode reads one file at a time in disk order, yet finds the same things;
    /// package folders keep names and hashes but get no text or thumbnails.
    #[test]
    fn os_leftovers_are_hidden_from_search() {
        let data = tempfile::tempdir().unwrap();
        let tree = tempfile::tempdir().unwrap();
        std::fs::write(tree.path().join("song.mp3"), b"music").unwrap();
        std::fs::write(tree.path().join("._song.mp3"), b"\x00\x05\x16\x07mac").unwrap();
        std::fs::write(tree.path().join(".DS_Store"), b"finder").unwrap();
        let cat = Catalog::open(DataDirs::at(data.path())).unwrap();
        let v = cat.add_folder(tree.path(), Some("Mac stick")).unwrap();
        cat.scan(v.id, &two_thread_scan(), &|_| {}).unwrap();
        let names = |q: &str| -> Vec<String> {
            let mut n: Vec<String> = cat
                .search(q, &SearchOptions::default())
                .unwrap()
                .hits
                .into_iter()
                .map(|h| h.name)
                .collect();
            n.sort();
            n
        };
        assert_eq!(names("song"), vec!["song.mp3"]);
        assert_eq!(names("ext:mp3"), vec!["song.mp3"]);
        assert_eq!(names("is:leftover"), vec![".DS_Store", "._song.mp3"]);
        assert_eq!(names("song is:leftover"), vec!["._song.mp3"]);
    }

    /// Zips made on a Mac carry a `__MACOSX` folder with an AppleDouble copy of every file.
    #[test]
    fn mac_zip_leftovers_are_hidden_from_search() {
        let data = tempfile::tempdir().unwrap();
        let tree = tempfile::tempdir().unwrap();
        {
            let f = std::fs::File::create(tree.path().join("tracks.zip")).unwrap();
            let mut z = zip::ZipWriter::new(f);
            let o = zip::write::SimpleFileOptions::default();
            // Finder writes the folder entries too.
            z.add_directory("__MACOSX/", o).unwrap();
            for (name, body) in [
                ("Album/anthem.mp3", &b"music"[..]),
                ("Album/._anthem.mp3", &b"\x00\x05\x16\x07mac"[..]),
                ("__MACOSX/Album/._anthem.mp3", &b"\x00\x05\x16\x07mac"[..]),
            ] {
                z.start_file(name, o).unwrap();
                std::io::Write::write_all(&mut z, body).unwrap();
            }
            z.finish().unwrap();
        }
        let cat = Catalog::open(DataDirs::at(data.path())).unwrap();
        let v = cat.add_folder(tree.path(), Some("Mac zips")).unwrap();
        cat.scan(v.id, &two_thread_scan(), &|_| {}).unwrap();
        let opts = EnrichOptions {
            threads: 2,
            ..Default::default()
        };
        cat.enrich(v.id, &opts, &|_| {}).unwrap();
        let paths = |q: &str| -> Vec<String> {
            let mut n: Vec<String> = cat
                .search(q, &SearchOptions::default())
                .unwrap()
                .hits
                .into_iter()
                .map(|h| format!("{}/{}", h.path.replace('\\', "/"), h.name))
                .collect();
            n.sort();
            n
        };
        assert_eq!(paths("anthem"), vec!["tracks.zip/Album/anthem.mp3"]);
        assert_eq!(paths("ext:mp3"), vec!["tracks.zip/Album/anthem.mp3"]);
        assert!(paths("macosx").is_empty());
        let hidden = paths("is:leftover");
        assert_eq!(hidden.len(), 3, "{hidden:?}");
        assert!(hidden.iter().any(|p| p.ends_with("/__MACOSX")));
    }

    #[test]
    fn index_catches_up_after_an_interrupted_run() {
        let data = tempfile::tempdir().unwrap();
        let tree = tempfile::tempdir().unwrap();
        std::fs::write(
            tree.path().join("plan.md"),
            "the zebra crossing plan
",
        )
        .unwrap();
        let cat = Catalog::open(DataDirs::at(data.path())).unwrap();
        let v = cat.add_folder(tree.path(), Some("Cut")).unwrap();
        cat.scan(v.id, &two_thread_scan(), &|_| {}).unwrap();
        cat.enrich(v.id, &EnrichOptions::default(), &|_| {})
            .unwrap();
        let hits = |q: &str| cat.search(q, &SearchOptions::default()).unwrap().total;
        assert_eq!(hits("zebra"), 1);

        // The power goes out after rows were saved but before the index committed them.
        cat.index.delete_volume(v.id).unwrap();
        cat.index.commit().unwrap();
        VolumeDb::open_rw(&cat.dirs.volume_db(v.id))
            .unwrap()
            .set_info(crate::enrich::INDEX_SYNCED_AT, "0")
            .unwrap();
        assert_eq!(hits("zebra"), 0);

        assert_eq!(cat.repair_index(v.id).unwrap(), 1);
        assert_eq!(hits("zebra"), 1);
    }

    #[test]
    fn spinning_mode_and_package_folders() {
        use crate::enrich::DiskMode;
        let data = tempfile::tempdir().unwrap();
        let tree = tempfile::tempdir().unwrap();
        let t = tree.path();
        std::fs::create_dir_all(t.join("notes")).unwrap();
        std::fs::create_dir_all(t.join("app/.venv/Lib/site-packages/lib")).unwrap();
        std::fs::write(t.join("notes/plan.md"), "the zebra crossing plan\n").unwrap();
        std::fs::write(
            t.join("app/.venv/Lib/site-packages/lib/index.js"),
            "const needle = 'giraffe';\n",
        )
        .unwrap();
        std::fs::write(t.join("app/.env"), "API_KEY=hunter2\n").unwrap();
        let img = image::RgbImage::from_fn(300, 200, |x, _| image::Rgb([(x % 255) as u8, 9, 9]));
        img.save(t.join("notes/shot.png")).unwrap();
        img.save(t.join("app/.venv/Lib/site-packages/lib/icon.png"))
            .unwrap();
        let blob: Vec<u8> = (0..1_500_000u32).map(|i| (i % 249) as u8).collect();
        std::fs::write(t.join("a.bin"), &blob).unwrap();
        std::fs::write(t.join("app/.venv/Lib/site-packages/lib/b.bin"), &blob).unwrap();

        let cat = Catalog::open(DataDirs::at(data.path())).unwrap();
        let v = cat.add_folder(t, Some("Spin")).unwrap();
        cat.scan(v.id, &two_thread_scan(), &|_| {}).unwrap();
        let passes = std::sync::Mutex::new(Vec::<u8>::new());
        let opts = EnrichOptions {
            threads: 3,
            disk: DiskMode::Spinning,
            ..Default::default()
        };
        let stats = cat
            .enrich(v.id, &opts, &|p| {
                let mut seen = passes.lock().unwrap();
                if seen.last() != Some(&p.pass) {
                    seen.push(p.pass);
                }
            })
            .unwrap();
        assert!(!stats.paused);
        assert_eq!(stats.processed, 7, "every file finished once");
        assert_eq!(stats.texts, 1, "only notes/plan.md");
        assert_eq!(stats.thumbs, 1, "png without a preview is read in full");
        assert_eq!(stats.hashed, 2, "package files are still hashed");
        let seen = passes.into_inner().unwrap();
        assert!(seen.starts_with(&[0, 1]) && seen.contains(&2), "{seen:?}");

        let cov = cat.enrich_coverage(v.id).unwrap();
        assert_eq!(cov.done, cov.files);
        let vdb = crate::db::volume::VolumeDb::open(&cat.dirs.volume_db(v.id)).unwrap();
        assert!(
            vdb.disk_positions().unwrap().is_empty(),
            "positions are forgotten once everything is read"
        );
        let r = cat.search("zebra", &SearchOptions::default()).unwrap();
        assert_eq!(r.total, 1);
        assert_eq!(
            cat.search("giraffe", &SearchOptions::default())
                .unwrap()
                .total,
            0
        );
        assert_eq!(
            cat.search("hunter2", &SearchOptions::default())
                .unwrap()
                .total,
            0
        );
        assert_eq!(
            cat.search("index.js", &SearchOptions::default())
                .unwrap()
                .total,
            1
        );
        assert_eq!(cat.duplicates(10, 0).unwrap().len(), 1);

        // Nothing left: a second run settles immediately.
        let again = cat.enrich(v.id, &opts, &|_| {}).unwrap();
        assert_eq!(again.processed, 0);
    }
}
