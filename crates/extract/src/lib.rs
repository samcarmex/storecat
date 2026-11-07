//! Extraction layer: turns a file on disk into catalogue enrichment.
//!
//! - `meta`: EXIF, audio tags and properties, MP4/MKV stream info, PDF and Office properties
//! - `text`: plain text for PDF, Office, ODF, plain text, code and email
//! - `thumbs`: JPEG thumbnails for images, RAW embedded previews and (via FFmpeg) video
//! - `archives`: entry listing for zip, tar and 7z
//! - `ffmpeg`: thin wrappers around the optional `ffprobe` / `ffmpeg` sidecar binaries
//!
//! Every extractor is pure Rust or an optional, separately downloaded sidecar; nothing
//! here links against GPL-incompatible libraries. Everything is best-effort: a function
//! returns `None` rather than failing a scan, and panicking parsers are fenced with
//! `catch_unwind`.

// Metadata extractors fill a `FileMeta::default()` field by field on purpose: each
// format sets a different subset and the flat form reads better than nested initialisers.
#![allow(clippy::field_reassign_with_default)]

pub mod archives;
pub mod covers;
pub mod ffmpeg;
pub mod iso;
pub mod meta;
pub mod pdf;
pub mod text;
pub mod thumbs;

pub use archives::{list_archive, ArchiveEntry};
pub use covers::{art_thumbnail, embedded_art};
pub use meta::{extract_meta, FileMeta};
pub use pdf::pdf_meta_and_text;
pub use text::extract_text;
pub use thumbs::{embedded_preview, make_thumbnail, thumbnail_from_bytes, Thumb};

/// Extractors that this build can run, by name.
pub fn available() -> &'static [&'static str] {
    &[
        "exif",
        "image-dimensions",
        "audio-tags",
        "mp4",
        "matroska",
        "pdf-info",
        "pdf-text",
        "office",
        "odf",
        "plain-text",
        "email",
        "zip",
        "tar",
        "7z",
        "thumb-image",
        "thumb-raw",
        "thumb-video (ffmpeg)",
        "ffprobe",
    ]
}

thread_local! {
    /// What the fenced parser on this thread is reading, while it runs.
    static GUARDED: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

/// The file a fenced parser on the current thread is reading, if one is running.
///
/// A panic hook calls this to tell an unreadable file (caught by [`guarded`] and
/// skipped) from a real crash, so third-party parser panics don't produce crash reports.
pub fn guarded_file() -> Option<String> {
    GUARDED.with(|g| g.borrow().clone())
}

/// Run `f`, turning a panic inside a third-party parser into `None`.
///
/// This relies on panics unwinding. The workspace's release profile must keep the
/// default `panic = "unwind"`: with `abort`, one malformed file ends the whole process.
pub(crate) fn guarded<T>(
    what: &str,
    path: &std::path::Path,
    f: impl FnOnce() -> Option<T> + std::panic::UnwindSafe,
) -> Option<T> {
    let label = format!("{what}: {}", path.display());
    let previous = GUARDED.with(|g| g.replace(Some(label.clone())));
    let result = std::panic::catch_unwind(f);
    GUARDED.with(|g| *g.borrow_mut() = previous);
    match result {
        Ok(v) => v,
        Err(_) => {
            log::warn!("skipped {label}: the parser could not read it");
            None
        }
    }
}
