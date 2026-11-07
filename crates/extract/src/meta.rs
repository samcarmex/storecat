//! Type-specific metadata. One flat struct with optional fields keeps the database and the
//! UI simple; `extra` holds anything else worth showing.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::BufReader;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::guarded;

#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct FileMeta {
    // Images and video
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub orientation: Option<u8>,
    // Time-based media
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bitrate_kbps: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub video_codec: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio_codec: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fps: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sample_rate: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channels: Option<u8>,
    // Tags
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artist: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub album: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub year: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub genre: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub track: Option<u32>,
    // Camera
    #[serde(skip_serializing_if = "Option::is_none")]
    pub camera_make: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub camera_model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lens: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub iso: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub f_number: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exposure: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub focal_length_mm: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub taken_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gps: Option<(f64, f64)>,
    // Documents
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pages: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub words: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub software: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub doc_created: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub doc_modified: Option<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, String>,
}

impl FileMeta {
    pub fn is_empty(&self) -> bool {
        self == &FileMeta::default()
    }

    /// Words worth indexing so a search for a camera, artist or author finds the file.
    pub fn index_text(&self) -> String {
        let mut parts: Vec<&str> = Vec::new();
        for s in [
            &self.title,
            &self.artist,
            &self.album,
            &self.genre,
            &self.camera_make,
            &self.camera_model,
            &self.lens,
            &self.author,
            &self.subject,
            &self.video_codec,
            &self.audio_codec,
            &self.software,
        ]
        .into_iter()
        .flatten()
        {
            parts.push(s);
        }
        parts.join(" ")
    }
}

/// Extract metadata for a file, dispatching on its lowercase extension.
pub fn extract_meta(path: &Path, ext: &str) -> Option<FileMeta> {
    let p = path.to_path_buf();
    let e = ext.to_string();
    guarded("meta", path, move || match e.as_str() {
        "jpg" | "jpeg" | "png" | "gif" | "webp" | "bmp" | "tif" | "tiff" | "heic" | "heif"
        | "avif" | "ico" | "dng" | "cr2" | "cr3" | "nef" | "arw" | "raf" | "orf" | "rw2"
        | "pef" | "srw" => image_meta(&p, &e),
        "mp3" | "flac" | "wav" | "aac" | "m4a" | "ogg" | "opus" | "aif" | "aiff" | "wma"
        | "ape" | "mpc" | "wv" => audio_meta(&p),
        "mp4" | "m4v" | "mov" | "3gp" => mp4_meta(&p),
        "mkv" | "webm" | "mka" => matroska_meta(&p),
        "pdf" => pdf_meta(&p),
        "docx" | "xlsx" | "pptx" | "docm" | "xlsm" | "pptm" => ooxml_meta(&p),
        "odt" | "ods" | "odp" | "odg" => odf_meta(&p),
        _ => None,
    })
    .filter(|m| !m.is_empty())
}

fn image_meta(path: &Path, ext: &str) -> Option<FileMeta> {
    let mut m = FileMeta::default();
    // Header-only dimensions for the formats `image` knows.
    if !matches!(
        ext,
        "dng"
            | "cr2"
            | "cr3"
            | "nef"
            | "arw"
            | "raf"
            | "orf"
            | "rw2"
            | "pef"
            | "srw"
            | "heic"
            | "heif"
            | "avif"
    ) {
        if let Ok(r) = image::ImageReader::open(path) {
            if let Ok(r) = r.with_guessed_format() {
                if let Ok((w, h)) = r.into_dimensions() {
                    m.width = Some(w);
                    m.height = Some(h);
                }
            }
        }
    }
    if let Ok(file) = File::open(path) {
        let mut reader = BufReader::new(file);
        if let Ok(exif) = exif::Reader::new().read_from_container(&mut reader) {
            apply_exif(&mut m, &exif);
        }
    }
    Some(m)
}

fn apply_exif(m: &mut FileMeta, exif: &exif::Exif) {
    use exif::{In, Tag, Value};
    let text = |tag: Tag| -> Option<String> {
        exif.get_field(tag, In::PRIMARY).map(|f| {
            f.display_value()
                .to_string()
                .trim_matches('"')
                .trim()
                .to_string()
        })
    };
    let num = |tag: Tag| -> Option<u32> {
        exif.get_field(tag, In::PRIMARY)
            .and_then(|f| f.value.get_uint(0))
    };
    let rational = |tag: Tag| -> Option<f32> {
        exif.get_field(tag, In::PRIMARY)
            .and_then(|f| match &f.value {
                Value::Rational(v) if !v.is_empty() => Some(v[0].to_f32()),
                Value::SRational(v) if !v.is_empty() => Some(v[0].to_f32()),
                _ => None,
            })
    };
    if m.width.is_none() {
        m.width = num(Tag::PixelXDimension).or_else(|| num(Tag::ImageWidth));
        m.height = num(Tag::PixelYDimension).or_else(|| num(Tag::ImageLength));
    }
    m.orientation = num(Tag::Orientation).map(|o| o as u8);
    m.camera_make = text(Tag::Make).filter(|s| !s.is_empty());
    m.camera_model = text(Tag::Model).filter(|s| !s.is_empty());
    m.lens = text(Tag::LensModel).filter(|s| !s.is_empty());
    m.iso = num(Tag::PhotographicSensitivity);
    m.f_number = rational(Tag::FNumber);
    m.focal_length_mm = rational(Tag::FocalLength);
    m.exposure = exif
        .get_field(Tag::ExposureTime, In::PRIMARY)
        .map(|f| f.display_value().to_string());
    m.taken_at = text(Tag::DateTimeOriginal)
        .or_else(|| text(Tag::DateTime))
        .filter(|s| !s.is_empty());
    m.software = text(Tag::Software).filter(|s| !s.is_empty());
    // GPS, decimal degrees.
    let coord = |tag: Tag, ref_tag: Tag| -> Option<f64> {
        let f = exif.get_field(tag, In::PRIMARY)?;
        let Value::Rational(v) = &f.value else {
            return None;
        };
        if v.len() < 3 {
            return None;
        }
        let deg = v[0].to_f64() + v[1].to_f64() / 60.0 + v[2].to_f64() / 3600.0;
        let r = exif
            .get_field(ref_tag, In::PRIMARY)
            .map(|f| f.display_value().to_string())
            .unwrap_or_default();
        Some(if r.starts_with('S') || r.starts_with('W') {
            -deg
        } else {
            deg
        })
    };
    if let (Some(lat), Some(lon)) = (
        coord(Tag::GPSLatitude, Tag::GPSLatitudeRef),
        coord(Tag::GPSLongitude, Tag::GPSLongitudeRef),
    ) {
        m.gps = Some((lat, lon));
    }
}

fn audio_meta(path: &Path) -> Option<FileMeta> {
    use lofty::file::{AudioFile, TaggedFileExt};
    use lofty::tag::{Accessor, ItemKey};
    let tagged = lofty::read_from_path(path).ok()?;
    let mut m = FileMeta::default();
    let props = tagged.properties();
    m.duration_ms = Some(props.duration().as_millis() as u64);
    m.bitrate_kbps = props.audio_bitrate();
    m.sample_rate = props.sample_rate();
    m.channels = props.channels();
    if let Some(tag) = tagged.primary_tag().or_else(|| tagged.first_tag()) {
        m.title = tag.title().map(|s| s.to_string());
        m.artist = tag.artist().map(|s| s.to_string());
        m.album = tag.album().map(|s| s.to_string());
        m.genre = tag.genre().map(|s| s.to_string());
        m.year = tag
            .get_string(ItemKey::Year)
            .or_else(|| tag.get_string(ItemKey::RecordingDate))
            .and_then(|s| s.chars().take(4).collect::<String>().parse().ok());
        m.track = tag.track();
        if let Some(v) = tag.get_string(ItemKey::AlbumArtist) {
            m.extra.insert("album_artist".into(), v.to_string());
        }
    }
    m.audio_codec = Some(format!("{:?}", tagged.file_type()).to_lowercase());
    Some(m)
}

fn mp4_meta(path: &Path) -> Option<FileMeta> {
    let f = File::open(path).ok()?;
    let size = f.metadata().ok()?.len();
    let reader = BufReader::new(f);
    let mp4 = mp4::Mp4Reader::read_header(reader, size).ok()?;
    let mut m = FileMeta::default();
    m.duration_ms = Some(mp4.duration().as_millis() as u64);
    for track in mp4.tracks().values() {
        match track.track_type() {
            Ok(mp4::TrackType::Video) => {
                m.width = Some(track.width() as u32);
                m.height = Some(track.height() as u32);
                m.fps = Some(track.frame_rate() as f32);
                m.video_codec = track
                    .media_type()
                    .ok()
                    .map(|t| t.to_string().to_lowercase());
                m.bitrate_kbps = Some(track.bitrate() / 1000);
            }
            Ok(mp4::TrackType::Audio) => {
                m.audio_codec = track
                    .media_type()
                    .ok()
                    .map(|t| t.to_string().to_lowercase());
                m.sample_rate = track.sample_freq_index().ok().map(|i| i.freq());
                m.channels = track.channel_config().ok().map(|c| match c {
                    mp4::ChannelConfig::Mono => 1,
                    mp4::ChannelConfig::Stereo => 2,
                    mp4::ChannelConfig::Three => 3,
                    mp4::ChannelConfig::Four => 4,
                    mp4::ChannelConfig::Five => 5,
                    mp4::ChannelConfig::FiveOne => 6,
                    mp4::ChannelConfig::SevenOne => 8,
                });
            }
            _ => {}
        }
    }
    if m.bitrate_kbps.is_none() {
        if let Some(d) = m.duration_ms.filter(|d| *d > 0) {
            m.bitrate_kbps = Some(((size * 8) / d.max(1)) as u32);
        }
    }
    Some(m)
}

fn matroska_meta(path: &Path) -> Option<FileMeta> {
    let mkv = matroska::open(path).ok()?;
    let mut m = FileMeta::default();
    m.duration_ms = mkv.info.duration.map(|d| d.as_millis() as u64);
    m.title = mkv.info.title.clone();
    for t in &mkv.tracks {
        match &t.settings {
            matroska::Settings::Video(v) => {
                m.width = Some(v.pixel_width as u32);
                m.height = Some(v.pixel_height as u32);
                m.video_codec = Some(codec_name(&t.codec_id));
            }
            matroska::Settings::Audio(a) => {
                m.audio_codec = Some(codec_name(&t.codec_id));
                m.sample_rate = Some(a.sample_rate as u32);
                m.channels = Some(a.channels as u8);
            }
            _ => {}
        }
    }
    if let (Some(d), Ok(md)) = (m.duration_ms, std::fs::metadata(path)) {
        m.bitrate_kbps = (md.len() * 8).checked_div(d).map(|b| b as u32);
    }
    Some(m)
}

fn codec_name(id: &str) -> String {
    // "V_MPEGH/ISO/HEVC" → "hevc", "A_AAC" → "aac"
    let s = id.rsplit('/').next().unwrap_or(id);
    let s = s
        .trim_start_matches("V_")
        .trim_start_matches("A_")
        .trim_start_matches("S_");
    s.to_lowercase()
}

pub(crate) const PDF_MAX_BYTES: u64 = crate::pdf::PDF_MAX_BYTES;

fn pdf_meta(path: &Path) -> Option<FileMeta> {
    if std::fs::metadata(path).ok()?.len() > PDF_MAX_BYTES {
        return None;
    }
    let doc = lopdf::Document::load(path).ok()?;
    Some(pdf_meta_of(&doc))
}

/// Page count and the Info dictionary of a parsed PDF.
pub(crate) fn pdf_meta_of(doc: &lopdf::Document) -> FileMeta {
    let mut m = FileMeta::default();
    m.pages = Some(doc.get_pages().len() as u32);
    if let Ok(info_ref) = doc.trailer.get(b"Info") {
        if let Ok(info) = info_ref
            .as_reference()
            .and_then(|r| doc.get_dictionary(r))
            .or_else(|_| info_ref.as_dict())
        {
            let get = |key: &[u8]| -> Option<String> {
                info.get(key)
                    .ok()
                    .and_then(|o| pdf_string(doc, o))
                    .filter(|s| !s.trim().is_empty())
            };
            m.title = get(b"Title");
            m.author = get(b"Author");
            m.subject = get(b"Subject");
            m.software = get(b"Producer").or_else(|| get(b"Creator"));
            m.doc_created = get(b"CreationDate").map(pdf_date);
            m.doc_modified = get(b"ModDate").map(pdf_date);
        }
    }
    m
}

fn pdf_string(doc: &lopdf::Document, o: &lopdf::Object) -> Option<String> {
    let o = match o {
        lopdf::Object::Reference(r) => doc.get_object(*r).ok()?,
        other => other,
    };
    match o {
        lopdf::Object::String(bytes, _) => Some(decode_pdf_string(bytes)),
        _ => None,
    }
}

/// PDF text strings are UTF-16BE with a BOM, UTF-8 with a BOM, or PDFDocEncoding
/// (byte per character, Latin-1 for everything that matters here).
fn decode_pdf_string(bytes: &[u8]) -> String {
    if bytes.len() >= 2 && bytes[0] == 0xFE && bytes[1] == 0xFF {
        let units: Vec<u16> = bytes[2..]
            .chunks_exact(2)
            .map(|c| u16::from_be_bytes([c[0], c[1]]))
            .collect();
        return String::from_utf16_lossy(&units);
    }
    if bytes.len() >= 3 && bytes[..3] == [0xEF, 0xBB, 0xBF] {
        return String::from_utf8_lossy(&bytes[3..]).into_owned();
    }
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_string(),
        Err(_) => bytes.iter().map(|&b| b as char).collect(),
    }
}

/// `D:20230414164200+10'00'` → `2023-04-14 16:42`
fn pdf_date(s: String) -> String {
    let d = s.trim_start_matches("D:");
    if d.len() >= 12 && d[..12].chars().all(|c| c.is_ascii_digit()) {
        format!(
            "{}-{}-{} {}:{}",
            &d[..4],
            &d[4..6],
            &d[6..8],
            &d[8..10],
            &d[10..12]
        )
    } else if d.len() >= 8 && d[..8].chars().all(|c| c.is_ascii_digit()) {
        format!("{}-{}-{}", &d[..4], &d[4..6], &d[6..8])
    } else {
        s
    }
}

fn ooxml_meta(path: &Path) -> Option<FileMeta> {
    let file = File::open(path).ok()?;
    let mut zip = zip::ZipArchive::new(BufReader::new(file)).ok()?;
    let mut m = FileMeta::default();
    if let Some(core) = read_zip_text(&mut zip, "docProps/core.xml", 256 * 1024) {
        m.title = xml_text(&core, "dc:title");
        m.author = xml_text(&core, "dc:creator");
        m.subject = xml_text(&core, "dc:subject");
        m.doc_created = xml_text(&core, "dcterms:created");
        m.doc_modified = xml_text(&core, "dcterms:modified");
    }
    if let Some(app) = read_zip_text(&mut zip, "docProps/app.xml", 256 * 1024) {
        m.pages = xml_text(&app, "Pages")
            .or_else(|| xml_text(&app, "Slides"))
            .and_then(|s| s.parse().ok());
        m.words = xml_text(&app, "Words").and_then(|s| s.parse().ok());
        m.software = xml_text(&app, "Application");
    }
    Some(m)
}

fn odf_meta(path: &Path) -> Option<FileMeta> {
    let file = File::open(path).ok()?;
    let mut zip = zip::ZipArchive::new(BufReader::new(file)).ok()?;
    let meta = read_zip_text(&mut zip, "meta.xml", 256 * 1024)?;
    let mut m = FileMeta::default();
    m.title = xml_text(&meta, "dc:title");
    m.author = xml_text(&meta, "meta:initial-creator").or_else(|| xml_text(&meta, "dc:creator"));
    m.subject = xml_text(&meta, "dc:subject");
    m.doc_created = xml_text(&meta, "meta:creation-date");
    m.doc_modified = xml_text(&meta, "dc:date");
    m.software = xml_text(&meta, "meta:generator");
    Some(m)
}

pub(crate) fn read_zip_text<R: std::io::Read + std::io::Seek>(
    zip: &mut zip::ZipArchive<R>,
    name: &str,
    max: u64,
) -> Option<String> {
    use std::io::Read;
    let entry = zip.by_name(name).ok()?;
    let mut buf = Vec::with_capacity(entry.size().min(max) as usize);
    entry.take(max).read_to_end(&mut buf).ok()?;
    Some(String::from_utf8_lossy(&buf).into_owned())
}

/// First text content of `<tag>…</tag>` in a small XML document. Good enough for the
/// property files, which are tiny and regular.
pub(crate) fn xml_text(xml: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}");
    let start = xml.find(&open)?;
    let after = &xml[start + open.len()..];
    let gt = after.find('>')?;
    if after[..gt].ends_with('/') {
        return None;
    }
    let body = &after[gt + 1..];
    let end = body.find(&format!("</{tag}>"))?;
    let v = html_unescape(body[..end].trim());
    if v.is_empty() {
        None
    } else {
        Some(v)
    }
}

pub(crate) fn html_unescape(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&#39;", "'")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xml_text_reads_simple_tags() {
        let xml = r#"<cp:coreProperties><dc:title>Quarterly &amp; Annual</dc:title><dc:creator/></cp:coreProperties>"#;
        assert_eq!(
            xml_text(xml, "dc:title").as_deref(),
            Some("Quarterly & Annual")
        );
        assert_eq!(xml_text(xml, "dc:creator"), None);
    }

    #[test]
    fn pdf_dates() {
        assert_eq!(
            pdf_date("D:20230414164200+10'00'".into()),
            "2023-04-14 16:42"
        );
        assert_eq!(pdf_date("D:20230414".into()), "2023-04-14");
    }

    #[test]
    fn png_dimensions_and_empty_meta() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("x.png");
        let img = image::RgbImage::from_pixel(37, 21, image::Rgb([200, 30, 30]));
        img.save(&p).unwrap();
        let m = extract_meta(&p, "png").unwrap();
        assert_eq!((m.width, m.height), (Some(37), Some(21)));
        assert!(extract_meta(&p, "xyz").is_none());
    }
}
