//! The walk pass: enumerate every directory and file under a root as fast as the disk
//! allows, in parallel, emitting one `DirBatch` per directory.
//!
//! Design notes:
//! - Directory and file ids are handed out from atomic counters by the walker itself, so
//!   batches can be written to SQLite and the search index in any order, on any thread.
//! - On Windows `read_dir` already returns size, times and attributes with each entry, so
//!   `entry.metadata()` costs no extra syscall. On Unix it is one `lstat` per file.
//! - Symlinks and junctions are never followed (cycle safety) and are not catalogued yet.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use crate::error::{Error, Result};
use crate::kind::{kind_for_file, split_ext};
use crate::model::Kind;

#[derive(Debug, Clone)]
pub struct FileEntry {
    pub id: i64,
    pub name: String,
    pub ext: String,
    pub size: u64,
    pub mtime: i64,
    pub ctime: i64,
    pub attrs: u32,
    pub kind: Kind,
}

#[derive(Debug, Clone)]
pub struct DirBatch {
    pub dir_id: i64,
    pub parent_id: Option<i64>,
    pub name: String,
    /// `/`-separated path relative to the root; empty for the root itself.
    pub rel_path: String,
    pub mtime: i64,
    pub files: Vec<FileEntry>,
}

#[derive(Debug, Clone)]
pub struct ScanOptions {
    /// Directory names skipped anywhere in the tree.
    pub exclude_names: Vec<String>,
    /// Directory names skipped only at the volume root (OS housekeeping folders).
    pub exclude_root_names: Vec<String>,
    pub threads: usize,
}

impl Default for ScanOptions {
    fn default() -> Self {
        let threads = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4)
            .clamp(2, 16);
        Self {
            exclude_names: vec![
                "node_modules".into(),
                ".git".into(),
                ".Trashes".into(),
                ".Spotlight-V100".into(),
                ".fseventsd".into(),
                ".TemporaryItems".into(),
                ".DocumentRevisions-V100".into(),
                "$RECYCLE.BIN".into(),
                "System Volume Information".into(),
            ],
            exclude_root_names: vec![
                "$Recycle.Bin".into(),
                "Recovery".into(),
                "Config.Msi".into(),
                "PerfLogs".into(),
                "Windows".into(),
                "ProgramData".into(),
                "Program Files".into(),
                "Program Files (x86)".into(),
                "hiberfil.sys".into(),
                "pagefile.sys".into(),
                "swapfile.sys".into(),
            ],
            threads,
        }
    }
}

/// Live counters a UI can poll while a walk runs.
#[derive(Default)]
pub struct Counters {
    pub files: AtomicU64,
    pub dirs: AtomicU64,
    pub bytes: AtomicU64,
    pub errors: AtomicU64,
    pub symlinks: AtomicU64,
    pub current: Mutex<String>,
}

impl Counters {
    pub fn current(&self) -> String {
        self.current.lock().map(|s| s.clone()).unwrap_or_default()
    }
}

struct Ctx<'a> {
    opts: &'a ScanOptions,
    counters: &'a Counters,
    next_dir_id: AtomicI64,
    next_file_id: AtomicI64,
    sink: &'a (dyn Fn(DirBatch) + Sync),
}

/// Walk `root`, calling `sink` once per directory from worker threads. Returns elapsed ms.
pub fn walk(
    root: &Path,
    opts: &ScanOptions,
    counters: &Counters,
    sink: &(dyn Fn(DirBatch) + Sync),
) -> Result<u64> {
    let started = Instant::now();
    let root_md = fs::metadata(root)?;
    if !root_md.is_dir() {
        return Err(Error::Other(format!(
            "{} is not a directory",
            root.display()
        )));
    }
    let ctx = Ctx {
        opts,
        counters,
        next_dir_id: AtomicI64::new(1),
        next_file_id: AtomicI64::new(1),
        sink,
    };
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(opts.threads)
        .thread_name(|i| format!("storecat-walk-{i}"))
        .build()
        .map_err(|e| Error::Other(e.to_string()))?;
    let root_mtime = to_unix(root_md.modified().ok());
    counters.dirs.fetch_add(1, Ordering::Relaxed);
    pool.scope(|s| {
        visit(
            s,
            &ctx,
            root.to_path_buf(),
            0,
            None,
            String::new(),
            String::new(),
            root_mtime,
            true,
        )
    });
    Ok(started.elapsed().as_millis() as u64)
}

#[allow(clippy::too_many_arguments)]
fn visit<'s, 'a: 's>(
    s: &rayon::Scope<'s>,
    ctx: &'a Ctx<'a>,
    abs: PathBuf,
    dir_id: i64,
    parent_id: Option<i64>,
    name: String,
    rel_path: String,
    mtime: i64,
    is_root: bool,
) {
    let rd = match fs::read_dir(&abs) {
        Ok(rd) => rd,
        Err(_) => {
            ctx.counters.errors.fetch_add(1, Ordering::Relaxed);
            return;
        }
    };
    if let Ok(mut cur) = ctx.counters.current.lock() {
        cur.clear();
        cur.push_str(&rel_path);
    }

    let mut files: Vec<FileEntry> = Vec::new();
    let mut subdirs: Vec<(PathBuf, String, i64)> = Vec::new();
    let mut bytes: u64 = 0;

    for entry in rd {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => {
                ctx.counters.errors.fetch_add(1, Ordering::Relaxed);
                continue;
            }
        };
        let ft = match entry.file_type() {
            Ok(ft) => ft,
            Err(_) => {
                ctx.counters.errors.fetch_add(1, Ordering::Relaxed);
                continue;
            }
        };
        let fname = entry.file_name().to_string_lossy().into_owned();
        if ft.is_symlink() {
            ctx.counters.symlinks.fetch_add(1, Ordering::Relaxed);
            continue;
        }
        if ft.is_dir() {
            if ctx.opts.exclude_names.iter().any(|n| n == &fname)
                || (is_root && ctx.opts.exclude_root_names.iter().any(|n| n == &fname))
            {
                continue;
            }
            let md_mtime = entry
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .map(|t| to_unix(Some(t)))
                .unwrap_or(0);
            subdirs.push((entry.path(), fname, md_mtime));
            continue;
        }
        if is_root && ctx.opts.exclude_root_names.iter().any(|n| n == &fname) {
            continue;
        }
        let md = match entry.metadata() {
            Ok(m) => m,
            Err(_) => {
                ctx.counters.errors.fetch_add(1, Ordering::Relaxed);
                continue;
            }
        };
        let size = md.len();
        bytes += size;
        let ext = split_ext(&fname);
        let kind = kind_for_file(&ext, size);
        files.push(FileEntry {
            id: 0,
            name: fname,
            ext,
            size,
            mtime: to_unix(md.modified().ok()),
            ctime: to_unix(md.created().ok()),
            attrs: attrs_of(&md),
            kind,
        });
    }

    if !files.is_empty() {
        let base = ctx
            .next_file_id
            .fetch_add(files.len() as i64, Ordering::Relaxed);
        for (i, f) in files.iter_mut().enumerate() {
            f.id = base + i as i64;
        }
        ctx.counters
            .files
            .fetch_add(files.len() as u64, Ordering::Relaxed);
        ctx.counters.bytes.fetch_add(bytes, Ordering::Relaxed);
    }

    (ctx.sink)(DirBatch {
        dir_id,
        parent_id,
        name,
        rel_path: rel_path.clone(),
        mtime,
        files,
    });

    if !subdirs.is_empty() {
        ctx.counters
            .dirs
            .fetch_add(subdirs.len() as u64, Ordering::Relaxed);
        for (path, sub_name, sub_mtime) in subdirs {
            let id = ctx.next_dir_id.fetch_add(1, Ordering::Relaxed);
            let sub_rel = if rel_path.is_empty() {
                sub_name.clone()
            } else {
                format!("{rel_path}/{sub_name}")
            };
            s.spawn(move |s| {
                visit(
                    s,
                    ctx,
                    path,
                    id,
                    Some(dir_id),
                    sub_name,
                    sub_rel,
                    sub_mtime,
                    false,
                )
            });
        }
    }
}

fn to_unix(t: Option<SystemTime>) -> i64 {
    t.and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(windows)]
fn attrs_of(md: &fs::Metadata) -> u32 {
    use std::os::windows::fs::MetadataExt;
    md.file_attributes()
}

#[cfg(unix)]
fn attrs_of(md: &fs::Metadata) -> u32 {
    use std::os::unix::fs::MetadataExt;
    md.mode()
}

#[cfg(not(any(windows, unix)))]
fn attrs_of(_md: &fs::Metadata) -> u32 {
    0
}

/// Windows `FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS` | `FILE_ATTRIBUTE_OFFLINE`: the file is a
/// cloud placeholder whose bytes are not on disk. Phase 2 uses this to avoid hydrating.
pub fn is_placeholder(attrs: u32) -> bool {
    if cfg!(windows) {
        attrs & 0x0040_0000 != 0 || attrs & 0x1000 != 0
    } else {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[test]
    fn walks_a_small_tree() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::create_dir_all(root.join("a/b")).unwrap();
        fs::create_dir_all(root.join("node_modules/x")).unwrap();
        fs::write(root.join("top.txt"), b"hello").unwrap();
        fs::write(root.join("a/photo.JPG"), vec![0u8; 100]).unwrap();
        fs::write(root.join("a/b/clip.mov"), vec![0u8; 10]).unwrap();
        fs::write(root.join("node_modules/x/skip.js"), b"no").unwrap();

        let counters = Counters::default();
        let batches = Mutex::new(Vec::new());
        let sink = |b: DirBatch| batches.lock().unwrap().push(b);
        let opts = ScanOptions {
            threads: 2,
            ..Default::default()
        };
        walk(root, &opts, &counters, &sink).unwrap();

        let batches = batches.into_inner().unwrap();
        assert_eq!(counters.files.load(Ordering::Relaxed), 3);
        assert_eq!(counters.dirs.load(Ordering::Relaxed), 3); // root, a, a/b
        assert_eq!(counters.bytes.load(Ordering::Relaxed), 115);
        let files: Vec<&FileEntry> = batches.iter().flat_map(|b| b.files.iter()).collect();
        let mut ids: Vec<i64> = files.iter().map(|f| f.id).collect();
        ids.sort();
        assert_eq!(ids, vec![1, 2, 3]);
        let photo = files.iter().find(|f| f.name == "photo.JPG").unwrap();
        assert_eq!(photo.ext, "jpg");
        assert_eq!(photo.kind, Kind::Image);
        let b = batches.iter().find(|b| b.rel_path == "a/b").unwrap();
        assert_eq!(b.name, "b");
        assert!(b.parent_id.is_some());
    }
}
