//! Planning an enrichment pass: decide once, up front, what every unfinished file needs and
//! in which order to read the files, instead of asking the database again for every batch.
//!
//! Work is split into two passes. The quick pass does what makes search and duplicates
//! useful soon (quick hashes, metadata, photo thumbnails, archive listings). The detail
//! pass does what is slow per file (document text, PDF details, video thumbnails).

use std::collections::HashSet;
use std::path::Path;
use std::time::{Duration, Instant};

use crate::db::volume::{PlanRow, VolumeDb};
use crate::error::Result;
use crate::model::Kind;
use crate::scan::is_placeholder;

use super::EnrichOptions;

/// Package, cache and version-control folders, anywhere in a path (case-insensitive).
/// Their files stay catalogued, searchable by name and hashed; they just don't get text
/// or thumbnails, which on a developer's C: drive is hundreds of thousands of files.
const BULK_ANYWHERE: &[&str] = &[
    "node_modules",
    "bower_components",
    "jspm_packages",
    "site-packages",
    "dist-packages",
    "__pycache__",
    ".venv",
    "venv",
    ".tox",
    ".nox",
    ".mypy_cache",
    ".pytest_cache",
    ".ruff_cache",
    ".git",
    ".svn",
    ".hg",
    ".gradle",
    ".m2",
    ".nuget",
    ".cargo",
    ".rustup",
    ".npm",
    ".yarn",
    ".pnpm-store",
    ".cache",
    ".next",
    ".nuxt",
    ".svelte-kit",
    ".parcel-cache",
    ".angular",
    ".terraform",
    ".dart_tool",
    ".pub-cache",
    "$recycle.bin",
    // Caches by any other name, and editor extensions (packages too).
    "cache",
    "caches",
    "npm-cache",
    "pnpm-cache",
    "node-gyp",
    ".vscode",
    ".vscode-server",
    ".cursor",
    ".windsurf",
    ".antigravity",
];

/// A folder inside a particular parent: names too common to skip on their own.
const BULK_PAIRS: &[(&str, &str)] = &[
    // Repository copies that coding agents check out next to the real one.
    (".claude", "worktrees"),
    // The Android SDK (platforms, sources, build tools).
    ("android", "sdk"),
    // Deno's download cache, `%LOCALAPPDATA%\deno`.
    ("local", "deno"),
];

/// Windows folders, at the top of any drive (an old system disk on USB has them too).
const BULK_TOP: &[&str] = &[
    "windows",
    "program files",
    "program files (x86)",
    "programdata",
    "system volume information",
    "recovery",
    "$windows.~bt",
    "$windows.~ws",
    "$winreagent",
    "msocache",
    "perflogs",
];

/// macOS and Linux system folders, only at the top of the system drive: on an external
/// drive a top-level "Library" or "bin" is more likely the user's own.
const BULK_TOP_SYSTEM: &[&str] = &[
    "system", "library", "private", "usr", "lib", "lib32", "lib64", "bin", "sbin", "proc", "sys",
    "dev", "run", "snap", "boot",
];

/// Whether a catalogue folder path (`a/b/c`, relative to the drive or folder root) is
/// inside a package, cache or system folder. `system_root`: the catalogue is the root of
/// the drive the operating system runs from.
pub fn is_bulk_path(path: &str, system_root: bool) -> bool {
    let segments: Vec<&str> = path.split(['/', '\\']).filter(|s| !s.is_empty()).collect();
    let Some(first) = segments.first() else {
        return false;
    };
    let is = |s: &str, list: &[&str]| list.iter().any(|b| s.eq_ignore_ascii_case(b));
    if is(first, BULK_TOP) || (system_root && is(first, BULK_TOP_SYSTEM)) {
        return true;
    }
    segments.iter().any(|s| is(s, BULK_ANYWHERE))
        || segments.windows(2).any(|w| {
            BULK_PAIRS.iter().any(|(parent, child)| {
                w[0].eq_ignore_ascii_case(parent) && w[1].eq_ignore_ascii_case(child)
            })
        })
}

/// Folder ids under package, cache or system folders.
pub(super) fn bulk_dirs(db: &VolumeDb, system_root: bool) -> Result<HashSet<i64>> {
    let mut out = HashSet::new();
    let mut after = i64::MIN;
    loop {
        let dirs = db.dirs_after(after, 20_000)?;
        let Some(last) = dirs.last() else { break };
        after = last.id;
        out.extend(
            dirs.iter()
                .filter(|d| is_bulk_path(&d.path, system_root))
                .map(|d| d.id),
        );
    }
    Ok(out)
}

// Work bits. Quick pass:
pub(super) const W_HASH: u16 = 1;
pub(super) const W_META: u16 = 1 << 1;
pub(super) const W_THUMB: u16 = 1 << 2;
pub(super) const W_ARCHIVE: u16 = 1 << 3;
pub(super) const QUICK: u16 = W_HASH | W_META | W_THUMB | W_ARCHIVE;
// Detail pass:
pub(super) const W_TEXT: u16 = 1 << 4;
pub(super) const W_PDF_META: u16 = 1 << 5;
pub(super) const W_VIDEO_THUMB: u16 = 1 << 6;
/// FFmpeg metadata for audio/video the built-in readers don't understand. Only known to be
/// needed after the quick pass tried those readers.
pub(super) const W_PROBE: u16 = 1 << 7;
pub(super) const DETAIL: u16 = W_TEXT | W_PDF_META | W_VIDEO_THUMB | W_PROBE;

const META_DOC_EXTS: &[&str] = &[
    "docx", "xlsx", "pptx", "docm", "xlsm", "pptm", "odt", "ods", "odp", "odg",
];

/// Smaller than this, a "video" is not worth starting FFmpeg for.
pub(super) const MIN_FFMPEG_BYTES: u64 = 64 * 1024;

/// The facts about one file that decide its work.
pub(super) struct FileFacts<'a> {
    pub ext: &'a str,
    pub kind: Kind,
    pub size: u64,
    pub attrs: u32,
    pub bulk: bool,
}

/// What a file needs, across both passes. W_PROBE is included for audio/video that might
/// need it; the quick pass drops it again when the built-in reader succeeded.
pub(super) fn work_for(f: &FileFacts, opts: &EnrichOptions, ffmpeg: bool) -> u16 {
    if is_placeholder(f.attrs) || f.size == 0 {
        return 0;
    }
    let media = matches!(f.kind, Kind::Image | Kind::Video | Kind::Audio);
    let ffmpeg_worthy = ffmpeg && f.size >= MIN_FFMPEG_BYTES;
    let mut w = 0;
    if opts.hash && f.size >= opts.hash_min_bytes {
        w |= W_HASH;
    }
    if opts.meta {
        if f.ext == "pdf" {
            w |= W_PDF_META;
        } else if media || META_DOC_EXTS.contains(&f.ext) {
            w |= W_META;
        }
        // Short sound effects are real audio; tiny "videos" are almost always something else.
        if (f.kind == Kind::Audio && ffmpeg) || (f.kind == Kind::Video && ffmpeg_worthy) {
            w |= W_PROBE;
        }
    }
    if opts.thumbs && !f.bulk {
        if (f.kind == Kind::Image && storecat_extract::thumbs::is_thumbnailable(f.ext))
            || storecat_extract::covers::has_embedded_art(f.ext)
        {
            // Photos, and the album art or saved preview other files carry.
            w |= W_THUMB;
        }
        if f.kind == Kind::Video && ffmpeg_worthy {
            w |= W_VIDEO_THUMB;
        }
    }
    if opts.text && !media && !f.bulk && storecat_extract::text::has_extractor(f.ext) {
        w |= W_TEXT;
    }
    if opts.archives && (f.kind == Kind::Archive || f.ext == "iso") {
        w |= W_ARCHIVE;
    }
    w
}

/// One file to process, in the order it will be read.
#[derive(Debug, Clone, Copy)]
pub(super) struct Job {
    pub id: i64,
    pub dir_id: i64,
    pub kind: Kind,
    pub size: u64,
    pub work: u16,
    /// Stages already recorded (detail pass: what the quick pass did).
    pub stages: u32,
}

/// Rows the planner settles without reading the file.
pub(super) struct Settled {
    pub id: i64,
    pub done: u8,
    pub stages: u32,
}

/// Split unfinished files into jobs for `pass` and files that can be settled right away.
pub(super) fn build(
    db: &VolumeDb,
    pass: u8,
    opts: &EnrichOptions,
    ffmpeg: bool,
    bulk: &HashSet<i64>,
) -> Result<(Vec<Job>, Vec<Settled>)> {
    use crate::db::volume::{ENRICH_DETAIL_PENDING, ENRICH_DONE, STAGE_SKIPPED_PLACEHOLDER};
    let mut jobs = Vec::new();
    let mut settled = Vec::new();
    db.plan_rows(|r: PlanRow| {
        let facts = FileFacts {
            ext: &r.ext,
            kind: r.kind,
            size: r.size,
            attrs: r.attrs,
            bulk: bulk.contains(&r.dir_id),
        };
        let work = work_for(&facts, opts, ffmpeg);
        let pending_detail = r.done == ENRICH_DETAIL_PENDING;
        if pass == 1 && !pending_detail {
            if work & QUICK != 0 {
                jobs.push(job(&r, work, 0));
            } else if work & DETAIL & !W_PROBE != 0 {
                // Nothing quick to do (a source file, a small PDF): straight to pass two.
                settled.push(Settled {
                    id: r.id,
                    done: ENRICH_DETAIL_PENDING,
                    stages: 0,
                });
            } else {
                let stages = if is_placeholder(r.attrs) {
                    STAGE_SKIPPED_PLACEHOLDER
                } else {
                    0
                };
                settled.push(Settled {
                    id: r.id,
                    done: ENRICH_DONE,
                    stages,
                });
            }
        } else if pass == 2 && pending_detail {
            let mut work = work & DETAIL;
            if r.stages & crate::db::volume::STAGE_META != 0 {
                work &= !W_PROBE;
            }
            if work == 0 {
                settled.push(Settled {
                    id: r.id,
                    done: ENRICH_DONE,
                    stages: 0,
                });
            } else {
                jobs.push(job(&r, work, r.stages));
            }
        }
        Ok(())
    })?;
    Ok((jobs, settled))
}

fn job(r: &PlanRow, work: u16, stages: u32) -> Job {
    Job {
        id: r.id,
        dir_id: r.dir_id,
        kind: r.kind,
        size: r.size,
        work,
        stages,
    }
}

/// Solid-state order. Quick pass: photos first so thumbnails appear early, then folder by
/// folder. Detail pass: documents before video, small before large.
pub(super) fn order_for_ssd(jobs: &mut [Job], pass: u8) {
    if pass == 1 {
        jobs.sort_by_key(|j| (kind_rank(j.kind), j.dir_id, j.id));
    } else {
        jobs.sort_by_key(|j| (kind_rank(j.kind), j.size, j.id));
    }
}

fn kind_rank(k: Kind) -> u8 {
    match k {
        Kind::Image => 0,
        Kind::Document => 1,
        Kind::Video => 2,
        Kind::Design => 3,
        Kind::Audio => 4,
        Kind::Archive => 5,
        Kind::Code => 6,
        _ => 7,
    }
}

/// Spinning-disk order: by where each file's data starts, so the heads sweep across the
/// disk once. Looking that up costs one metadata open per file; if the drive answers too
/// slowly (more than `budget` per file on average) folder order is kept instead.
pub(super) fn order_for_hdd(
    jobs: &mut [Job],
    path_of: &mut dyn FnMut(&Job) -> Option<String>,
    cancel: &dyn Fn() -> bool,
    progress: &mut dyn FnMut(usize),
    budget: Duration,
) -> bool {
    jobs.sort_by_key(|j| (j.dir_id, j.id));
    let started = Instant::now();
    let mut keys: Vec<u64> = Vec::with_capacity(jobs.len());
    for (i, j) in jobs.iter().enumerate() {
        if cancel() {
            return false;
        }
        if i >= 500 && started.elapsed() > budget * i as u32 {
            log::info!(
                "disk positions too slow to look up ({i} in {:?}); using folder order",
                started.elapsed()
            );
            return false;
        }
        // Files without clusters of their own sit in the file table: read them where their
        // folder neighbours are (keep folder order for them by using the previous key).
        let lcn = path_of(j).and_then(|p| super::diskpos::first_cluster(Path::new(&p)));
        let key = lcn.unwrap_or_else(|| keys.last().copied().unwrap_or(0));
        keys.push(key);
        if i % 2000 == 0 {
            progress(i);
        }
    }
    let mut order: Vec<usize> = (0..jobs.len()).collect();
    order.sort_by_key(|&i| (keys[i], i));
    let sorted: Vec<Job> = order.iter().map(|&i| jobs[i]).collect();
    jobs.copy_from_slice(&sorted);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_and_system_folders_are_bulk() {
        let bulk = |p| is_bulk_path(p, false);
        assert!(bulk("Users/sam/dev/app/node_modules/react"));
        assert!(bulk(
            "Users/sam/AppData/Local/Programs/Python/Lib/site-packages/x"
        ));
        assert!(bulk("Windows/System32"));
        assert!(bulk("Program Files (x86)/Steam"));
        assert!(bulk("code/storecat/.git/objects"));
        assert!(bulk("$RECYCLE.BIN/S-1-5-21"));
        assert!(bulk("Users/sam/AppData/Local/uv/cache/archive-v0"));
        assert!(bulk(
            "Users/sam/AppData/Local/Google/Chrome/User Data/Default/Cache"
        ));
        assert!(bulk("Users/sam/.vscode/extensions/rust-lang.rust-analyzer"));
        assert!(bulk("GitHub/printdash/.claude/worktrees/agent-1/src"));
        assert!(bulk("Users/sam/AppData/Local/Android/Sdk/platforms"));
        assert!(bulk("Users/sam/AppData/Local/deno/npm/registry.npmjs.org"));
        // The same names elsewhere are the user's own.
        assert!(!bulk("GitHub/printdash/.claude/commands"));
        assert!(!bulk("Projects/worktrees/notes"));
        assert!(!bulk("Projects/deno/src"));
        assert!(!bulk("Clients/Android/Designs"));
        // AppData itself, the user's own folders, and look-alikes deeper down are kept.
        assert!(!bulk("Users/sam/AppData/Roaming/Notes"));
        assert!(!bulk("Users/sam/Documents/Windows/screenshots"));
        assert!(!bulk("Projects/library/books"));
        assert!(!bulk(""));
        // Unix system folders only count at the top of the system drive.
        assert!(!bulk("Library/Photos"));
        assert!(is_bulk_path("Library/Caches", true));
        assert!(is_bulk_path("usr/share/doc", true));
    }

    fn facts(ext: &str, kind: Kind, size: u64, bulk: bool) -> FileFacts<'_> {
        FileFacts {
            ext,
            kind,
            size,
            attrs: 0,
            bulk,
        }
    }

    #[test]
    fn work_splits_into_quick_and_detail() {
        let o = EnrichOptions::default();
        let photo = work_for(&facts("jpg", Kind::Image, 3_000_000, false), &o, true);
        assert_eq!(photo, W_HASH | W_META | W_THUMB);
        let pdf = work_for(&facts("pdf", Kind::Document, 200_000, false), &o, true);
        assert_eq!(pdf, W_PDF_META | W_TEXT);
        let video = work_for(&facts("mp4", Kind::Video, 50_000_000, false), &o, true);
        assert_eq!(video, W_HASH | W_META | W_PROBE | W_VIDEO_THUMB);
        let no_ffmpeg = work_for(&facts("mp4", Kind::Video, 50_000_000, false), &o, false);
        assert_eq!(no_ffmpeg, W_HASH | W_META);
        // Package folders: hashed and described, but no text or thumbnails.
        let lib = work_for(&facts("py", Kind::Code, 4_000, true), &o, true);
        assert_eq!(lib, 0);
        let lib_img = work_for(&facts("png", Kind::Image, 4_000, true), &o, true);
        assert_eq!(lib_img, W_META);
        // Secrets are never read for text.
        assert_eq!(
            work_for(&facts("env", Kind::Other, 400, false), &o, true),
            0
        );
        // A tiny "video" is not worth FFmpeg.
        let tiny = work_for(&facts("mov", Kind::Video, 900, false), &o, true);
        assert_eq!(tiny, W_META);
        let beep = work_for(&facts("wav", Kind::Audio, 900, false), &o, true);
        assert_eq!(beep, W_META | W_PROBE | W_THUMB);
        // Album art and saved document previews become thumbnails in the quick pass.
        let song = work_for(&facts("mp3", Kind::Audio, 4_000_000, false), &o, true);
        assert_eq!(song & QUICK, W_HASH | W_META | W_THUMB);
        let deck = work_for(&facts("pptx", Kind::Document, 2_000_000, false), &o, true);
        assert_ne!(deck & W_THUMB, 0);
        let deck_in_pkg = work_for(&facts("pptx", Kind::Document, 2_000_000, true), &o, true);
        assert_eq!(deck_in_pkg & W_THUMB, 0);
    }
}
