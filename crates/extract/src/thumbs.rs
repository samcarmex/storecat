//! Thumbnails as small JPEGs. Long edge `max_edge` (256 by default), quality 82: about
//! 8–15 KB each, decodable anywhere, and fine for a result list and a detail pane.

use std::fs::File;
use std::io::{BufReader, Cursor, Read, Seek, SeekFrom};
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

/// Thumbnail for an image file. Returns `None` for unsupported formats or decode errors.
pub fn make_thumbnail(path: &Path, ext: &str, max_edge: u32) -> Option<Thumb> {
    let p = path.to_path_buf();
    let e = ext.to_string();
    guarded("thumb", move || {
        if RAW_EXTS.contains(&e.as_str()) {
            return raw_thumbnail(&p, max_edge);
        }
        if !IMAGE_EXTS.contains(&e.as_str()) {
            return None;
        }
        let size = std::fs::metadata(&p).ok()?.len();
        if size > BIG_IMAGE_BYTES {
            if let Some(t) = raw_thumbnail(&p, max_edge) {
                return Some(t);
            }
        }
        let reader = image::ImageReader::open(&p)
            .ok()?
            .with_guessed_format()
            .ok()?;
        let mut limits = image::Limits::default();
        limits.max_image_width = Some(30_000);
        limits.max_image_height = Some(30_000);
        limits.max_alloc = Some(1024 * 1024 * 1024);
        let mut reader = reader;
        reader.limits(limits);
        let img = reader.decode().ok()?;
        let img = apply_orientation(img, exif_orientation(&p).unwrap_or(1));
        Some(encode(img, max_edge))
    })
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

fn exif_orientation(path: &Path) -> Option<u8> {
    let f = File::open(path).ok()?;
    let exif = exif::Reader::new()
        .read_from_container(&mut BufReader::new(f))
        .ok()?;
    exif.get_field(exif::Tag::Orientation, exif::In::PRIMARY)
        .and_then(|f| f.value.get_uint(0))
        .map(|v| v as u8)
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

/// The JPEG preview embedded in a RAW (or very large JPEG/TIFF) via EXIF IFD1.
fn raw_thumbnail(path: &Path, max_edge: u32) -> Option<Thumb> {
    let mut f = File::open(path).ok()?;
    let exif = exif::Reader::new()
        .read_from_container(&mut BufReader::new(&mut f))
        .ok()?;
    let off = exif
        .get_field(exif::Tag::JPEGInterchangeFormat, exif::In::THUMBNAIL)?
        .value
        .get_uint(0)? as usize;
    let len = exif
        .get_field(exif::Tag::JPEGInterchangeFormatLength, exif::In::THUMBNAIL)?
        .value
        .get_uint(0)? as usize;
    let buf = exif.buf();
    let bytes: Vec<u8> = if off + len <= buf.len() {
        buf[off..off + len].to_vec()
    } else {
        // Some RAWs store previews outside the TIFF header buffer: offsets are then
        // relative to file start. Try that before giving up.
        let mut v = vec![0u8; len];
        f.seek(SeekFrom::Start(off as u64)).ok()?;
        f.read_exact(&mut v).ok()?;
        v
    };
    let img = image::load_from_memory_with_format(&bytes, ImageFormat::Jpeg).ok()?;
    let orientation = exif
        .get_field(exif::Tag::Orientation, exif::In::PRIMARY)
        .and_then(|f| f.value.get_uint(0))
        .unwrap_or(1) as u8;
    Some(encode(apply_orientation(img, orientation), max_edge))
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
}
