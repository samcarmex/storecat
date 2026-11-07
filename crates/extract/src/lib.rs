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
pub mod ffmpeg;
pub mod iso;
pub mod meta;
pub mod text;
pub mod thumbs;

pub use archives::{list_archive, ArchiveEntry};
pub use meta::{extract_meta, FileMeta};
pub use text::extract_text;
pub use thumbs::{make_thumbnail, Thumb};

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

/// Run `f`, turning a panic inside a third-party parser into `None`.
pub(crate) fn guarded<T>(
    what: &str,
    f: impl FnOnce() -> Option<T> + std::panic::UnwindSafe,
) -> Option<T> {
    match std::panic::catch_unwind(f) {
        Ok(v) => v,
        Err(_) => {
            log::debug!("{what}: parser panicked");
            None
        }
    }
}
