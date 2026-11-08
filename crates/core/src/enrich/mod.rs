//! The enrichment pass: hashes, metadata, thumbnails, text and archive listings for files
//! the walk pass already catalogued. Runs in the background while a drive is connected,
//! commits as it goes, and resumes where it left off.
//!
//! A run plans once ([`plan`]), then streams files through a pool of workers ([`work`])
//! in two passes: quick (hashes, metadata, photo thumbnails, archive listings) and detail
//! (document text, PDF details, video thumbnails). On spinning disks files are read in
//! the order they sit on the platter, one at a time, while decoding runs in parallel.

mod diskpos;
pub mod plan;
mod work;

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::catalog::{join_abs, Catalog};
use crate::db::volume::{
    FileRecord, ThumbDb, VolumeDb, ENRICH_DETAIL_PENDING, ENRICH_DONE, STAGE_ARCHIVE, STAGE_HASH,
    STAGE_META, STAGE_SKIPPED_PLACEHOLDER, STAGE_TEXT, STAGE_THUMB,
};
use crate::error::{Error, Result};
use crate::index::{doc_key, DocInput};
use crate::model::*;
use crate::scan::is_placeholder;
use crate::tools::find_ffmpeg;

/// How to treat the drive being enriched.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DiskMode {
    /// From the drive's profile: spinning disks and optical discs read one file at a time.
    #[default]
    Auto,
    /// Read many files at once, in the most useful order.
    Solid,
    /// Read one file at a time in on-disk order; thumbnail photos from their previews.
    Spinning,
}

#[derive(Debug, Clone)]
pub struct EnrichOptions {
    /// Files smaller than this are not hashed (duplicates of tiny files don't matter).
    pub hash_min_bytes: u64,
    pub hash: bool,
    pub meta: bool,
    pub thumbs: bool,
    pub text: bool,
    pub archives: bool,
    /// Worker threads; 0 = pick from the CPU and the drive type.
    pub threads: usize,
    pub text_max_bytes: usize,
    pub thumb_edge: u32,
    /// Rows per database transaction.
    pub batch: usize,
    pub disk: DiskMode,
    /// Skip text and thumbnails inside package, cache and system folders.
    pub skip_bulk_folders: bool,
}

impl Default for EnrichOptions {
    fn default() -> Self {
        Self {
            hash_min_bytes: 1024 * 1024,
            hash: true,
            meta: true,
            thumbs: true,
            text: true,
            archives: true,
            threads: 0,
            text_max_bytes: storecat_extract::text::DEFAULT_MAX_TEXT,
            thumb_edge: storecat_extract::thumbs::DEFAULT_MAX_EDGE,
            batch: 400,
            disk: DiskMode::Auto,
            skip_bulk_folders: true,
        }
    }
}

const HEAD_TAIL: usize = 128 * 1024;

/// Quick hash: BLAKE3 over the size plus the first and last 128 KB (whole file if small).
pub fn quick_hash(path: &Path, size: u64) -> std::io::Result<[u8; 32]> {
    let mut f = File::open(path)?;
    let mut h = blake3::Hasher::new();
    h.update(&size.to_le_bytes());
    if size <= (2 * HEAD_TAIL) as u64 {
        let mut buf = Vec::with_capacity(size as usize);
        f.read_to_end(&mut buf)?;
        h.update(&buf);
    } else {
        let mut buf = vec![0u8; HEAD_TAIL];
        f.read_exact(&mut buf)?;
        h.update(&buf);
        f.seek(SeekFrom::End(-(HEAD_TAIL as i64)))?;
        f.read_exact(&mut buf)?;
        h.update(&buf);
    }
    Ok(*h.finalize().as_bytes())
}

/// Full-content BLAKE3, streamed.
pub fn full_hash(path: &Path) -> std::io::Result<[u8; 32]> {
    full_hash_with(path, &AtomicBool::new(false), &mut |_| {})
}

/// [`full_hash`], reporting bytes read and stopping when `cancel` is set.
pub fn full_hash_with(
    path: &Path,
    cancel: &AtomicBool,
    on_bytes: &mut dyn FnMut(u64),
) -> std::io::Result<[u8; 32]> {
    let mut f = File::open(path)?;
    let mut h = blake3::Hasher::new();
    let mut buf = vec![0u8; 1024 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
        on_bytes(n as u64);
        if cancel.load(Ordering::Relaxed) {
            return Err(std::io::Error::other("cancelled"));
        }
    }
    Ok(*h.finalize().as_bytes())
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Catalogue path of a file: `dir/path/name`.
pub fn item_path(f: &FileRecord) -> String {
    if f.path.is_empty() {
        f.name.clone()
    } else {
        format!("{}/{}", f.path, f.name)
    }
}

/// Index flags implied by stage bits.
pub fn flags_for(stages: u32, attrs: u32) -> u64 {
    let mut fl = 0;
    if stages & STAGE_THUMB != 0 {
        fl |= flags::HAS_THUMB;
    }
    if stages & STAGE_TEXT != 0 {
        fl |= flags::HAS_TEXT;
    }
    if stages & STAGE_META != 0 {
        fl |= flags::HAS_META;
    }
    if stages & STAGE_HASH != 0 {
        fl |= flags::HASHED;
    }
    if is_placeholder(attrs) {
        fl |= flags::PLACEHOLDER;
    }
    fl
}

/// Volume info key: unix time at which the search index last held every enrichment row
/// saved before it. Rows saved since then are re-indexed by `Catalog::repair_index`.
pub(crate) const INDEX_SYNCED_AT: &str = "index_synced_at";

pub(crate) fn mark_index_synced(db: &VolumeDb) -> Result<()> {
    db.set_info(INDEX_SYNCED_AT, &now_unix().to_string())
}

impl Catalog {
    pub fn is_enriching(&self, id: VolumeId) -> bool {
        self.enriching
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains_key(&id)
    }

    pub fn enriching_ids(&self) -> Vec<VolumeId> {
        self.enriching
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .keys()
            .copied()
            .collect()
    }

    /// Ask a running enrichment to stop; files already read are kept.
    pub fn cancel_enrich(&self, id: VolumeId) {
        if let Some(flag) = self
            .enriching
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&id)
        {
            flag.store(true, Ordering::Relaxed);
        }
    }

    pub fn enrich_coverage(&self, id: VolumeId) -> Result<EnrichCoverage> {
        if !self.dirs.volume_db(id).exists() {
            return Ok(EnrichCoverage::default());
        }
        self.volume_db(id)?.coverage()
    }

    /// Queue what an older version of Storecat skipped (embedded artwork) so the next
    /// enrichment fills it in. Cheap once done: a single info lookup. Returns the number
    /// of files queued.
    pub fn upgrade_enrichment(&self, id: VolumeId) -> Result<usize> {
        if self.is_scanning(id) || !self.dirs.volume_db(id).exists() {
            return Ok(0);
        }
        if self
            .enriching
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains_key(&id)
        {
            return Ok(0);
        }
        VolumeDb::open_rw(&self.dirs.volume_db(id))?.upgrade_enrichment()
    }

    /// Enrich every file of a volume that has not been processed yet. Blocking: run on a
    /// worker thread. Stops cleanly (and resumes next time) if the drive disappears or
    /// `cancel_enrich` is called.
    pub fn enrich(
        &self,
        id: VolumeId,
        opts: &EnrichOptions,
        progress: &(dyn Fn(EnrichProgress) + Sync),
    ) -> Result<EnrichStats> {
        let volume = self.volume(id)?;
        let root = volume.mount_path.clone().ok_or(Error::VolumeOffline)?;
        let cancel = Arc::new(AtomicBool::new(false));
        {
            let mut map = self.enriching.lock().unwrap_or_else(|e| e.into_inner());
            if map.contains_key(&id) || self.is_scanning(id) {
                return Err(Error::ScanInProgress);
            }
            map.insert(id, Arc::clone(&cancel));
        }
        let run_started = now_unix();
        let result = self.enrich_inner(id, &volume, &root, opts, &cancel, progress);
        self.enriching
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&id);
        // Read trouble found during this run, by the run itself or by the OS's error log.
        let fault = match &result {
            Ok(s) if s.paused => s.fault.clone().or_else(|| {
                self.volume(id)
                    .ok()
                    .filter(|v| v.fault_at.is_some_and(|at| at >= run_started))
                    .and_then(|v| v.fault)
            }),
            _ => None,
        };
        match &result {
            Ok(stats) => progress(EnrichProgress {
                volume_id: id,
                phase: match fault {
                    Some(message) => EnrichPhase::Stopped { message },
                    None if stats.paused => EnrichPhase::Paused,
                    None => EnrichPhase::Done,
                },
                pass: 2,
                done: stats.processed,
                total: stats.processed,
                hashed: stats.hashed,
                metas: stats.metas,
                thumbs: stats.thumbs,
                texts: stats.texts,
                archives: stats.archives,
                elapsed_ms: stats.elapsed_ms,
                current: String::new(),
            }),
            Err(e) => progress(EnrichProgress {
                volume_id: id,
                phase: EnrichPhase::Failed {
                    message: e.to_string(),
                },
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
            }),
        }
        result
    }

    fn enrich_inner(
        &self,
        id: VolumeId,
        volume: &Volume,
        root: &str,
        opts: &EnrichOptions,
        cancel: &AtomicBool,
        progress: &(dyn Fn(EnrichProgress) + Sync),
    ) -> Result<EnrichStats> {
        let started = Instant::now();
        let db_path = self.dirs.volume_db(id);
        if !db_path.exists() {
            return Err(Error::Other("volume has not been scanned yet".into()));
        }
        self.repair_index(id)?;
        let writer = VolumeDb::open_rw(&db_path)?;
        let upgraded = writer.upgrade_enrichment()?;
        if upgraded > 0 {
            log::info!(
                "{}: {upgraded} files get the artwork older versions skipped",
                volume.label
            );
        }
        mark_index_synced(&writer)?;
        let thumbs = ThumbDb::create(&self.dirs.thumbs_db(id))?;
        let ffmpeg = find_ffmpeg(&self.dirs);
        let spinning = match opts.disk {
            DiskMode::Solid => false,
            DiskMode::Spinning => true,
            DiskMode::Auto => matches!(volume.media_type, MediaType::Hdd | MediaType::Optical),
        };
        let threads = if opts.threads > 0 {
            opts.threads
        } else if matches!(volume.media_type, MediaType::Network | MediaType::Cloud) {
            4
        } else {
            // Spinning disks too: reads take turns, decoding uses every core.
            std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(4)
                .clamp(2, 8)
        };
        let bulk = if opts.skip_bulk_folders {
            plan::bulk_dirs(&writer, crate::paths::is_system_drive(root))?
        } else {
            Default::default()
        };
        let turn = work::Turn::new();
        let ctx = work::Ctx {
            root,
            opts,
            ffmpeg: ffmpeg.as_ref(),
            spinning,
            turn: spinning.then_some(&turn),
        };
        let mut run = PassRun {
            id,
            root,
            opts,
            threads,
            db_path: &db_path,
            writer: &writer,
            thumbs: &thumbs,
            tag_map: self.master().tag_map(id)?,
            cancel,
            progress,
            started,
            stats: EnrichStats::default(),
            // A slow network share is just slow; only local drives are judged by read times.
            watch_faults: !matches!(volume.media_type, MediaType::Network | MediaType::Cloud),
            io_faults: 0,
            slow_reads: 0,
        };
        log::info!(
            "enriching volume {id}: {threads} threads, {} disk",
            if spinning { "spinning" } else { "solid-state" }
        );

        // Disk positions found by earlier runs: resuming needn't look them all up again.
        let mut known = if spinning {
            writer.disk_positions().unwrap_or_default()
        } else {
            Default::default()
        };
        let mut finished = true;
        for pass in [1u8, 2] {
            let (mut jobs, settled) = plan::build(&writer, pass, opts, ffmpeg.is_some(), &bulk)?;
            run.settle(&settled)?;
            // Settled files need no index change, so the index is in step here.
            mark_index_synced(&writer)?;
            if spinning && !jobs.is_empty() {
                let total = jobs.len() as u64;
                run.emit(0, 0, total, "");
                let mut path_of = |j: &plan::Job| {
                    writer
                        .file(j.id)
                        .ok()
                        .flatten()
                        .map(|f| join_abs(root, &f.path, &f.name))
                };
                let mut tick = |i: usize| run.emit(0, i as u64, total, "");
                let mut found = Vec::new();
                plan::order_for_hdd(
                    &mut jobs,
                    &known,
                    &mut found,
                    &mut path_of,
                    &|| cancel.load(Ordering::Relaxed),
                    &mut tick,
                    Duration::from_millis(4),
                );
                if let Err(e) = writer.save_disk_positions(&found) {
                    log::warn!("volume {id}: could not remember disk positions: {e}");
                }
                known.extend(found);
            } else {
                plan::order_for_ssd(&mut jobs, pass);
            }
            let paused = run.pass(self, pass, &jobs, &ctx)?;
            self.index.commit()?;
            mark_index_synced(&writer)?;
            if pass == 1 {
                // Hashes are all quick-pass work: duplicates are ready now.
                self.refresh_dupes_for(id, &writer, opts.hash_min_bytes)?;
            }
            if paused || cancel.load(Ordering::Relaxed) {
                run.stats.paused = true;
                finished = false;
                break;
            }
        }
        if finished && spinning {
            // Everything is read: the positions would only go stale.
            let _ = writer.clear_disk_positions();
        }
        run.stats.elapsed_ms = started.elapsed().as_millis() as u64;
        Ok(run.stats)
    }

    /// Rebuild a file's index document (and its archive-entry documents) from enrichment.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn index_enriched_file(
        &self,
        volume_id: VolumeId,
        f: &FileRecord,
        meta_json: Option<&str>,
        text: Option<&str>,
        stages: u32,
        entries: &[(i64, storecat_extract::ArchiveEntry)],
        tags: &[String],
    ) -> Result<()> {
        let meta_text = meta_json
            .and_then(|j| serde_json::from_str::<storecat_extract::FileMeta>(j).ok())
            .map(|m| m.index_text())
            .filter(|s| !s.is_empty());
        let key = doc_key(volume_id, ItemType::File, f.id);
        self.index.replace_doc(&DocInput {
            volume_id,
            item_id: f.id,
            item_type: ItemType::File,
            name: &f.name,
            path: &f.path,
            ext: &f.ext,
            kind: f.kind,
            size: f.size,
            mtime: f.mtime,
            flags: flags_for(stages, f.attrs),
            parent_key: 0,
            content: text,
            meta_text: meta_text.as_deref(),
            tags,
        })?;
        if stages & STAGE_ARCHIVE != 0 {
            self.index.delete_children(key)?;
            let archive_path = if f.path.is_empty() {
                f.name.clone()
            } else {
                format!("{}/{}", f.path, f.name)
            };
            for (entry_id, e) in entries {
                let (dir, base) = match e.path.rfind('/') {
                    Some(i) => (&e.path[..i], &e.path[i + 1..]),
                    None => ("", e.path.as_str()),
                };
                let path = if dir.is_empty() {
                    archive_path.clone()
                } else {
                    format!("{archive_path}/{dir}")
                };
                let ext = crate::kind::split_ext(base);
                let kind = if e.is_dir {
                    Kind::Dir
                } else {
                    crate::kind::kind_for_file(&ext, e.size)
                };
                self.index.add_doc(&DocInput {
                    volume_id,
                    item_id: *entry_id,
                    item_type: ItemType::ArchiveEntry,
                    name: base,
                    path: &path,
                    ext: &ext,
                    kind,
                    size: e.size,
                    mtime: e.mtime,
                    flags: 0,
                    parent_key: key,
                    content: None,
                    meta_text: None,
                    tags: &[],
                })?;
            }
        }
        Ok(())
    }

    pub(crate) fn refresh_dupes_for(
        &self,
        id: VolumeId,
        db: &VolumeDb,
        min_size: u64,
    ) -> Result<()> {
        let rows = db.quick_hashes(min_size)?;
        self.dupes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .replace_volume(id, &rows)
    }

    /// Compute full hashes for every connected member of a duplicate group and report back.
    pub fn verify_group(&self, hash_hex: &str) -> Result<DupGroup> {
        self.verify_group_with(hash_hex, &AtomicBool::new(false), &|_| {})
    }

    /// Hash every connected copy in a duplicate group in full, reporting progress. Copies
    /// hashed in full before are not read again: a file that changes loses its hashes.
    pub fn verify_group_with(
        &self,
        hash_hex: &str,
        cancel: &AtomicBool,
        progress: &dyn Fn(&VerifyProgress),
    ) -> Result<DupGroup> {
        let hash = unhex(hash_hex).ok_or_else(|| Error::Other("bad hash".into()))?;
        let members = self
            .dupes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .members(&hash)?;
        let volumes = self.volumes()?;
        let now = now_unix();
        // What needs reading: connected copies without a full hash.
        let mut todo: Vec<(VolumeId, i64, PathBuf, u64)> = Vec::new();
        for (vid, fid) in &members {
            let Some(v) = volumes.iter().find(|v| v.id == *vid) else {
                continue;
            };
            let Some(mount) = &v.mount_path else { continue };
            let db = VolumeDb::open(&self.dirs.volume_db(*vid))?;
            let Some(f) = db.file(*fid)? else { continue };
            if db.hashes(*fid)?.1.is_some() {
                continue;
            }
            let path = PathBuf::from(join_abs(mount, &f.path, &f.name));
            todo.push((*vid, *fid, path, f.size));
        }
        let mut p = VerifyProgress {
            hash: hash_hex.to_string(),
            bytes_total: todo.iter().map(|t| t.3).sum(),
            files_total: todo.len() as u64,
            ..Default::default()
        };
        progress(&p);
        let mut last = Instant::now();
        for (vid, fid, path, size) in &todo {
            p.current = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            progress(&p);
            let start = p.bytes_done;
            let read = full_hash_with(path, cancel, &mut |n| {
                p.bytes_done += n;
                if last.elapsed() >= Duration::from_millis(200) {
                    last = Instant::now();
                    progress(&p);
                }
            });
            if cancel.load(Ordering::Relaxed) {
                return Err(Error::Other("cancelled".into()));
            }
            if let Ok(full) = read {
                VolumeDb::open_rw(&self.dirs.volume_db(*vid))?.set_full_hash(*fid, &full, now)?;
            }
            p.bytes_done = start + size;
            p.files_done += 1;
        }
        p.current.clear();
        p.done = true;
        progress(&p);
        self.duplicate_group(&hash, None)?
            .ok_or_else(|| Error::Other("group vanished".into()))
    }
}

pub fn unhex(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}

/// State shared by the passes of one enrichment run.
struct PassRun<'a> {
    id: VolumeId,
    root: &'a str,
    opts: &'a EnrichOptions,
    threads: usize,
    db_path: &'a Path,
    writer: &'a VolumeDb,
    thumbs: &'a ThumbDb,
    tag_map: std::collections::HashMap<String, Vec<String>>,
    cancel: &'a AtomicBool,
    progress: &'a (dyn Fn(EnrichProgress) + Sync),
    started: Instant,
    stats: EnrichStats,
    watch_faults: bool,
    /// Files whose reads failed with disk errors this run.
    io_faults: u32,
    /// Reads that took far longer than their size explains.
    slow_reads: u32,
}

/// Disk read errors (or crawling reads) that stop a run: a drive that fails this often is
/// failing, and every retry strains it further.
const FAULTS_TO_STOP: u32 = 3;
/// A read still going this long after its size explains has hung on damaged sectors.
const STALLED_READ: Duration = Duration::from_secs(90);

/// Whether reading a file took far longer than its size explains (20 s, plus 5 MB/s).
fn is_slow_read(read_ms: u64, size: u64) -> bool {
    read_ms > 20_000 + size / 5_000
}

type Written = (work::Outcome, Vec<(i64, storecat_extract::ArchiveEntry)>);

impl PassRun<'_> {
    fn emit(&self, pass: u8, done: u64, total: u64, current: &str) {
        let s = &self.stats;
        (self.progress)(EnrichProgress {
            volume_id: self.id,
            phase: EnrichPhase::Running,
            pass,
            done,
            total,
            hashed: s.hashed,
            metas: s.metas,
            thumbs: s.thumbs,
            texts: s.texts,
            archives: s.archives,
            elapsed_ms: self.started.elapsed().as_millis() as u64,
            current: current.to_string(),
        });
    }

    /// Stop the run because the drive is failing to read: record why on the volume (so
    /// nothing resumes it by itself) and tell the UI now, before hung reads return.
    fn stop_for_fault(&mut self, cat: &Catalog, pass: u8, why: String) {
        log::warn!("volume {}: stopping enrichment: {why}", self.id);
        if let Err(e) = cat.set_volume_fault(self.id, Some(&why)) {
            log::warn!("volume {}: could not record the fault: {e}", self.id);
        }
        let s = &self.stats;
        (self.progress)(EnrichProgress {
            volume_id: self.id,
            phase: EnrichPhase::Stopped {
                message: why.clone(),
            },
            pass,
            done: 0,
            total: 0,
            hashed: s.hashed,
            metas: s.metas,
            thumbs: s.thumbs,
            texts: s.texts,
            archives: s.archives,
            elapsed_ms: self.started.elapsed().as_millis() as u64,
            current: String::new(),
        });
        self.stats.fault = Some(why);
    }

    /// Record files the planner settled without reading them.
    fn settle(&mut self, settled: &[plan::Settled]) -> Result<()> {
        let now = now_unix();
        for chunk in settled.chunks(50_000) {
            self.writer.begin()?;
            for s in chunk {
                self.writer.mark_enriched(s.id, s.done, s.stages, now)?;
            }
            self.writer.commit()?;
        }
        self.stats.processed += settled.iter().filter(|s| s.done == ENRICH_DONE).count() as u64;
        Ok(())
    }

    /// Stream `jobs` through the workers and write what comes back. Returns whether the
    /// run was paused (cancelled, or the drive went away).
    fn pass(
        &mut self,
        cat: &Catalog,
        pass: u8,
        jobs: &[plan::Job],
        ctx: &work::Ctx,
    ) -> Result<bool> {
        let total = jobs.len() as u64;
        self.emit(pass, 0, total, "");
        if jobs.is_empty() {
            return Ok(false);
        }
        let stop = AtomicBool::new(false);
        let (job_tx, job_rx) = crossbeam_channel::bounded::<work::Item>(self.threads * 2);
        let (out_tx, out_rx) = crossbeam_channel::bounded::<work::Outcome>(self.threads * 4);
        let db_path = self.db_path;
        std::thread::scope(|s| -> Result<bool> {
            let stop = &stop;
            // Read turns continue from the previous pass's last number.
            let first = ctx.turn.map(|t| t.position()).unwrap_or(0);
            let feeder = s.spawn(move || -> Result<()> {
                let db = VolumeDb::open(db_path)?;
                let mut seq = first;
                for j in jobs {
                    if stop.load(Ordering::Relaxed) {
                        break;
                    }
                    let Some(file) = db.file(j.id)? else { continue };
                    // Detail-pass jobs, and artwork-only jobs for files enriched by an older
                    // version, keep what was found before in the search index.
                    let prior_meta = if j.stages & STAGE_META != 0 {
                        db.meta_json(j.id)?
                    } else {
                        None
                    };
                    let prior_text = if pass == 1 && j.stages & STAGE_TEXT != 0 {
                        db.text(j.id)?
                    } else {
                        None
                    };
                    let item = work::Item {
                        seq,
                        file,
                        work: j.work,
                        pass,
                        prior_stages: j.stages,
                        prior_meta,
                        prior_text,
                    };
                    seq += 1;
                    if job_tx.send(item).is_err() {
                        break;
                    }
                }
                Ok(())
            });
            for _ in 0..self.threads {
                let job_rx = job_rx.clone();
                let out_tx = out_tx.clone();
                s.spawn(move || {
                    for item in job_rx.iter() {
                        if stop.load(Ordering::Relaxed) {
                            break;
                        }
                        let file = item.file.clone();
                        let out = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            work::process(ctx, item)
                        }))
                        .unwrap_or_else(|_| work::failed(file, "could not be read"));
                        if out_tx.send(out).is_err() {
                            break;
                        }
                    }
                });
            }
            drop(job_rx);
            drop(out_tx);
            let written = self.write_outcomes(cat, pass, total, &out_rx, ctx.turn);
            // Stop feeding; workers finish what they hold and find nobody listening.
            stop.store(true, Ordering::Relaxed);
            if !matches!(written, Ok(false)) {
                // Stopping early: nobody waits for a read turn, so no further file is read.
                if let Some(t) = ctx.turn {
                    t.abort();
                }
            }
            drop(out_rx);
            let fed = feeder.join().unwrap_or(Ok(()));
            let paused = written?;
            fed?;
            Ok(paused)
        })
    }

    fn write_outcomes(
        &mut self,
        cat: &Catalog,
        pass: u8,
        total: u64,
        outcomes: &crossbeam_channel::Receiver<work::Outcome>,
        turn: Option<&work::Turn>,
    ) -> Result<bool> {
        let mut pending: Vec<Written> = Vec::with_capacity(self.opts.batch);
        let mut done = 0u64;
        let mut current = String::new();
        let mut last_commit = Instant::now();
        let mut last_emit = Instant::now();
        let mut since_index = 0usize;
        let mut last_index = Instant::now();
        let mut paused = false;
        let mut open = true;
        self.writer.begin()?;
        self.thumbs.begin()?;
        loop {
            // Wait in short steps: a pause, or a read hung on a damaged disk, must be
            // noticed even when no file finishes.
            let out = match outcomes.recv_timeout(Duration::from_millis(250)) {
                Ok(out) => out,
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                    if self.cancel.load(Ordering::Relaxed) {
                        paused = true;
                        break;
                    }
                    if let Some(age) = turn.and_then(|t| t.hung_read(STALLED_READ)) {
                        if self.watch_faults {
                            self.stop_for_fault(
                                cat,
                                pass,
                                format!("a read hung for over {} s", age.as_secs()),
                            );
                            paused = true;
                            break;
                        }
                    }
                    continue;
                }
                Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
            };
            if self.cancel.load(Ordering::Relaxed) {
                paused = true;
                break;
            }
            if self.watch_faults {
                self.io_faults += out.io_fault as u32;
                self.slow_reads += is_slow_read(out.read_ms, out.file.size) as u32;
            }
            let done_value = if out.needs_detail {
                ENRICH_DETAIL_PENDING
            } else {
                ENRICH_DONE
            };
            let Some(entries) = self
                .writer
                .write_enrichment(&out.row, done_value, now_unix())?
            else {
                // Deleted or changed on disk while it was read: live changes took it out.
                done += 1;
                continue;
            };
            if let Some((jpeg, w, h)) = &out.row.thumb {
                self.thumbs.put(out.file.id, jpeg, *w, *h)?;
            }
            done += 1;
            if done_value == ENRICH_DONE {
                self.stats.processed += 1;
            }
            pending.push((out, entries));
            if pending.len() >= self.opts.batch || last_commit.elapsed() >= Duration::from_secs(1) {
                if !Path::new(self.root).is_dir() {
                    // The drive went away mid-batch: these reads may be failures, not
                    // facts. Forget them and redo them next time.
                    self.writer.rollback()?;
                    self.thumbs.rollback()?;
                    open = false;
                    paused = true;
                    break;
                }
                self.thumbs.commit()?;
                self.writer.commit()?;
                if let Some((o, _)) = pending.last() {
                    current = item_path(&o.file);
                }
                since_index += pending.len();
                self.index_outcomes(cat, pass, &mut pending)?;
                self.writer.begin()?;
                self.thumbs.begin()?;
                last_commit = Instant::now();
                if since_index >= 5000 || last_index.elapsed() >= Duration::from_secs(20) {
                    cat.index.commit()?;
                    // Saved with the next batch; until then a restart re-indexes a little extra.
                    mark_index_synced(self.writer)?;
                    since_index = 0;
                    last_index = Instant::now();
                }
            }
            if last_emit.elapsed() >= Duration::from_millis(250) {
                self.emit(pass, done, total, &current);
                last_emit = Instant::now();
            }
            if self.io_faults >= FAULTS_TO_STOP || self.slow_reads >= FAULTS_TO_STOP {
                let why = if self.io_faults >= FAULTS_TO_STOP {
                    format!("{} files failed with disk read errors", self.io_faults)
                } else {
                    format!("{} reads took far too long", self.slow_reads)
                };
                self.stop_for_fault(cat, pass, why);
                paused = true;
                break;
            }
        }
        if open {
            if !Path::new(self.root).is_dir() {
                self.writer.rollback()?;
                self.thumbs.rollback()?;
                paused = true;
            } else {
                self.thumbs.commit()?;
                self.writer.commit()?;
                self.index_outcomes(cat, pass, &mut pending)?;
            }
        }
        self.emit(pass, done, total, &current);
        Ok(paused)
    }

    /// Update index documents for written outcomes and count what they added.
    fn index_outcomes(
        &mut self,
        cat: &Catalog,
        pass: u8,
        pending: &mut Vec<Written>,
    ) -> Result<()> {
        // Live changes may have removed or moved files since they were read: index what
        // the catalogue holds now, under the same lock live changes take.
        let _live = cat.live_gate.lock().unwrap_or_else(|e| e.into_inner());
        for (out, entries) in pending.drain(..) {
            let r = &out.row;
            if r.error.is_some() {
                self.stats.errors += 1;
            }
            if r.stages & !STAGE_SKIPPED_PLACEHOLDER == 0 {
                continue; // nothing new for the index
            }
            let Some(file) = self.writer.file(out.file.id)? else {
                continue;
            };
            let tags = self
                .tag_map
                .get(&item_path(&file))
                .map(|v| v.as_slice())
                .unwrap_or(&[]);
            // The detail pass never lists archives: leave entry documents alone.
            let stages = if pass == 1 {
                out.stages_all
            } else {
                out.stages_all & !STAGE_ARCHIVE
            };
            cat.index_enriched_file(
                self.id,
                &file,
                out.meta_json.as_deref(),
                r.text.as_deref().or(out.prior_text.as_deref()),
                stages,
                &entries,
                tags,
            )?;
            self.stats.hashed += (r.stages & STAGE_HASH != 0) as u64;
            self.stats.metas += (r.stages & STAGE_META != 0) as u64;
            self.stats.thumbs += (r.stages & STAGE_THUMB != 0) as u64;
            self.stats.texts += (r.stages & STAGE_TEXT != 0) as u64;
            self.stats.archives += (r.stages & STAGE_ARCHIVE != 0) as u64;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::is_slow_read;

    #[test]
    fn slow_reads_allow_for_size() {
        assert!(!is_slow_read(3_000, 1_000));
        assert!(is_slow_read(25_000, 1_000));
        // A 500 MB file read at 10 MB/s is slow-ish but explained by its size.
        assert!(!is_slow_read(50_000, 500_000_000));
        assert!(is_slow_read(200_000, 500_000_000));
    }
}
