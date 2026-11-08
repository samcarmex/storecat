//! Live changes: what the OS reports about files changing on a connected volume, applied
//! to its catalogue and the search index without a rescan.
//!
//! Every change is checked against the disk as it is now, so applying a change twice, or
//! changes out of order, settles on the same catalogue a rescan would build. Files whose
//! content changed get a new id, which keeps enrichment that was reading the old content
//! from writing it back. New and changed files have no enrichment yet, so the next
//! enrichment run picks them up. Folder sizes are marked stale and rebuilt on next use.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::{Mutex, TryLockError};

use rusqlite::{params, OptionalExtension};
use serde::Serialize;

use crate::catalog::Catalog;
use crate::db::volume::{DirRecord, ThumbDb, VolumeDb};
use crate::error::{Error, Result};
use crate::index::{doc_key, parent_of, DocInput};
use crate::kind::{kind_for_file, split_ext};
use crate::model::{ItemType, Kind, VolumeId};
use crate::scan::{self, Counters, DirBatch, FileEntry, ScanOptions};

/// One thing the OS said happened, by absolute path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    /// Something at this path was created, changed or removed.
    Path(PathBuf),
    /// Renamed or moved within the volume.
    Rename { from: PathBuf, to: PathBuf },
    /// Changes under this folder were lost (the OS event buffer overflowed): compare the
    /// whole folder with the catalogue.
    Rescan(PathBuf),
}

/// What applying a set of changes did.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize)]
pub struct LiveStats {
    pub files_added: u64,
    /// Content changed: catalogued again and queued for enrichment.
    pub files_updated: u64,
    pub files_removed: u64,
    pub dirs_added: u64,
    pub dirs_removed: u64,
    /// Files and folders renamed or moved in place, enrichment kept.
    pub renamed: u64,
    /// Folder modification times and file attributes refreshed.
    pub touched: u64,
}

impl LiveStats {
    pub fn changed(&self) -> bool {
        *self != Self::default()
    }

    /// Files that now wait for enrichment.
    pub fn to_enrich(&self) -> u64 {
        self.files_added + self.files_updated
    }
}

/// Folder at the root of a drive where Storecat keeps a copy of the catalogue.
const DRIVE_COPY_DIR: &str = ".storecat";
const INFO_NEXT_FILE: &str = "live_next_file_id";
const INFO_NEXT_DIR: &str = "live_next_dir_id";

/// Index updates, applied in order once the catalogue transaction has committed.
enum IndexOp {
    DropFile(i64),
    DropDir(i64),
    File(i64),
    Dir(i64),
}

impl Catalog {
    /// The path inside a volume (`/`-separated, relative to `root`) that a live change
    /// refers to, or `None` when it should be ignored: outside the volume, Storecat's own
    /// data folder or drive copy, or a folder the scanner skips.
    pub fn live_rel_path(&self, root: &Path, abs: &Path) -> Option<String> {
        if is_under(abs, &self.dirs.root) {
            return None;
        }
        let rest = strip_root(abs, root)?;
        let mut parts: Vec<String> = Vec::new();
        for c in rest.components() {
            match c {
                Component::Normal(p) => parts.push(p.to_string_lossy().into_owned()),
                Component::CurDir => {}
                _ => return None,
            }
        }
        let rel = parts.join("/");
        if parts.first().map(|p| p == DRIVE_COPY_DIR).unwrap_or(false)
            || scan::is_excluded(&rel, &ScanOptions::default())
        {
            return None;
        }
        Some(rel)
    }

    /// Apply live changes to a connected volume's catalogue and the search index.
    ///
    /// Returns [`Error::Busy`] while any scan runs (its half-built index must not be
    /// committed): keep the changes and try again later. A volume that has never been
    /// scanned is left alone.
    pub fn apply_changes(&self, id: VolumeId, changes: &[Change]) -> Result<LiveStats> {
        let volume = self.volume(id)?;
        let root = PathBuf::from(volume.mount_path.ok_or(Error::VolumeOffline)?);
        let db_path = self.dirs.volume_db(id);
        if !db_path.exists() {
            return Ok(LiveStats::default());
        }
        let _scan = match self.scan_gate.try_lock() {
            Ok(g) => g,
            Err(TryLockError::Poisoned(p)) => p.into_inner(),
            Err(TryLockError::WouldBlock) => return Err(Error::Busy),
        };
        if self.is_scanning(id) || self.scan_interrupted(id) {
            return Err(Error::Busy);
        }
        // Make sure the catalogue is on the current schema before writing to it.
        drop(self.volume_db(id)?);
        let _live = self.live_gate.lock().unwrap_or_else(|e| e.into_inner());

        let db = VolumeDb::open_rw(&db_path)?;
        db.conn().busy_timeout(std::time::Duration::from_secs(60))?;
        let mut ap = Applier::new(self, root, &db)?;
        db.begin()?;
        let applied = (|| -> Result<()> {
            for c in ap.pair_moves(changes)? {
                ap.apply(&c)?;
            }
            ap.finish()
        })();
        if let Err(e) = applied {
            let _ = db.rollback();
            return Err(e);
        }
        db.commit()?;

        let Applier {
            stats,
            ops,
            dropped,
            hashed_dropped,
            files_delta,
            dirs_delta,
            bytes_delta,
            ..
        } = ap;
        if !stats.changed() {
            return Ok(stats);
        }
        if !dropped.is_empty() {
            let thumbs = self.dirs.thumbs_db(id);
            if thumbs.exists() {
                if let Err(e) = ThumbDb::create(&thumbs).and_then(|t| t.delete(&dropped)) {
                    log::debug!("volume {id}: could not drop thumbnails: {e}");
                }
            }
        }
        self.master()
            .adjust_counts(id, files_delta, dirs_delta, bytes_delta)?;
        self.apply_index_ops(id, &db, &ops)?;
        self.index.commit()?;
        if hashed_dropped {
            let _ = self.refresh_dupes_for(id, &db, self.hash_min_bytes());
        }
        Ok(stats)
    }

    fn apply_index_ops(&self, id: VolumeId, db: &VolumeDb, ops: &[IndexOp]) -> Result<()> {
        let mut tag_map: Option<HashMap<String, Vec<String>>> = None;
        for op in ops {
            match *op {
                IndexOp::DropFile(fid) => {
                    let key = doc_key(id, ItemType::File, fid);
                    self.index.delete_doc(key)?;
                    self.index.delete_children(key)?;
                }
                IndexOp::DropDir(did) => {
                    self.index.delete_doc(doc_key(id, ItemType::Dir, did))?;
                }
                IndexOp::File(fid) => {
                    let key = doc_key(id, ItemType::File, fid);
                    self.index.delete_doc(key)?;
                    self.index.delete_children(key)?;
                    if let Some(f) = db.file(fid)? {
                        if tag_map.is_none() {
                            tag_map = Some(self.master().tag_map(id)?);
                        }
                        let tags = tag_map.as_ref().expect("loaded above");
                        self.index_file_from_db(id, db, &f, tags)?;
                    }
                }
                IndexOp::Dir(did) => {
                    self.index.delete_doc(doc_key(id, ItemType::Dir, did))?;
                    let Some(d) = db.dir(did)? else { continue };
                    if d.id == 0 {
                        continue; // the volume root has no document
                    }
                    if tag_map.is_none() {
                        tag_map = Some(self.master().tag_map(id)?);
                    }
                    let tags = tag_map.as_ref().expect("loaded above");
                    self.index.add_doc(&DocInput {
                        volume_id: id,
                        item_id: d.id,
                        item_type: ItemType::Dir,
                        name: &d.name,
                        path: parent_of(&d.path),
                        ext: "",
                        kind: Kind::Dir,
                        size: 0,
                        mtime: d.mtime,
                        flags: 0,
                        parent_key: 0,
                        content: None,
                        meta_text: None,
                        tags: tags.get(&d.path).map(|v| v.as_slice()).unwrap_or(&[]),
                    })?;
                }
            }
        }
        Ok(())
    }
}

/// Existing catalogue row for a file, as far as change detection needs it.
#[derive(Debug, Clone, Copy)]
struct FileRow {
    id: i64,
    size: u64,
    mtime: i64,
    attrs: u32,
}

struct Applier<'a> {
    cat: &'a Catalog,
    root: PathBuf,
    db: &'a VolumeDb,
    opts: ScanOptions,
    next_file: i64,
    next_dir: i64,
    stats: LiveStats,
    ops: Vec<IndexOp>,
    dropped: Vec<i64>,
    hashed_dropped: bool,
    files_delta: i64,
    dirs_delta: i64,
    bytes_delta: i64,
}

impl<'a> Applier<'a> {
    fn new(cat: &'a Catalog, root: PathBuf, db: &'a VolumeDb) -> Result<Self> {
        // Ids only ever grow, so enrichment still holding an old id can't hit a new file.
        let next = |info: &str, table: &str| -> Result<i64> {
            let stored = db
                .info(info)?
                .and_then(|v| v.parse::<i64>().ok())
                .unwrap_or(0);
            let max: i64 = db.conn().query_row(
                &format!("SELECT COALESCE(MAX(id), 0) + 1 FROM {table}"),
                [],
                |r| r.get(0),
            )?;
            Ok(stored.max(max))
        };
        Ok(Self {
            cat,
            root,
            db,
            opts: ScanOptions {
                threads: 2,
                ..Default::default()
            },
            next_file: next(INFO_NEXT_FILE, "files")?,
            next_dir: next(INFO_NEXT_DIR, "dirs")?,
            stats: LiveStats::default(),
            ops: Vec::new(),
            dropped: Vec::new(),
            hashed_dropped: false,
            files_delta: 0,
            dirs_delta: 0,
            bytes_delta: 0,
        })
    }

    fn rel(&self, abs: &Path) -> Option<String> {
        self.cat.live_rel_path(&self.root, abs)
    }

    /// Windows reports a move to another folder as a removal and an addition, not as a
    /// rename. A catalogued path that is gone, paired with a new path of the same name, is
    /// tried as a move first (which keeps the id and enrichment when size and time match
    /// too); the plain paths that follow then find nothing left to do.
    fn pair_moves(&self, changes: &[Change]) -> Result<Vec<Change>> {
        let mut gone: Vec<(String, &PathBuf)> = Vec::new();
        let mut fresh: Vec<(String, &PathBuf)> = Vec::new();
        for c in changes {
            let Change::Path(p) = c else { continue };
            let Some(rel) = self.rel(p).filter(|r| !r.is_empty()) else {
                continue;
            };
            let known =
                self.db.file_by_path(&rel)?.is_some() || self.db.dir_by_path(&rel)?.is_some();
            match fs::symlink_metadata(self.abs(&rel)) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound && known => gone.push((rel, p)),
                Ok(md) if !md.file_type().is_symlink() && !known => fresh.push((rel, p)),
                _ => {}
            }
        }
        if gone.is_empty() || fresh.is_empty() {
            return Ok(changes.to_vec());
        }
        let mut used: HashSet<usize> = HashSet::new();
        let mut moves: Vec<Change> = Vec::new();
        for (g_rel, g_abs) in &gone {
            let name = split(g_rel).1;
            let matches: Vec<usize> = fresh
                .iter()
                .enumerate()
                .filter(|(i, (f_rel, _))| !used.contains(i) && split(f_rel).1 == name)
                .map(|(i, _)| i)
                .collect();
            if let [i] = matches[..] {
                used.insert(i);
                moves.push(Change::Rename {
                    from: (*g_abs).clone(),
                    to: fresh[i].1.clone(),
                });
            }
        }
        // After the renames the OS reported, before the single paths.
        let at = changes
            .iter()
            .position(|c| matches!(c, Change::Path(_)))
            .unwrap_or(changes.len());
        let mut out = changes[..at].to_vec();
        out.extend(moves);
        out.extend_from_slice(&changes[at..]);
        Ok(out)
    }

    fn abs(&self, rel: &str) -> PathBuf {
        let mut p = self.root.clone();
        for part in rel.split('/').filter(|p| !p.is_empty()) {
            p.push(part);
        }
        p
    }

    fn apply(&mut self, change: &Change) -> Result<()> {
        match change {
            Change::Path(p) => match self.rel(p) {
                Some(rel) => self.path(&rel),
                None => Ok(()),
            },
            Change::Rescan(p) => match self.rel(p) {
                Some(rel) => self.tree(&rel),
                None => Ok(()),
            },
            Change::Rename { from, to } => match (self.rel(from), self.rel(to)) {
                (Some(from), Some(to)) => self.rename(&from, &to),
                (Some(only), None) | (None, Some(only)) => self.path(&only),
                (None, None) => Ok(()),
            },
        }
    }

    fn finish(&mut self) -> Result<()> {
        if !self.stats.changed() {
            return Ok(());
        }
        self.db
            .set_info(INFO_NEXT_FILE, &self.next_file.to_string())?;
        self.db
            .set_info(INFO_NEXT_DIR, &self.next_dir.to_string())?;
        // Folder sizes and extension counts are rebuilt from the files on next use.
        self.db
            .conn()
            .execute("DELETE FROM info WHERE key IN ('dir_sizes', 'exts_at')", [])?;
        Ok(())
    }

    // ----- one path -------------------------------------------------------------------

    /// Make the catalogue match the disk at `rel`.
    fn path(&mut self, rel: &str) -> Result<()> {
        let abs = self.abs(rel);
        let md = match fs::symlink_metadata(&abs) {
            Ok(md) => md,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return self.remove(rel),
            // Locked or unreadable right now: the next event or rescan catches up.
            Err(_) => return Ok(()),
        };
        if md.file_type().is_symlink() {
            // The scanner never follows or lists links.
            return self.remove(rel);
        }
        if md.is_dir() {
            if let Some(f) = self.db.file_by_path(rel)? {
                self.drop_file(f.id)?;
                self.stats.files_removed += 1;
            }
            match self.db.dir_by_path(rel)? {
                Some(d) => {
                    let mtime = scan::to_unix(md.modified().ok());
                    if d.mtime != mtime {
                        self.db.conn().execute(
                            "UPDATE dirs SET mtime = ?2 WHERE id = ?1",
                            params![d.id, mtime],
                        )?;
                        self.ops.push(IndexOp::Dir(d.id));
                        self.stats.touched += 1;
                    }
                    Ok(())
                }
                None => self.tree(rel),
            }
        } else {
            if !rel.is_empty() {
                if let Some(_d) = self.db.dir_by_path(rel)? {
                    self.drop_tree(rel)?;
                }
            }
            self.upsert_file(rel, &md)
        }
    }

    fn remove(&mut self, rel: &str) -> Result<()> {
        if rel.is_empty() {
            return Ok(()); // the drive itself went away: that is the drive watcher's news
        }
        if let Some(f) = self.db.file_by_path(rel)? {
            self.drop_file(f.id)?;
            self.stats.files_removed += 1;
        }
        if self.db.dir_by_path(rel)?.is_some() {
            self.drop_tree(rel)?;
        }
        Ok(())
    }

    fn upsert_file(&mut self, rel: &str, md: &fs::Metadata) -> Result<()> {
        let (parent, name) = split(rel);
        let Some(dir) = self.db.dir_by_path(parent)? else {
            // Its folder isn't catalogued yet: catalogue the outermost missing folder.
            let top = self.outermost_missing(parent)?;
            return self.tree(&top);
        };
        let entry = file_entry(name, md);
        let existing = self.file_in(dir.id, name)?;
        self.put_file(dir.id, existing, entry)
    }

    /// Add `entry` to folder `dir_id`, replacing `existing` if its content changed.
    fn put_file(&mut self, dir_id: i64, existing: Option<FileRow>, entry: FileEntry) -> Result<()> {
        if let Some(old) = existing {
            if old.size == entry.size && old.mtime == entry.mtime {
                if old.attrs != entry.attrs {
                    self.db.conn().execute(
                        "UPDATE files SET attrs = ?2 WHERE id = ?1",
                        params![old.id, entry.attrs as i64],
                    )?;
                    self.ops.push(IndexOp::File(old.id));
                    self.stats.touched += 1;
                }
                return Ok(());
            }
            self.drop_file(old.id)?;
            self.stats.files_updated += 1;
        } else {
            self.stats.files_added += 1;
        }
        self.insert_file(dir_id, &entry)?;
        Ok(())
    }

    fn insert_file(&mut self, dir_id: i64, f: &FileEntry) -> Result<i64> {
        let id = self.next_file;
        self.next_file += 1;
        self.db.conn().execute(
            "INSERT INTO files (id, dir_id, name, ext, size, mtime, ctime, attrs, kind)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                id,
                dir_id,
                f.name,
                f.ext,
                f.size as i64,
                f.mtime,
                f.ctime,
                f.attrs as i64,
                f.kind as u8 as i64
            ],
        )?;
        self.files_delta += 1;
        self.bytes_delta += f.size as i64;
        self.ops.push(IndexOp::File(id));
        Ok(id)
    }

    fn insert_dir(&mut self, parent_id: i64, name: &str, path: &str, mtime: i64) -> Result<i64> {
        let id = self.next_dir;
        self.next_dir += 1;
        self.db.conn().execute(
            "INSERT INTO dirs (id, parent_id, name, path, mtime) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![id, parent_id, name, path, mtime],
        )?;
        self.dirs_delta += 1;
        self.stats.dirs_added += 1;
        self.ops.push(IndexOp::Dir(id));
        Ok(id)
    }

    fn file_in(&self, dir_id: i64, name: &str) -> Result<Option<FileRow>> {
        let mut stmt = self.db.conn().prepare_cached(
            "SELECT id, size, mtime, attrs FROM files WHERE dir_id = ?1 AND name = ?2 LIMIT 1",
        )?;
        Ok(stmt
            .query_row(params![dir_id, name], |r| {
                Ok(FileRow {
                    id: r.get(0)?,
                    size: r.get::<_, i64>(1)? as u64,
                    mtime: r.get(2)?,
                    attrs: r.get::<_, i64>(3)? as u32,
                })
            })
            .optional()?)
    }

    /// Take a file and its enrichment out of the catalogue.
    fn drop_file(&mut self, id: i64) -> Result<()> {
        let conn = self.db.conn();
        let size: Option<i64> = conn
            .query_row("SELECT size FROM files WHERE id = ?1", [id], |r| r.get(0))
            .optional()?;
        let Some(size) = size else { return Ok(()) };
        if !self.hashed_dropped {
            self.hashed_dropped = conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM hashes WHERE file_id = ?1 AND quick IS NOT NULL)",
                [id],
                |r| r.get::<_, i64>(0),
            )? != 0;
        }
        for table in ["hashes", "meta", "text", "enrich", "archive_entries"] {
            conn.prepare_cached(&format!("DELETE FROM {table} WHERE file_id = ?1"))?
                .execute([id])?;
        }
        conn.prepare_cached("DELETE FROM files WHERE id = ?1")?
            .execute([id])?;
        self.files_delta -= 1;
        self.bytes_delta -= size;
        self.dropped.push(id);
        self.ops.push(IndexOp::DropFile(id));
        Ok(())
    }

    /// Take a folder, everything under it and their enrichment out of the catalogue.
    fn drop_tree(&mut self, rel: &str) -> Result<()> {
        let dirs = self.subtree_dirs(rel)?;
        if dirs.is_empty() {
            return Ok(());
        }
        let mut files: Vec<i64> = Vec::new();
        {
            let mut stmt = self
                .db
                .conn()
                .prepare_cached("SELECT id FROM files WHERE dir_id = ?1")?;
            for d in &dirs {
                let ids = stmt
                    .query_map([d.id], |r| r.get::<_, i64>(0))?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                files.extend(ids);
            }
        }
        self.stats.files_removed += files.len() as u64;
        for f in files {
            self.drop_file(f)?;
        }
        let mut del = self
            .db
            .conn()
            .prepare_cached("DELETE FROM dirs WHERE id = ?1")?;
        for d in &dirs {
            del.execute([d.id])?;
            self.ops.push(IndexOp::DropDir(d.id));
        }
        self.dirs_delta -= dirs.len() as i64;
        self.stats.dirs_removed += dirs.len() as u64;
        Ok(())
    }

    /// The folder at `rel` and every folder below it.
    fn subtree_dirs(&self, rel: &str) -> Result<Vec<DirRecord>> {
        let conn = self.db.conn();
        let (sql, args): (&str, Vec<String>) = if rel.is_empty() {
            ("SELECT id, parent_id, name, path, mtime FROM dirs", vec![])
        } else {
            // '0' sorts right after '/': a range on the path index, no LIKE escaping.
            (
                "SELECT id, parent_id, name, path, mtime FROM dirs
                 WHERE path = ?1 OR (path >= ?2 AND path < ?3)",
                vec![rel.to_string(), format!("{rel}/"), format!("{rel}0")],
            )
        };
        let mut stmt = conn.prepare_cached(sql)?;
        let rows = stmt
            .query_map(rusqlite::params_from_iter(args.iter()), |r| {
                Ok(DirRecord {
                    id: r.get(0)?,
                    parent_id: r.get(1)?,
                    name: r.get(2)?,
                    path: r.get(3)?,
                    mtime: r.get(4)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// The outermost folder on the way down to `rel` that isn't catalogued yet.
    fn outermost_missing(&self, rel: &str) -> Result<String> {
        let mut cur = rel.to_string();
        while !cur.is_empty() {
            let up = parent_of(&cur).to_string();
            if self.db.dir_by_path(&up)?.is_some() {
                break;
            }
            cur = up;
        }
        Ok(cur)
    }

    // ----- a whole folder -------------------------------------------------------------

    /// Compare a folder and everything below it with the disk, keeping unchanged files
    /// (and their enrichment) as they are.
    fn tree(&mut self, rel: &str) -> Result<()> {
        let abs = self.abs(rel);
        match fs::symlink_metadata(&abs) {
            Ok(md) if md.is_dir() && !md.file_type().is_symlink() => {}
            Ok(_) => return self.path(rel),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return self.remove(rel),
            Err(_) => return Ok(()),
        }
        let parent_id = if rel.is_empty() {
            None
        } else {
            match self.db.dir_by_path(parent_of(rel))? {
                Some(p) => Some(p.id),
                None => {
                    let top = self.outermost_missing(parent_of(rel))?;
                    return self.tree(&top);
                }
            }
        };

        let batches = Mutex::new(Vec::<DirBatch>::new());
        let sink = |b: DirBatch| batches.lock().unwrap_or_else(|e| e.into_inner()).push(b);
        let counters = Counters::default();
        let walked = if rel.is_empty() {
            scan::walk(&abs, &self.opts, &counters, &sink)
        } else {
            scan::walk_subtree(&abs, &self.opts, &counters, &sink)
        };
        if let Err(e) = walked {
            log::debug!("live: could not read {}: {e}", abs.display());
            return Ok(());
        }
        let mut batches = batches.into_inner().unwrap_or_else(|e| e.into_inner());
        // Parents before children.
        batches.sort_by_key(|b| {
            (
                b.rel_path.matches('/').count() + !b.rel_path.is_empty() as usize,
                b.dir_id,
            )
        });

        let mut known: HashMap<String, DirRecord> = self
            .subtree_dirs(rel)?
            .into_iter()
            .map(|d| (d.path.clone(), d))
            .collect();
        let mut ids: HashMap<i64, i64> = HashMap::new();
        let mut seen: HashSet<String> = HashSet::new();
        for b in batches {
            let path = join(rel, &b.rel_path);
            let name = if b.dir_id == 0 {
                split(rel).1.to_string()
            } else {
                b.name.clone()
            };
            let dir_id = match known.remove(&path) {
                Some(d) => {
                    if d.mtime != b.mtime {
                        self.db.conn().execute(
                            "UPDATE dirs SET mtime = ?2 WHERE id = ?1",
                            params![d.id, b.mtime],
                        )?;
                        self.ops.push(IndexOp::Dir(d.id));
                        self.stats.touched += 1;
                    }
                    d.id
                }
                None => {
                    let parent = if b.dir_id == 0 {
                        parent_id
                    } else {
                        b.parent_id.and_then(|p| ids.get(&p).copied())
                    };
                    let Some(parent) = parent else { continue };
                    self.insert_dir(parent, &name, &path, b.mtime)?
                }
            };
            ids.insert(b.dir_id, dir_id);
            seen.insert(path);

            let mut existing: HashMap<String, FileRow> = HashMap::new();
            {
                let mut stmt = self.db.conn().prepare_cached(
                    "SELECT id, name, size, mtime, attrs FROM files WHERE dir_id = ?1",
                )?;
                let rows = stmt.query_map([dir_id], |r| {
                    Ok((
                        r.get::<_, String>(1)?,
                        FileRow {
                            id: r.get(0)?,
                            size: r.get::<_, i64>(2)? as u64,
                            mtime: r.get(3)?,
                            attrs: r.get::<_, i64>(4)? as u32,
                        },
                    ))
                })?;
                for row in rows {
                    let (n, f) = row?;
                    existing.insert(n, f);
                }
            }
            for f in b.files {
                let old = existing.remove(&f.name);
                self.put_file(dir_id, old, f)?;
            }
            for (_, gone) in existing {
                self.drop_file(gone.id)?;
                self.stats.files_removed += 1;
            }
        }
        // Catalogued folders that are no longer there (outermost first).
        let mut gone: Vec<String> = known.into_keys().filter(|p| !seen.contains(p)).collect();
        gone.sort();
        for p in gone {
            self.drop_tree(&p)?;
        }
        Ok(())
    }

    // ----- renames --------------------------------------------------------------------

    /// Move a file or folder in the catalogue, keeping ids and enrichment. Anything that
    /// doesn't line up (destination folder not catalogued, the content changed too)
    /// falls back to checking both paths against the disk.
    fn rename(&mut self, from: &str, to: &str) -> Result<()> {
        if from == to || from.is_empty() || to.is_empty() {
            return self.path(to);
        }
        if self.try_move(from, to)? {
            self.stats.renamed += 1;
            // Something re-created at the old path since is caught here. Not for a change
            // of case only: on most Windows and macOS disks both spellings find the file.
            if !from.eq_ignore_ascii_case(to) {
                self.path(from)?;
            }
            return Ok(());
        }
        self.path(from)?;
        self.path(to)
    }

    fn try_move(&mut self, from: &str, to: &str) -> Result<bool> {
        let md = match fs::symlink_metadata(self.abs(to)) {
            Ok(md) if !md.file_type().is_symlink() => md,
            _ => return Ok(false),
        };
        let (to_parent, to_name) = split(to);
        let Some(dest) = self.db.dir_by_path(to_parent)? else {
            return Ok(false);
        };
        let same_except_case = from.eq_ignore_ascii_case(to);
        if md.is_file() {
            let Some(f) = self.db.file_by_path(from)? else {
                return Ok(false);
            };
            let entry = file_entry(to_name, &md);
            if entry.size != f.size || entry.mtime != f.mtime || entry.ext != f.ext {
                return Ok(false);
            }
            if !same_except_case {
                self.remove(to)?; // replaced whatever was there
            }
            self.db.conn().execute(
                "UPDATE files SET dir_id = ?2, name = ?3, attrs = ?4 WHERE id = ?1",
                params![f.id, dest.id, to_name, entry.attrs as i64],
            )?;
            self.ops.push(IndexOp::File(f.id));
            return Ok(true);
        }
        if !md.is_dir() {
            return Ok(false);
        }
        let Some(d) = self.db.dir_by_path(from)? else {
            return Ok(false);
        };
        if to.starts_with(&format!("{from}/")) {
            return Ok(false);
        }
        if !same_except_case {
            self.remove(to)?;
        }
        let conn = self.db.conn();
        conn.execute(
            "UPDATE dirs SET name = ?2, parent_id = ?3 WHERE id = ?1",
            params![d.id, to_name, dest.id],
        )?;
        conn.execute(
            "UPDATE dirs SET path = ?4 || substr(path, length(?1) + 1)
             WHERE path = ?1 OR (path >= ?2 AND path < ?3)",
            params![from, format!("{from}/"), format!("{from}0"), to],
        )?;
        let moved = self.subtree_dirs(to)?;
        let mut stmt = conn.prepare_cached("SELECT id FROM files WHERE dir_id = ?1")?;
        for m in &moved {
            self.ops.push(IndexOp::Dir(m.id));
            let ids = stmt
                .query_map([m.id], |r| r.get::<_, i64>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            self.ops.extend(ids.into_iter().map(IndexOp::File));
        }
        Ok(true)
    }
}

fn file_entry(name: &str, md: &fs::Metadata) -> FileEntry {
    let ext = split_ext(name);
    let size = md.len();
    FileEntry {
        id: 0,
        name: name.to_string(),
        kind: kind_for_file(&ext, size),
        ext,
        size,
        mtime: scan::to_unix(md.modified().ok()),
        ctime: scan::to_unix(md.created().ok()),
        attrs: scan::attrs_of(md),
    }
}

/// `("a/b", "c")` for `"a/b/c"`, `("", "c")` for `"c"`.
fn split(rel: &str) -> (&str, &str) {
    match rel.rfind('/') {
        Some(i) => (&rel[..i], &rel[i + 1..]),
        None => ("", rel),
    }
}

fn join(base: &str, rest: &str) -> String {
    match (base.is_empty(), rest.is_empty()) {
        (true, _) => rest.to_string(),
        (_, true) => base.to_string(),
        _ => format!("{base}/{rest}"),
    }
}

/// `abs` relative to `root`, ignoring case on Windows and macOS (the usual file systems
/// there don't care, and the OS may report either spelling of the drive letter).
fn strip_root(abs: &Path, root: &Path) -> Option<PathBuf> {
    if let Ok(rest) = abs.strip_prefix(root) {
        return Some(rest.to_path_buf());
    }
    if cfg!(any(windows, target_os = "macos")) {
        let a: Vec<Component> = abs.components().collect();
        let r: Vec<Component> = root.components().collect();
        if a.len() >= r.len()
            && a.iter().zip(&r).all(|(x, y)| {
                x.as_os_str()
                    .to_string_lossy()
                    .eq_ignore_ascii_case(&y.as_os_str().to_string_lossy())
            })
        {
            return Some(a[r.len()..].iter().collect());
        }
    }
    None
}

fn is_under(path: &Path, dir: &Path) -> bool {
    strip_root(path, dir).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::SearchOptions;
    use crate::paths::DataDirs;
    use std::sync::Arc;

    struct Fixture {
        _data: tempfile::TempDir,
        disk: tempfile::TempDir,
        cat: Arc<Catalog>,
        id: VolumeId,
    }

    impl Fixture {
        fn new() -> Self {
            let data = tempfile::tempdir().unwrap();
            let disk = tempfile::tempdir().unwrap();
            let root = disk.path();
            fs::create_dir_all(root.join("docs/old")).unwrap();
            fs::create_dir_all(root.join("node_modules/pkg")).unwrap();
            fs::write(root.join("docs/report.txt"), b"quarterly").unwrap();
            fs::write(root.join("docs/old/notes.txt"), b"old notes").unwrap();
            fs::write(root.join("top.bin"), vec![0u8; 50]).unwrap();
            let cat = Catalog::open(DataDirs {
                root: data.path().to_path_buf(),
            })
            .unwrap();
            let v = cat.add_folder(root, Some("Test")).unwrap();
            let opts = ScanOptions {
                threads: 2,
                ..Default::default()
            };
            cat.scan(v.id, &opts, &|_| {}).unwrap();
            Self {
                _data: data,
                disk,
                cat,
                id: v.id,
            }
        }

        fn p(&self, rel: &str) -> PathBuf {
            let mut p = self.disk.path().to_path_buf();
            for part in rel.split('/') {
                p.push(part);
            }
            p
        }

        fn apply(&self, changes: &[Change]) -> LiveStats {
            self.cat.apply_changes(self.id, changes).unwrap()
        }

        fn names(&self, q: &str) -> Vec<String> {
            let mut v: Vec<String> = self
                .cat
                .search(q, &SearchOptions::default())
                .unwrap()
                .hits
                .into_iter()
                .filter(|h| h.name.to_lowercase().contains(q))
                .map(|h| {
                    if h.path.is_empty() {
                        h.name
                    } else {
                        format!("{}/{}", h.path, h.name)
                    }
                })
                .collect();
            v.sort();
            v
        }

        fn db(&self) -> VolumeDb {
            VolumeDb::open(&self.cat.dirs.volume_db(self.id)).unwrap()
        }

        fn volume_counts(&self) -> (u64, u64, u64) {
            let v = self.cat.volume(self.id).unwrap();
            (v.file_count, v.dir_count, v.total_bytes)
        }
    }

    #[test]
    fn creates_changes_and_deletes_files() {
        let fx = Fixture::new();
        let (files0, dirs0, bytes0) = fx.volume_counts();

        fs::write(fx.p("docs/budget.xlsx"), vec![1u8; 30]).unwrap();
        let s = fx.apply(&[Change::Path(fx.p("docs/budget.xlsx"))]);
        assert_eq!(s.files_added, 1);
        assert_eq!(s.to_enrich(), 1);
        assert_eq!(fx.names("budget"), vec!["docs/budget.xlsx"]);
        assert_eq!(fx.volume_counts(), (files0 + 1, dirs0, bytes0 + 30));

        // Same event again: nothing to do.
        assert!(!fx
            .apply(&[Change::Path(fx.p("docs/budget.xlsx"))])
            .changed());

        // Content changes: new id, enrichment queued again.
        let old_id = fx
            .db()
            .file_by_path("docs/budget.xlsx")
            .unwrap()
            .unwrap()
            .id;
        fs::write(fx.p("docs/budget.xlsx"), vec![1u8; 70]).unwrap();
        let s = fx.apply(&[Change::Path(fx.p("docs/budget.xlsx"))]);
        assert_eq!(s.files_updated, 1);
        let f = fx.db().file_by_path("docs/budget.xlsx").unwrap().unwrap();
        assert_ne!(f.id, old_id);
        assert_eq!(f.size, 70);
        assert_eq!(fx.names("budget"), vec!["docs/budget.xlsx"]);

        fs::remove_file(fx.p("docs/budget.xlsx")).unwrap();
        let s = fx.apply(&[Change::Path(fx.p("docs/budget.xlsx"))]);
        assert_eq!(s.files_removed, 1);
        assert!(fx.names("budget").is_empty());
        assert_eq!(fx.volume_counts(), (files0, dirs0, bytes0));
    }

    #[test]
    fn new_folders_are_walked_and_deleted_folders_dropped() {
        let fx = Fixture::new();
        let (files0, dirs0, _) = fx.volume_counts();
        fs::create_dir_all(fx.p("photos/2024/trip")).unwrap();
        fs::write(fx.p("photos/2024/trip/beach.jpg"), vec![0u8; 10]).unwrap();
        fs::write(fx.p("photos/cover.png"), vec![0u8; 10]).unwrap();
        // Only the deepest file is reported: its missing folders come along.
        let s = fx.apply(&[Change::Path(fx.p("photos/2024/trip/beach.jpg"))]);
        assert_eq!(s.dirs_added, 3);
        assert_eq!(s.files_added, 2);
        assert_eq!(fx.names("beach"), vec!["photos/2024/trip/beach.jpg"]);
        assert_eq!(fx.names("trip"), vec!["photos/2024/trip"]);
        assert_eq!(fx.volume_counts().0, files0 + 2);
        assert_eq!(fx.volume_counts().1, dirs0 + 3);

        fs::remove_dir_all(fx.p("photos")).unwrap();
        let s = fx.apply(&[Change::Path(fx.p("photos"))]);
        assert_eq!(s.dirs_removed, 3);
        assert_eq!(s.files_removed, 2);
        assert!(fx.names("beach").is_empty());
        assert!(fx.names("trip").is_empty());
        assert_eq!(fx.volume_counts().0, files0);
        assert_eq!(fx.volume_counts().1, dirs0);
    }

    #[test]
    fn renames_keep_ids_and_move_whole_folders() {
        let fx = Fixture::new();
        let id = fx.db().file_by_path("docs/report.txt").unwrap().unwrap().id;
        fs::rename(fx.p("docs/report.txt"), fx.p("docs/final report.txt")).unwrap();
        let s = fx.apply(&[Change::Rename {
            from: fx.p("docs/report.txt"),
            to: fx.p("docs/final report.txt"),
        }]);
        assert_eq!(s.renamed, 1);
        assert_eq!(s.to_enrich(), 0);
        assert_eq!(
            fx.db()
                .file_by_path("docs/final report.txt")
                .unwrap()
                .unwrap()
                .id,
            id
        );
        assert_eq!(fx.names("report"), vec!["docs/final report.txt"]);

        fs::rename(fx.p("docs"), fx.p("archive")).unwrap();
        let s = fx.apply(&[Change::Rename {
            from: fx.p("docs"),
            to: fx.p("archive"),
        }]);
        assert_eq!(s.renamed, 1);
        assert_eq!(fx.names("notes"), vec!["archive/old/notes.txt"]);
        assert_eq!(fx.names("old"), vec!["archive/old"]);
        assert_eq!(
            fx.db()
                .file_by_path("archive/final report.txt")
                .unwrap()
                .unwrap()
                .id,
            id
        );
        assert!(fx.db().dir_by_path("docs").unwrap().is_none());

        // A rename seen only as two separate paths still ends up right.
        fs::rename(fx.p("archive/old/notes.txt"), fx.p("top notes.txt")).unwrap();
        fx.apply(&[
            Change::Path(fx.p("archive/old/notes.txt")),
            Change::Path(fx.p("top notes.txt")),
        ]);
        assert_eq!(fx.names("notes"), vec!["top notes.txt"]);
    }

    #[test]
    fn moves_reported_as_removal_and_addition_keep_ids() {
        let fx = Fixture::new();
        let file = fx.db().file_by_path("docs/report.txt").unwrap().unwrap().id;
        let notes = fx
            .db()
            .file_by_path("docs/old/notes.txt")
            .unwrap()
            .unwrap()
            .id;
        fs::rename(fx.p("docs/report.txt"), fx.p("report.txt")).unwrap();
        fs::create_dir_all(fx.p("keep")).unwrap();
        fx.apply(&[Change::Path(fx.p("keep"))]);
        fs::rename(fx.p("docs/old"), fx.p("keep/old")).unwrap();
        // As Windows reports moves between folders: old paths removed, new paths added.
        let s = fx.apply(&[
            Change::Path(fx.p("docs/report.txt")),
            Change::Path(fx.p("docs/old")),
            Change::Path(fx.p("report.txt")),
            Change::Path(fx.p("keep/old")),
        ]);
        assert_eq!(s.renamed, 2);
        assert_eq!(s.to_enrich(), 0);
        assert_eq!(
            fx.db().file_by_path("report.txt").unwrap().unwrap().id,
            file
        );
        assert_eq!(
            fx.db()
                .file_by_path("keep/old/notes.txt")
                .unwrap()
                .unwrap()
                .id,
            notes
        );
        assert_eq!(fx.names("notes"), vec!["keep/old/notes.txt"]);
        assert_eq!(fx.names("report"), vec!["report.txt"]);
    }

    #[test]
    fn rescan_of_a_folder_settles_everything_under_it() {
        let fx = Fixture::new();
        let keep = fx.db().file_by_path("docs/report.txt").unwrap().unwrap().id;
        fs::remove_dir_all(fx.p("docs/old")).unwrap();
        fs::create_dir_all(fx.p("docs/new")).unwrap();
        fs::write(fx.p("docs/new/plan.md"), b"plan").unwrap();
        let s = fx.apply(&[Change::Rescan(fx.p("docs"))]);
        assert_eq!(s.dirs_added, 1);
        assert_eq!(s.dirs_removed, 1);
        assert_eq!(s.files_added, 1);
        assert_eq!(s.files_removed, 1);
        assert!(fx.names("notes").is_empty());
        assert_eq!(fx.names("plan"), vec!["docs/new/plan.md"]);
        // Unchanged files keep their id (and so their enrichment).
        assert_eq!(
            fx.db().file_by_path("docs/report.txt").unwrap().unwrap().id,
            keep
        );
    }

    #[test]
    fn skips_excluded_folders_and_the_data_folder() {
        let fx = Fixture::new();
        fs::write(fx.p("node_modules/pkg/index.js"), b"x").unwrap();
        fs::create_dir_all(fx.p(".storecat")).unwrap();
        fs::write(fx.p(".storecat/catalog.db"), b"x").unwrap();
        let s = fx.apply(&[
            Change::Path(fx.p("node_modules/pkg/index.js")),
            Change::Path(fx.p(".storecat/catalog.db")),
            Change::Path(fx.cat.dirs.master_db()),
        ]);
        assert!(!s.changed());
        let root = fx.disk.path();
        assert_eq!(
            fx.cat.live_rel_path(root, &fx.p("docs/a.txt")).as_deref(),
            Some("docs/a.txt")
        );
        assert_eq!(fx.cat.live_rel_path(root, &fx.p("docs/.git/HEAD")), None);
        assert_eq!(fx.cat.live_rel_path(root, Path::new("/elsewhere/x")), None);
    }

    #[test]
    fn folder_sizes_are_rebuilt_after_changes() {
        let fx = Fixture::new();
        let before = fx.cat.folder_tree(fx.id, None).unwrap().node.bytes;
        fs::write(fx.p("docs/big.bin"), vec![0u8; 1000]).unwrap();
        fx.apply(&[Change::Path(fx.p("docs/big.bin"))]);
        assert_eq!(
            fx.cat.folder_tree(fx.id, None).unwrap().node.bytes,
            before + 1000
        );
    }

    #[test]
    fn ids_are_never_reused() {
        let fx = Fixture::new();
        fs::write(fx.p("a.txt"), b"a").unwrap();
        fx.apply(&[Change::Path(fx.p("a.txt"))]);
        let first = fx.db().file_by_path("a.txt").unwrap().unwrap().id;
        fs::remove_file(fx.p("a.txt")).unwrap();
        fx.apply(&[Change::Path(fx.p("a.txt"))]);
        fs::write(fx.p("b.txt"), b"b").unwrap();
        fx.apply(&[Change::Path(fx.p("b.txt"))]);
        let second = fx.db().file_by_path("b.txt").unwrap().unwrap().id;
        assert!(second > first);
    }
}
