//! Processing one file: first everything that reads the disk, then everything that only
//! needs the CPU. On a spinning drive the reading part takes turns in the planned order
//! (see [`Turn`]), while decoding carries on in parallel.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Condvar, Mutex};

use crate::catalog::join_abs;
use crate::db::volume::{
    EnrichRow, FileRecord, STAGE_ARCHIVE, STAGE_HASH, STAGE_META, STAGE_TEXT, STAGE_THUMB,
};
use crate::tools::FfmpegPaths;

use super::plan::*;
use super::{quick_hash, EnrichOptions};

/// Photos above this are only thumbnailed from their embedded preview, never loaded whole.
const MAX_IMAGE_READ: u64 = 128 * 1024 * 1024;

pub(super) struct Ctx<'a> {
    pub root: &'a str,
    pub opts: &'a EnrichOptions,
    pub ffmpeg: Option<&'a FfmpegPaths>,
    /// Spinning disk: photos use their embedded preview, and reads take turns.
    pub spinning: bool,
    pub turn: Option<&'a Turn>,
}

/// One file handed to a worker. `seq` is its place in the read order.
pub(super) struct Item {
    pub seq: u64,
    pub file: FileRecord,
    /// Everything the file needs, across both passes.
    pub work: u16,
    /// 1 = quick pass, 2 = detail pass.
    pub pass: u8,
    /// Stages already recorded (detail pass: what the quick pass did).
    pub prior_stages: u32,
    /// Detail pass: the metadata the quick pass stored (video duration, index text).
    pub prior_meta: Option<String>,
    /// Document text found by an earlier run, for re-indexing an artwork-only job.
    pub prior_text: Option<String>,
}

/// A worker's answer for one file.
pub(super) struct Outcome {
    pub file: FileRecord,
    pub row: EnrichRow,
    /// Quick pass: whether the detail pass still has work for this file.
    pub needs_detail: bool,
    /// Metadata to index the file with (new, or the quick pass's).
    pub meta_json: Option<String>,
    /// Text found by an earlier run, when this run didn't read any.
    pub prior_text: Option<String>,
    /// Everything recorded for the file once this outcome is written.
    pub stages_all: u32,
}

/// Files this small are read a few at a time on spinning disks: each costs a seek and the
/// time to open it (antivirus scans included) rather than transfer time, and with several
/// requests waiting the drive can serve them in the order its heads pass them.
pub(super) const SMALL_READ: u64 = 64 * 1024;
/// How many small files may be read at once.
const SMALL_WINDOW: u64 = 4;

/// Hands out disk access in `seq` order. A large file is read alone; small files may
/// overlap, up to [`SMALL_WINDOW`] consecutive ones. Workers pull files in `seq` order, so
/// whoever holds the next number is never waiting on anything else.
pub(super) struct Turn {
    state: Mutex<TurnState>,
    cv: Condvar,
}

struct TurnState {
    /// The next number allowed to start reading.
    next: u64,
    /// The lowest number still reading (or not yet started).
    low: u64,
    /// Numbers above `low` that already finished.
    finished: BTreeSet<u64>,
    /// A large file is being read.
    large: bool,
}

impl Turn {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(TurnState {
                next: 0,
                low: 0,
                finished: BTreeSet::new(),
                large: false,
            }),
            cv: Condvar::new(),
        }
    }

    /// The next turn to be handed out; a new pass numbers its files from here.
    pub fn position(&self) -> u64 {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).next
    }

    fn wait(&self, seq: u64, small: bool) -> TurnGuard<'_> {
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            let ready = st.next == seq
                && !st.large
                && if small {
                    seq < st.low + SMALL_WINDOW
                } else {
                    seq == st.low
                };
            if ready {
                break;
            }
            st = self.cv.wait(st).unwrap_or_else(|e| e.into_inner());
        }
        st.next = seq + 1;
        st.large = !small;
        drop(st);
        // Others may start alongside a small file.
        self.cv.notify_all();
        TurnGuard {
            turn: self,
            seq,
            small,
        }
    }
}

pub(super) struct TurnGuard<'a> {
    turn: &'a Turn,
    seq: u64,
    small: bool,
}

impl Drop for TurnGuard<'_> {
    fn drop(&mut self) {
        let mut st = self.turn.state.lock().unwrap_or_else(|e| e.into_inner());
        if !self.small {
            st.large = false;
        }
        if self.seq == st.low {
            st.low += 1;
            loop {
                let low = st.low;
                if !st.finished.remove(&low) {
                    break;
                }
                st.low += 1;
            }
        } else {
            st.finished.insert(self.seq);
        }
        drop(st);
        self.turn.cv.notify_all();
    }
}

/// What the reading half hands the decoding half.
#[derive(Default)]
struct Read {
    image: Option<Vec<u8>>,
    /// Album art or a document's saved preview (JPEG/PNG bytes).
    art: Option<Vec<u8>>,
    pdf: Option<Vec<u8>>,
}

pub(super) fn process(ctx: &Ctx, item: Item) -> Outcome {
    let f = &item.file;
    let mut row = EnrichRow {
        file_id: f.id,
        ..Default::default()
    };
    let path = PathBuf::from(join_abs(ctx.root, &f.path, &f.name));
    let ext = f.ext.as_str();
    let w = if item.pass == 1 {
        item.work & QUICK
    } else {
        item.work & DETAIL
    };
    let mut meta: Option<storecat_extract::FileMeta> = None;
    let mut read = Read::default();

    // ---- Reading: everything that touches the disk ----
    {
        let _turn = ctx.turn.map(|t| t.wait(item.seq, f.size <= SMALL_READ));
        if w & W_HASH != 0 {
            match quick_hash(&path, f.size) {
                Ok(h) => {
                    row.quick_hash = Some(h);
                    row.stages |= STAGE_HASH;
                }
                Err(e) => row.error = Some(format!("hash: {e}")),
            }
        }
        if w & W_META != 0 {
            meta = storecat_extract::extract_meta(&path, ext);
        }
        if w & W_THUMB != 0 {
            if storecat_extract::thumbs::is_thumbnailable(ext) {
                read.image = read_image(ctx, &path, ext, f.size, &mut row);
            } else {
                read.art = storecat_extract::embedded_art(&path, ext);
            }
        }
        if w & W_ARCHIVE != 0 {
            if let Some(entries) = storecat_extract::list_archive(
                &path,
                ext,
                storecat_extract::archives::DEFAULT_MAX_ENTRIES,
            ) {
                row.entries = Some(entries);
                row.stages |= STAGE_ARCHIVE;
            }
        }
        if w & (W_PDF_META | W_TEXT) != 0 && ext == "pdf" {
            if f.size <= storecat_extract::pdf::PDF_MAX_BYTES {
                read.pdf = std::fs::read(&path).ok();
            }
        } else if w & W_TEXT != 0 {
            // Other formats read only what they need (a zip's directory, a text prefix)
            // and cost little to decode, so they run here.
            if let Some(t) = storecat_extract::extract_text(&path, ext, ctx.opts.text_max_bytes) {
                row.text = Some(t);
                row.stages |= STAGE_TEXT;
            }
        }
    }

    // ---- Decoding: CPU only (or FFmpeg, which reads a little on its own) ----
    if let Some(bytes) = read.image {
        if let Some(t) =
            storecat_extract::thumbnail_from_bytes(&path, &bytes, ext, ctx.opts.thumb_edge)
        {
            row.thumb = Some((t.jpeg, t.width, t.height));
            row.stages |= STAGE_THUMB;
        }
    }
    if let Some(bytes) = read.art {
        if let Some(t) = storecat_extract::art_thumbnail(&path, &bytes, ctx.opts.thumb_edge) {
            row.thumb = Some((t.jpeg, t.width, t.height));
            row.stages |= STAGE_THUMB;
        }
    }
    if let Some(bytes) = read.pdf {
        let (m, t) = storecat_extract::pdf_meta_and_text(
            &path,
            &bytes,
            w & W_PDF_META != 0,
            w & W_TEXT != 0,
            ctx.opts.text_max_bytes,
        );
        if m.is_some() {
            meta = m;
        }
        if let Some(t) = t {
            row.text = Some(t);
            row.stages |= STAGE_TEXT;
        }
    }
    // After the quick pass: is there detail work left? FFmpeg metadata only if the
    // built-in reader came up empty.
    let later = item.work & DETAIL;
    let needs_detail =
        item.pass == 1 && (later & !W_PROBE != 0 || (later & W_PROBE != 0 && meta.is_none()));
    if w & W_PROBE != 0 {
        if let Some(ff) = ctx.ffmpeg {
            meta = storecat_extract::ffmpeg::probe(&ff.ffprobe, &path);
        }
    }
    if let Some(m) = &meta {
        row.meta_json = serde_json::to_string(m).ok();
        row.stages |= STAGE_META;
    }
    if w & W_VIDEO_THUMB != 0 {
        let known = meta.clone().or_else(|| {
            item.prior_meta
                .as_deref()
                .and_then(|j| serde_json::from_str::<storecat_extract::FileMeta>(j).ok())
        });
        if let Some(ff) = ctx.ffmpeg {
            if let Some(t) = storecat_extract::ffmpeg::video_thumbnail(
                &ff.ffmpeg,
                &path,
                known.as_ref().and_then(|m| m.duration_ms),
                ctx.opts.thumb_edge,
            ) {
                row.thumb = Some((t.jpeg, t.width, t.height));
                row.stages |= STAGE_THUMB;
            }
        }
    }
    let meta_json = row.meta_json.clone().or(item.prior_meta);
    let stages_all = row.stages | item.prior_stages;
    Outcome {
        file: item.file,
        row,
        needs_detail,
        meta_json,
        stages_all,
        prior_text: item.prior_text,
    }
}

/// The bytes to thumbnail a photo from, or `None` when the embedded preview already did.
fn read_image(
    ctx: &Ctx,
    path: &Path,
    ext: &str,
    size: u64,
    row: &mut EnrichRow,
) -> Option<Vec<u8>> {
    let raw = storecat_extract::thumbs::is_raw(ext);
    // Spinning disks, RAW files and huge images: the camera's preview, if it left one.
    if ctx.spinning || raw || size > MAX_IMAGE_READ {
        if let Some(t) = storecat_extract::embedded_preview(path, ctx.opts.thumb_edge) {
            row.thumb = Some((t.jpeg, t.width, t.height));
            row.stages |= STAGE_THUMB;
            return None;
        }
        if raw || size > MAX_IMAGE_READ {
            return None;
        }
    }
    std::fs::read(path).ok()
}

/// What a worker reports when processing a file panicked outside the guarded parsers.
pub(super) fn failed(file: FileRecord, why: &str) -> Outcome {
    Outcome {
        row: EnrichRow {
            file_id: file.id,
            error: Some(why.to_string()),
            ..Default::default()
        },
        file,
        needs_detail: false,
        meta_json: None,
        stages_all: 0,
        prior_text: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

    /// Small files overlap up to the window; a large file waits for every earlier read
    /// and nothing starts beside it.
    #[test]
    fn small_reads_overlap_large_reads_go_alone() {
        let turn = Turn::new();
        let started = AtomicU64::new(0);
        std::thread::scope(|s| {
            let g0 = turn.wait(0, true);
            let g1 = turn.wait(1, true);
            let g2 = turn.wait(2, true);
            let g3 = turn.wait(3, true);
            let t = &turn;
            let st = &started;
            let small4 = s.spawn(move || {
                let g = t.wait(4, true);
                st.store(4, Ordering::SeqCst);
                g
            });
            std::thread::sleep(Duration::from_millis(50));
            assert_eq!(started.load(Ordering::SeqCst), 0, "fifth small read waits");
            drop(g1); // finishing out of order doesn't open the window
            std::thread::sleep(Duration::from_millis(50));
            assert_eq!(started.load(Ordering::SeqCst), 0);
            drop(g0);
            let g4 = small4.join().unwrap();
            assert_eq!(started.load(Ordering::SeqCst), 4);

            let large5 = s.spawn(move || {
                let g = t.wait(5, false);
                st.store(5, Ordering::SeqCst);
                g
            });
            std::thread::sleep(Duration::from_millis(50));
            assert_eq!(
                started.load(Ordering::SeqCst),
                4,
                "large read waits for 2, 3, 4"
            );
            drop(g2);
            drop(g3);
            drop(g4);
            let g5 = large5.join().unwrap();
            assert_eq!(started.load(Ordering::SeqCst), 5);

            let small6 = s.spawn(move || {
                let g = t.wait(6, true);
                st.store(6, Ordering::SeqCst);
                g
            });
            std::thread::sleep(Duration::from_millis(50));
            assert_eq!(
                started.load(Ordering::SeqCst),
                5,
                "nothing starts beside a large read"
            );
            drop(g5);
            drop(small6.join().unwrap());
        });
        assert_eq!(turn.position(), 7);
    }
}
