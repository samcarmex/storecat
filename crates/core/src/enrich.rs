//! The enrichment pass: hashes, metadata, thumbnails, text and archive listings for files
//! the walk pass already catalogued. Runs in the background while a drive is connected,
//! writes in batches, and resumes where it left off.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use rayon::prelude::*;

use crate::catalog::{join_abs, Catalog};
use crate::db::volume::{
    EnrichRow, FileRecord, ThumbDb, VolumeDb, STAGE_ARCHIVE, STAGE_HASH, STAGE_META,
    STAGE_SKIPPED_PLACEHOLDER, STAGE_TEXT, STAGE_THUMB,
};
use crate::error::{Error, Result};
use crate::index::{doc_key, DocInput};
use crate::model::*;
use crate::scan::is_placeholder;
use crate::tools::{find_ffmpeg, FfmpegPaths};

#[derive(Debug, Clone)]
pub struct EnrichOptions {
    /// Files smaller than this are not hashed (duplicates of tiny files don't matter).
    pub hash_min_bytes: u64,
    pub hash: bool,
    pub meta: bool,
    pub thumbs: bool,
    pub text: bool,
    pub archives: bool,
    /// 0 = pick from the drive type.
    pub threads: usize,
    pub text_max_bytes: usize,
    pub thumb_edge: u32,
    pub batch: usize,
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

/// A file whose enrichment has been written, with the ids of its new archive entries.
type Persisted<'a> = (
    &'a FileRecord,
    &'a EnrichRow,
    Vec<(i64, storecat_extract::ArchiveEntry)>,
);

struct Ctx<'a> {
    root: &'a str,
    opts: &'a EnrichOptions,
    ffmpeg: Option<&'a FfmpegPaths>,
}

const META_DOC_EXTS: &[&str] = &[
    "pdf", "docx", "xlsx", "pptx", "docm", "xlsm", "pptm", "odt", "ods", "odp", "odg",
];

fn process_file(ctx: &Ctx, f: &FileRecord) -> EnrichRow {
    let mut row = EnrichRow {
        file_id: f.id,
        ..Default::default()
    };
    if is_placeholder(f.attrs) {
        row.stages = STAGE_SKIPPED_PLACEHOLDER;
        return row;
    }
    let path = PathBuf::from(join_abs(ctx.root, &f.path, &f.name));
    let ext = f.ext.as_str();

    if ctx.opts.hash && f.size >= ctx.opts.hash_min_bytes {
        match quick_hash(&path, f.size) {
            Ok(h) => {
                row.quick_hash = Some(h);
                row.stages |= STAGE_HASH;
            }
            Err(e) => row.error = Some(format!("hash: {e}")),
        }
    }

    let media = matches!(f.kind, Kind::Image | Kind::Video | Kind::Audio);
    let mut meta: Option<storecat_extract::FileMeta> = None;
    if ctx.opts.meta && (media || META_DOC_EXTS.contains(&ext)) && f.size > 0 {
        meta = storecat_extract::extract_meta(&path, ext);
        if meta.is_none() && matches!(f.kind, Kind::Video | Kind::Audio) {
            if let Some(ff) = ctx.ffmpeg {
                meta = storecat_extract::ffmpeg::probe(&ff.ffprobe, &path);
            }
        }
        if let Some(m) = &meta {
            row.meta_json = serde_json::to_string(m).ok();
            row.stages |= STAGE_META;
        }
    }

    if ctx.opts.thumbs && f.size > 0 {
        let thumb = match f.kind {
            Kind::Image => storecat_extract::make_thumbnail(&path, ext, ctx.opts.thumb_edge),
            Kind::Video => ctx.ffmpeg.and_then(|ff| {
                storecat_extract::ffmpeg::video_thumbnail(
                    &ff.ffmpeg,
                    &path,
                    meta.as_ref().and_then(|m| m.duration_ms),
                    ctx.opts.thumb_edge,
                )
            }),
            _ => None,
        };
        if let Some(t) = thumb {
            row.thumb = Some((t.jpeg, t.width, t.height));
            row.stages |= STAGE_THUMB;
        }
    }

    if ctx.opts.text && !media && f.size > 0 {
        if let Some(t) = storecat_extract::extract_text(&path, ext, ctx.opts.text_max_bytes) {
            row.text = Some(t);
            row.stages |= STAGE_TEXT;
        }
    }

    let listable = f.kind == Kind::Archive || ext == "iso";
    if ctx.opts.archives && listable && f.size > 0 {
        if let Some(entries) = storecat_extract::list_archive(
            &path,
            ext,
            storecat_extract::archives::DEFAULT_MAX_ENTRIES,
        ) {
            row.entries = Some(entries);
            row.stages |= STAGE_ARCHIVE;
        }
    }
    row
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

    /// Ask a running enrichment to stop after its current batch.
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
        let writer = VolumeDb::open_rw(&db_path)?;
        let reader = self.volume_db(id)?;
        let thumbs = ThumbDb::create(&self.dirs.thumbs_db(id))?;
        let total = reader.pending_count()?;
        let ffmpeg = find_ffmpeg(&self.dirs);
        let threads = if opts.threads > 0 {
            opts.threads
        } else {
            match volume.media_type {
                MediaType::Hdd | MediaType::Optical => 3,
                MediaType::Network | MediaType::Cloud => 4,
                _ => std::thread::available_parallelism()
                    .map(|n| n.get())
                    .unwrap_or(4)
                    .clamp(2, 8),
            }
        };
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .thread_name(|i| format!("storecat-enrich-{i}"))
            .build()
            .map_err(|e| Error::Other(e.to_string()))?;
        let ctx = Ctx {
            root,
            opts,
            ffmpeg: ffmpeg.as_ref(),
        };

        let tag_map = self.master().tag_map(id)?;
        let mut stats = EnrichStats::default();
        let mut since_commit = 0usize;
        let mut last_commit = Instant::now();
        let mut paused = false;

        loop {
            if cancel.load(Ordering::Relaxed) || !Path::new(root).is_dir() {
                paused = true;
                break;
            }
            let batch = reader.pending_enrichment(opts.batch)?;
            if batch.is_empty() {
                break;
            }
            let current = batch
                .last()
                .map(|f| {
                    if f.path.is_empty() {
                        f.name.clone()
                    } else {
                        format!("{}/{}", f.path, f.name)
                    }
                })
                .unwrap_or_default();

            let rows: Vec<EnrichRow> =
                pool.install(|| batch.par_iter().map(|f| process_file(&ctx, f)).collect());

            // Persist the batch and update the index documents it touched.
            let now = now_unix();
            writer.begin()?;
            thumbs.begin()?;
            let mut persisted: Vec<Persisted<'_>> = Vec::with_capacity(rows.len());
            for (f, row) in batch.iter().zip(rows.iter()) {
                let entries = writer.write_enrichment(row, now)?;
                if let Some((jpeg, w, h)) = &row.thumb {
                    thumbs.put(f.id, jpeg, *w, *h)?;
                }
                persisted.push((f, row, entries));
            }
            thumbs.commit()?;
            writer.commit()?;

            for (f, row, entries) in &persisted {
                if row.stages & !STAGE_SKIPPED_PLACEHOLDER == 0 {
                    continue; // nothing new for the index
                }
                let tags = tag_map
                    .get(&item_path(f))
                    .map(|v| v.as_slice())
                    .unwrap_or(&[]);
                self.index_enriched_file(
                    id,
                    f,
                    row.meta_json.as_deref(),
                    row.text.as_deref(),
                    row.stages,
                    entries,
                    tags,
                )?;
                stats.hashed += (row.stages & STAGE_HASH != 0) as u64;
                stats.metas += (row.stages & STAGE_META != 0) as u64;
                stats.thumbs += (row.stages & STAGE_THUMB != 0) as u64;
                stats.texts += (row.stages & STAGE_TEXT != 0) as u64;
                stats.archives += (row.stages & STAGE_ARCHIVE != 0) as u64;
            }
            stats.errors += rows.iter().filter(|r| r.error.is_some()).count() as u64;
            stats.processed += rows.len() as u64;
            since_commit += rows.len();

            if since_commit >= 5000 || last_commit.elapsed().as_secs() >= 20 {
                self.index.commit()?;
                since_commit = 0;
                last_commit = Instant::now();
            }
            progress(EnrichProgress {
                volume_id: id,
                phase: EnrichPhase::Running,
                done: stats.processed,
                total,
                hashed: stats.hashed,
                metas: stats.metas,
                thumbs: stats.thumbs,
                texts: stats.texts,
                archives: stats.archives,
                elapsed_ms: started.elapsed().as_millis() as u64,
                current,
            });
        }
        self.index.commit()?;
        self.refresh_dupes_for(id, &reader, opts.hash_min_bytes)?;
        stats.elapsed_ms = started.elapsed().as_millis() as u64;
        stats.paused = paused;
        Ok(stats)
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
