//! Thumbnails as small JPEGs. Long edge `max_edge` (256 by default), quality 82: about
//! 8–15 KB each, decodable anywhere, and fine for a result list and a detail pane.

use std::fs::File;
use std::io::{BufRead, BufReader, Cursor, Read, Seek, SeekFrom};
use std::path::Path;

use image::imageops::FilterType;
use image::{DynamicImage, ImageFormat};

use crate::guarded;

pub const DEFAULT_MAX_EDGE: u32 = 256;
pub const JPEG_QUALITY: u8 = 82;

#[derive(Debug, Clone)]
pub struct Thumb {
    pub jpeg: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

const RAW_EXTS: &[&str] = &[
    "dng", "cr2", "nef", "arw", "orf", "pef", "srw", "rw2", "raf", "cr3",
];
const IMAGE_EXTS: &[&str] = &[
    "jpg", "jpeg", "png", "gif", "webp", "bmp", "tif", "tiff", "ico",
];
/// Images above this are decoded via their embedded EXIF preview when one exists.
const BIG_IMAGE_BYTES: u64 = 40 * 1024 * 1024;

/// Whether Storecat can make a thumbnail for this extension (RAW files via their preview).
pub fn is_thumbnailable(ext: &str) -> bool {
    RAW_EXTS.contains(&ext) || IMAGE_EXTS.contains(&ext)
}

/// RAW camera files: only their embedded preview is used, never a full decode.
pub fn is_raw(ext: &str) -> bool {
    RAW_EXTS.contains(&ext)
}

/// Thumbnail for an image file. Returns `None` for unsupported formats or decode errors.
pub fn make_thumbnail(path: &Path, ext: &str, max_edge: u32) -> Option<Thumb> {
    if RAW_EXTS.contains(&ext) {
        return embedded_preview(path, max_edge);
    }
    if !IMAGE_EXTS.contains(&ext) {
        return None;
    }
    let size = std::fs::metadata(path).ok()?.len();
    if size > BIG_IMAGE_BYTES {
        if let Some(t) = embedded_preview(path, max_edge) {
            return Some(t);
        }
    }
    let bytes = std::fs::read(path).ok()?;
    thumbnail_from_bytes(path, &bytes, ext, max_edge)
}

/// Thumbnail from an image already read into memory. `path` only labels log lines.
pub fn thumbnail_from_bytes(path: &Path, bytes: &[u8], ext: &str, max_edge: u32) -> Option<Thumb> {
    let raw = RAW_EXTS.contains(&ext);
    if !raw && !IMAGE_EXTS.contains(&ext) {
        return None;
    }
    guarded("thumb", path, move || {
        if raw || bytes.len() as u64 > BIG_IMAGE_BYTES {
            let preview = preview_from(&mut Cursor::new(bytes), max_edge);
            if raw || preview.is_some() {
                return preview;
            }
        }
        let mut reader = image::ImageReader::new(Cursor::new(bytes))
            .with_guessed_format()
            .ok()?;
        let mut limits = image::Limits::default();
        limits.max_image_width = Some(30_000);
        limits.max_image_height = Some(30_000);
        limits.max_alloc = Some(1024 * 1024 * 1024);
        reader.limits(limits);
        let img = reader.decode().ok()?;
        let orientation = exif::Reader::new()
            .read_from_container(&mut Cursor::new(bytes))
            .ok()
            .and_then(|e| orientation_of(&e))
            .unwrap_or(1);
        Some(encode(apply_orientation(img, orientation), max_edge))
    })
}

/// Thumbnail from the JPEG preview a camera or editor embedded in the file's EXIF data.
/// Reads the metadata block and the preview, not the image: on a hard drive that is a
/// few dozen kilobytes instead of the whole photo. Usually 160 px wide, so slightly soft.
pub fn embedded_preview(path: &Path, max_edge: u32) -> Option<Thumb> {
    guarded("thumb", path, move || {
        let mut f = File::open(path).ok()?;
        // TIFF-based RAWs keep their directories near the start; parse a prefix before
        // falling back to reading the whole file (kamadak-exif's default for TIFF).
        let mut head = Vec::with_capacity(TIFF_PREFIX);
        (&mut f)
            .take(TIFF_PREFIX as u64)
            .read_to_end(&mut head)
            .ok()?;
        if is_tiff(&head) {
            if let Ok(exif) = exif::Reader::new().read_raw(head) {
                if let Some(t) = preview_of(&exif, &mut f, max_edge) {
                    return Some(t);
                }
            }
        }
        f.seek(SeekFrom::Start(0)).ok()?;
        preview_from(&mut BufReader::new(f), max_edge)
    })
}

const TIFF_PREFIX: usize = 1024 * 1024;

fn is_tiff(b: &[u8]) -> bool {
    b.starts_with(b"II*\0") || b.starts_with(b"MM\0*")
}

fn preview_from<R: BufRead + Seek>(r: &mut R, max_edge: u32) -> Option<Thumb> {
    let exif = exif::Reader::new().read_from_container(r).ok()?;
    preview_of(&exif, r, max_edge)
}

/// The EXIF IFD1 JPEG. Its offset is relative to the TIFF header, which is the file start
/// for TIFF/RAW; some RAWs point outside the parsed block, so read from `file` then.
fn preview_of<R: Read + Seek>(exif: &exif::Exif, file: &mut R, max_edge: u32) -> Option<Thumb> {
    let off = exif
        .get_field(exif::Tag::JPEGInterchangeFormat, exif::In::THUMBNAIL)?
        .value
        .get_uint(0)? as usize;
    let len = exif
        .get_field(exif::Tag::JPEGInterchangeFormatLength, exif::In::THUMBNAIL)?
        .value
        .get_uint(0)? as usize;
    if len == 0 || len > 32 * 1024 * 1024 {
        return None;
    }
    let buf = exif.buf();
    let bytes: Vec<u8> = if off + len <= buf.len() {
        buf[off..off + len].to_vec()
    } else {
        let mut v = vec![0u8; len];
        file.seek(SeekFrom::Start(off as u64)).ok()?;
        file.read_exact(&mut v).ok()?;
        v
    };
    let img = image::load_from_memory_with_format(&bytes, ImageFormat::Jpeg).ok()?;
    Some(encode(
        apply_orientation(img, orientation_of(exif).unwrap_or(1)),
        max_edge,
    ))
}

fn orientation_of(exif: &exif::Exif) -> Option<u8> {
    exif.get_field(exif::Tag::Orientation, exif::In::PRIMARY)
        .and_then(|f| f.value.get_uint(0))
        .map(|v| v as u8)
}

/// Shrink and encode. Uses the fast `thumbnail` path (box filter) for big reductions and a
/// triangle filter when the source is already small, which keeps edges crisp.
pub fn encode(img: DynamicImage, max_edge: u32) -> Thumb {
    let (w, h) = (img.width(), img.height());
    let small = if w.max(h) > max_edge * 3 {
        img.thumbnail(max_edge, max_edge)
    } else if w.max(h) > max_edge {
        img.resize(max_edge, max_edge, FilterType::Triangle)
    } else {
        img
    };
    let rgb = small.to_rgb8();
    let mut out = Cursor::new(Vec::with_capacity(16 * 1024));
    let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, JPEG_QUALITY);
    let _ = enc.encode_image(&rgb);
    Thumb {
        jpeg: out.into_inner(),
        width: rgb.width(),
        height: rgb.height(),
    }
}

fn apply_orientation(img: DynamicImage, o: u8) -> DynamicImage {
    match o {
        2 => img.fliph(),
        3 => img.rotate180(),
        4 => img.flipv(),
        5 => img.rotate90().fliph(),
        6 => img.rotate90(),
        7 => img.rotate270().fliph(),
        8 => img.rotate270(),
        _ => img,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thumbnails_a_png() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("big.png");
        let img =
            image::RgbImage::from_fn(1200, 800, |x, _| image::Rgb([(x % 255) as u8, 90, 160]));
        img.save(&p).unwrap();
        let t = make_thumbnail(&p, "png", 256).unwrap();
        assert_eq!(t.width, 256);
        assert!((170..=171).contains(&t.height));
        assert!(t.jpeg.len() > 500 && t.jpeg.len() < 60_000);
        assert!(make_thumbnail(&p, "txt", 256).is_none());
    }

    fn jpeg_of(w: u32, h: u32) -> Vec<u8> {
        let img = image::RgbImage::from_fn(w, h, |x, y| {
            image::Rgb([(x % 255) as u8, (y % 255) as u8, 40])
        });
        let mut out = Cursor::new(Vec::new());
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 80)
            .encode_image(&img)
            .unwrap();
        out.into_inner()
    }

    /// A 640×480 JPEG carrying a 160×120 EXIF preview (orientation 6: rotate 90°).
    fn jpeg_with_preview() -> Vec<u8> {
        let preview = jpeg_of(160, 120);
        let mut tiff = b"II*\0".to_vec();
        tiff.extend_from_slice(&8u32.to_le_bytes());
        // IFD0: Orientation = 6, then the offset of IFD1.
        tiff.extend_from_slice(&1u16.to_le_bytes());
        tiff.extend_from_slice(&0x0112u16.to_le_bytes());
        tiff.extend_from_slice(&3u16.to_le_bytes());
        tiff.extend_from_slice(&1u32.to_le_bytes());
        tiff.extend_from_slice(&6u32.to_le_bytes());
        tiff.extend_from_slice(&26u32.to_le_bytes());
        // IFD1: where the preview JPEG is and how long.
        tiff.extend_from_slice(&2u16.to_le_bytes());
        for (tag, value) in [(0x0201u16, 56u32), (0x0202, preview.len() as u32)] {
            tiff.extend_from_slice(&tag.to_le_bytes());
            tiff.extend_from_slice(&4u16.to_le_bytes());
            tiff.extend_from_slice(&1u32.to_le_bytes());
            tiff.extend_from_slice(&value.to_le_bytes());
        }
        tiff.extend_from_slice(&0u32.to_le_bytes());
        assert_eq!(tiff.len(), 56);
        tiff.extend_from_slice(&preview);
        let mut app1 = vec![0xFF, 0xE1];
        app1.extend_from_slice(&((tiff.len() + 8) as u16).to_be_bytes());
        app1.extend_from_slice(b"Exif\0\0");
        app1.extend_from_slice(&tiff);
        let main = jpeg_of(640, 480);
        let mut out = main[..2].to_vec();
        out.extend_from_slice(&app1);
        out.extend_from_slice(&main[2..]);
        out
    }

    #[test]
    fn embedded_preview_reads_only_the_preview() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("camera.jpg");
        std::fs::write(&p, jpeg_with_preview()).unwrap();
        // The preview, rotated: 120×160, not scaled up to 256.
        let t = embedded_preview(&p, 256).unwrap();
        assert_eq!((t.width, t.height), (120, 160));
        // The full photo, decoded from memory, rotated the same way.
        let bytes = std::fs::read(&p).unwrap();
        let full = thumbnail_from_bytes(&p, &bytes, "jpg", 256).unwrap();
        assert_eq!((full.width, full.height), (192, 256));
        // No preview: nothing.
        let plain = tmp.path().join("plain.jpg");
        std::fs::write(&plain, jpeg_of(64, 64)).unwrap();
        assert!(embedded_preview(&plain, 256).is_none());
    }
}
