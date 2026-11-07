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
    let mut f = File::open(path)?;
    let mut h = blake3::Hasher::new();
    let mut buf = vec![0u8; 1024 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
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
        let result = self.enrich_inner(id, &volume, &root, opts, &cancel, progress);
        self.enriching
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&id);
        match &result {
            Ok(stats) => progress(EnrichProgress {
                volume_id: id,
                phase: if stats.paused {
                    EnrichPhase::Paused
                } else {
                    EnrichPhase::Done
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
        };
        log::info!(
            "enriching volume {id}: {threads} threads, {} disk",
            if spinning { "spinning" } else { "solid-state" }
        );

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
                plan::order_for_hdd(
                    &mut jobs,
                    &mut path_of,
                    &|| cancel.load(Ordering::Relaxed),
                    &mut tick,
                    Duration::from_millis(4),
                );
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
                break;
            }
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
        let hash = unhex(hash_hex).ok_or_else(|| Error::Other("bad hash".into()))?;
        let members = self
            .dupes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .members(&hash)?;
        let volumes = self.volumes()?;
        let now = now_unix();
        for (vid, fid) in &members {
            let Some(v) = volumes.iter().find(|v| v.id == *vid) else {
                continue;
            };
            let Some(mount) = &v.mount_path else { continue };
            let db = VolumeDb::open_rw(&self.dirs.volume_db(*vid))?;
            let Some(f) = db.file(*fid)? else { continue };
            let path = PathBuf::from(join_abs(mount, &f.path, &f.name));
            if let Ok(full) = full_hash(&path) {
                db.set_full_hash(*fid, &full, now)?;
            }
        }
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
                    let prior_meta = if pass == 2 && j.stages & STAGE_META != 0 {
                        db.meta_json(j.id)?
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
            let written = self.write_outcomes(cat, pass, total, &out_rx);
            // Stop feeding; workers finish what they hold and find nobody listening.
            stop.store(true, Ordering::Relaxed);
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
        for out in outcomes.iter() {
            if self.cancel.load(Ordering::Relaxed) {
                paused = true;
                break;
            }
            let done_value = if out.needs_detail {
                ENRICH_DETAIL_PENDING
            } else {
                ENRICH_DONE
            };
            let entries = self
                .writer
                .write_enrichment(&out.row, done_value, now_unix())?;
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
        for (out, entries) in pending.drain(..) {
            let r = &out.row;
            if r.error.is_some() {
                self.stats.errors += 1;
            }
            if r.stages & !STAGE_SKIPPED_PLACEHOLDER == 0 {
                continue; // nothing new for the index
            }
            let tags = self
                .tag_map
                .get(&item_path(&out.file))
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
                &out.file,
                out.meta_json.as_deref(),
                r.text.as_deref(),
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
