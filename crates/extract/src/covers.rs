//! Pictures that files already carry: album art in audio tags, the preview Office,
//! LibreOffice and design apps save inside their documents, an e-book's cover, a
//! Photoshop file's thumbnail. Reading one costs a tag block or a zip directory plus one
//! small entry, never a render. PDFs have no such picture (they would need a renderer).

use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::Path;

use crate::guarded;
use crate::thumbs::{encode, Thumb};

/// Embedded pictures larger than this are ignored (they are artwork masters, not previews).
const MAX_ART_BYTES: u64 = 16 * 1024 * 1024;

const AUDIO_EXTS: &[&str] = &[
    "mp3", "flac", "m4a", "m4b", "aac", "ogg", "oga", "opus", "aif", "aiff", "wav", "ape", "wv",
    "mpc", "spx",
];

/// Zip-based formats and where they keep their preview, best first.
const ZIP_PREVIEWS: &[(&[&str], &[&str])] = &[
    (
        &[
            "docx", "docm", "dotx", "xlsx", "xlsm", "xltx", "pptx", "pptm", "potx", "ppsx", "vsdx",
        ],
        &[
            "docProps/thumbnail.jpeg",
            "docProps/thumbnail.jpg",
            "docProps/thumbnail.png",
        ],
    ),
    (
        &["odt", "ods", "odp", "odg", "ott", "ots", "otp", "ora"],
        &["Thumbnails/thumbnail.png"],
    ),
    (&["kra"], &["preview.png"]),
    (&["sketch"], &["previews/preview.png"]),
    (&["procreate"], &["QuickLook/Thumbnail.png"]),
    (&["3mf"], &["Metadata/thumbnail.png"]),
    (
        &["key", "pages", "numbers"],
        &["preview.jpg", "QuickLook/Thumbnail.jpg", "preview-web.jpg"],
    ),
    (&["xd"], &["preview.png"]),
];

/// Whether files with this extension may carry a picture worth showing as their thumbnail.
pub fn has_embedded_art(ext: &str) -> bool {
    AUDIO_EXTS.contains(&ext)
        || ext == "epub"
        || ext == "psd"
        || ext == "psb"
        || ZIP_PREVIEWS.iter().any(|(exts, _)| exts.contains(&ext))
}

/// The embedded picture's encoded bytes (JPEG or PNG), read from the file. Cheap IO, no
/// decoding: pair with [`art_thumbnail`] off the disk-reading path.
pub fn embedded_art(path: &Path, ext: &str) -> Option<Vec<u8>> {
    guarded("artwork", path, move || {
        if AUDIO_EXTS.contains(&ext) {
            audio_art(path)
        } else if ext == "epub" {
            epub_cover(path)
        } else if ext == "psd" || ext == "psb" {
            psd_thumbnail(path)
        } else {
            let names = ZIP_PREVIEWS.iter().find(|(exts, _)| exts.contains(&ext))?.1;
            let mut zip = open_zip(path)?;
            names.iter().find_map(|n| zip_entry(&mut zip, n))
        }
    })
}

/// Thumbnail from bytes returned by [`embedded_art`]. `path` only labels log lines.
pub fn art_thumbnail(path: &Path, bytes: &[u8], max_edge: u32) -> Option<Thumb> {
    guarded("artwork", path, move || {
        let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes))
            .with_guessed_format()
            .ok()?;
        let mut limits = image::Limits::default();
        limits.max_image_width = Some(12_000);
        limits.max_image_height = Some(12_000);
        limits.max_alloc = Some(512 * 1024 * 1024);
        reader.limits(limits);
        Some(encode(reader.decode().ok()?, max_edge))
    })
}

fn audio_art(path: &Path) -> Option<Vec<u8>> {
    use lofty::config::ParseOptions;
    use lofty::file::TaggedFileExt;
    use lofty::picture::PictureType;
    let tagged = lofty::probe::Probe::open(path)
        .ok()?
        .options(ParseOptions::new().read_properties(false))
        .read()
        .ok()?;
    let pictures: Vec<&lofty::picture::Picture> =
        tagged.tags().iter().flat_map(|t| t.pictures()).collect();
    let pick = pictures
        .iter()
        .find(|p| p.pic_type() == PictureType::CoverFront)
        .or_else(|| pictures.first())?;
    let data = pick.data();
    (!data.is_empty() && data.len() as u64 <= MAX_ART_BYTES).then(|| data.to_vec())
}

fn open_zip(path: &Path) -> Option<zip::ZipArchive<BufReader<File>>> {
    zip::ZipArchive::new(BufReader::new(File::open(path).ok()?)).ok()
}

fn zip_entry(zip: &mut zip::ZipArchive<BufReader<File>>, name: &str) -> Option<Vec<u8>> {
    let mut entry = zip.by_name(name).ok()?;
    if entry.size() == 0 || entry.size() > MAX_ART_BYTES {
        return None;
    }
    let mut out = Vec::with_capacity(entry.size() as usize);
    (&mut entry)
        .take(MAX_ART_BYTES)
        .read_to_end(&mut out)
        .ok()?;
    Some(out)
}

/// The cover an EPUB names in its package file, or failing that an image called "cover".
fn epub_cover(path: &Path) -> Option<Vec<u8>> {
    let mut zip = open_zip(path)?;
    let container = String::from_utf8(zip_entry(&mut zip, "META-INF/container.xml")?).ok()?;
    let opf_path = attrs_of(&container, "rootfile")
        .into_iter()
        .find_map(|a| attr(&a, "full-path"))?;
    if let Some(opf) = zip_entry(&mut zip, &opf_path).and_then(|b| String::from_utf8(b).ok()) {
        let items = attrs_of(&opf, "item");
        // EPUB 3: <item properties="cover-image">. EPUB 2: <meta name="cover" content="id">.
        let by_property = items.iter().find(|a| {
            attr(a, "properties")
                .map(|p| p.split_whitespace().any(|w| w == "cover-image"))
                .unwrap_or(false)
        });
        let cover_id = attrs_of(&opf, "meta")
            .into_iter()
            .find(|a| attr(a, "name").as_deref() == Some("cover"))
            .and_then(|a| attr(&a, "content"));
        let by_meta = cover_id.and_then(|id| {
            items
                .iter()
                .find(|a| attr(a, "id").as_deref() == Some(id.as_str()))
        });
        if let Some(href) = by_property.or(by_meta).and_then(|a| attr(a, "href")) {
            let base = opf_path.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
            let full = resolve(base, &percent_decode(&href));
            if let Some(b) = zip_entry(&mut zip, &full) {
                return Some(b);
            }
        }
    }
    let fallback = zip
        .file_names()
        .find(|n| {
            let l = n.to_ascii_lowercase();
            l.contains("cover")
                && (l.ends_with(".jpg") || l.ends_with(".jpeg") || l.ends_with(".png"))
        })?
        .to_string();
    zip_entry(&mut zip, &fallback)
}

/// The raw attribute text of every `<tag ...>` element (namespace prefixes ignored).
fn attrs_of(xml: &str, tag: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = xml;
    while let Some(i) = rest.find('<') {
        rest = &rest[i + 1..];
        let end = rest.find('>').unwrap_or(rest.len());
        let el = &rest[..end];
        let name_end = el
            .find(|c: char| c.is_whitespace() || c == '/')
            .unwrap_or(el.len());
        let name = &el[..name_end];
        let local = name.rsplit(':').next().unwrap_or(name);
        if local == tag {
            out.push(el[name_end..].to_string());
        }
        rest = &rest[end.min(rest.len())..];
    }
    out
}

fn attr(attrs: &str, name: &str) -> Option<String> {
    let mut rest = attrs;
    loop {
        let i = rest.find(name)?;
        let before = rest[..i].chars().last();
        let after = &rest[i + name.len()..];
        let after_trim = after.trim_start();
        if before.map(|c| c.is_whitespace()).unwrap_or(true) && after_trim.starts_with('=') {
            let v = after_trim[1..].trim_start();
            let q = v.chars().next()?;
            if q == '"' || q == '\'' {
                let v = &v[1..];
                return v.find(q).map(|e| v[..e].to_string());
            }
        }
        rest = after;
    }
}

fn resolve(base: &str, href: &str) -> String {
    let mut parts: Vec<&str> = if base.is_empty() {
        Vec::new()
    } else {
        base.split('/').collect()
    };
    for seg in href.split('/') {
        match seg {
            "." | "" => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    parts.join("/")
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Photoshop's own thumbnail: image resource 1036, a JPEG after a 28-byte header.
fn psd_thumbnail(path: &Path) -> Option<Vec<u8>> {
    let mut f = BufReader::new(File::open(path).ok()?);
    let mut header = [0u8; 26];
    f.read_exact(&mut header).ok()?;
    if &header[..4] != b"8BPS" {
        return None;
    }
    let color_len = read_u32(&mut f)?;
    f.seek(SeekFrom::Current(color_len as i64)).ok()?;
    let resources_len = read_u32(&mut f)? as u64;
    let mut left = resources_len;
    while left >= 12 {
        let mut sig = [0u8; 4];
        f.read_exact(&mut sig).ok()?;
        if &sig != b"8BIM" {
            return None;
        }
        let mut id = [0u8; 2];
        f.read_exact(&mut id).ok()?;
        let id = u16::from_be_bytes(id);
        let mut name_len = [0u8; 1];
        f.read_exact(&mut name_len).ok()?;
        // Pascal string, padded so length byte + text is even.
        let name_total = (1 + name_len[0] as u64 + 1) & !1;
        f.seek(SeekFrom::Current(name_total as i64 - 1)).ok()?;
        let size = read_u32(&mut f)? as u64;
        let padded = (size + 1) & !1;
        if id == 1036 && size > 28 && size - 28 <= MAX_ART_BYTES {
            let mut head = [0u8; 28];
            f.read_exact(&mut head).ok()?;
            if u32::from_be_bytes([head[0], head[1], head[2], head[3]]) != 1 {
                return None; // raw RGB, not JPEG
            }
            let mut jpeg = vec![0u8; (size - 28) as usize];
            f.read_exact(&mut jpeg).ok()?;
            return Some(jpeg);
        }
        f.seek(SeekFrom::Current(padded as i64)).ok()?;
        left = left.checked_sub(4 + 2 + name_total + 4 + padded)?;
    }
    None
}

fn read_u32(r: &mut impl Read) -> Option<u32> {
    let mut b = [0u8; 4];
    r.read_exact(&mut b).ok()?;
    Some(u32::from_be_bytes(b))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Write};

    fn jpeg(w: u32, h: u32) -> Vec<u8> {
        let img = image::RgbImage::from_fn(w, h, |x, y| {
            image::Rgb([(x % 255) as u8, (y % 255) as u8, 90])
        });
        let mut out = Cursor::new(Vec::new());
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 80)
            .encode_image(&img)
            .unwrap();
        out.into_inner()
    }

    fn zip_with(path: &Path, entries: &[(&str, &[u8])]) {
        let mut z = zip::ZipWriter::new(File::create(path).unwrap());
        for (name, data) in entries {
            z.start_file(*name, zip::write::SimpleFileOptions::default())
                .unwrap();
            z.write_all(data).unwrap();
        }
        z.finish().unwrap();
    }

    #[test]
    fn office_and_epub_previews() {
        let tmp = tempfile::tempdir().unwrap();
        let pic = jpeg(320, 240);

        let pptx = tmp.path().join("deck.pptx");
        zip_with(
            &pptx,
            &[
                ("[Content_Types].xml", b"<x/>"),
                ("docProps/thumbnail.jpeg", &pic),
            ],
        );
        assert!(has_embedded_art("pptx"));
        let bytes = embedded_art(&pptx, "pptx").unwrap();
        let t = art_thumbnail(&pptx, &bytes, 256).unwrap();
        assert_eq!((t.width, t.height), (256, 192));

        let epub = tmp.path().join("book.epub");
        zip_with(
            &epub,
            &[
                (
                    "META-INF/container.xml",
                    br#"<container><rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles></container>"#,
                ),
                (
                    "OEBPS/content.opf",
                    br#"<package><metadata><meta name="cover" content="c1"/></metadata><manifest><item id="c1" href="images/Front%20Cover.jpg" media-type="image/jpeg"/></manifest></package>"#,
                ),
                ("OEBPS/images/Front Cover.jpg", &pic),
            ],
        );
        assert_eq!(embedded_art(&epub, "epub").unwrap(), pic);

        // A document saved without a preview has none.
        let docx = tmp.path().join("plain.docx");
        zip_with(&docx, &[("word/document.xml", b"<w/>")]);
        assert!(embedded_art(&docx, "docx").is_none());
        assert!(!has_embedded_art("pdf"));
    }

    #[test]
    fn album_art_from_id3() {
        let tmp = tempfile::tempdir().unwrap();
        let pic = jpeg(300, 300);
        let mut apic = vec![0u8];
        apic.extend_from_slice(b"image/jpeg\0");
        apic.push(3); // front cover
        apic.push(0); // empty description
        apic.extend_from_slice(&pic);
        let mut frame = b"APIC".to_vec();
        frame.extend_from_slice(&(apic.len() as u32).to_be_bytes());
        frame.extend_from_slice(&[0, 0]);
        frame.extend_from_slice(&apic);
        let n = frame.len() as u32;
        let syncsafe = [
            (n >> 21) as u8 & 0x7f,
            (n >> 14) as u8 & 0x7f,
            (n >> 7) as u8 & 0x7f,
            n as u8 & 0x7f,
        ];
        let mut mp3 = b"ID3\x03\x00\x00".to_vec();
        mp3.extend_from_slice(&syncsafe);
        mp3.extend_from_slice(&frame);
        for _ in 0..4 {
            mp3.extend_from_slice(&[0xFF, 0xFB, 0x90, 0x00]);
            mp3.extend_from_slice(&[0u8; 413]);
        }
        let p = tmp.path().join("song.mp3");
        std::fs::write(&p, &mp3).unwrap();
        assert_eq!(embedded_art(&p, "mp3").unwrap(), pic);
        let t = art_thumbnail(&p, &pic, 256).unwrap();
        assert_eq!((t.width, t.height), (256, 256));
    }

    #[test]
    fn photoshop_thumbnail() {
        let tmp = tempfile::tempdir().unwrap();
        let pic = jpeg(160, 120);
        let mut psd = b"8BPS".to_vec();
        psd.extend_from_slice(&[0, 1, 0, 0, 0, 0, 0, 0, 0, 3]);
        psd.extend_from_slice(&100u32.to_be_bytes());
        psd.extend_from_slice(&100u32.to_be_bytes());
        psd.extend_from_slice(&[0, 8, 0, 3]);
        psd.extend_from_slice(&0u32.to_be_bytes()); // colour mode data
        let mut res = Vec::new();
        // An unrelated resource first, with an odd size to exercise padding.
        res.extend_from_slice(b"8BIM");
        res.extend_from_slice(&1005u16.to_be_bytes());
        res.extend_from_slice(&[0, 0]);
        res.extend_from_slice(&3u32.to_be_bytes());
        res.extend_from_slice(&[1, 2, 3, 0]);
        res.extend_from_slice(b"8BIM");
        res.extend_from_slice(&1036u16.to_be_bytes());
        res.extend_from_slice(&[0, 0]);
        res.extend_from_slice(&((28 + pic.len()) as u32).to_be_bytes());
        let mut head = vec![0u8; 28];
        head[3] = 1;
        res.extend_from_slice(&head);
        res.extend_from_slice(&pic);
        if res.len() % 2 == 1 {
            res.push(0);
        }
        psd.extend_from_slice(&(res.len() as u32).to_be_bytes());
        psd.extend_from_slice(&res);
        let p = tmp.path().join("art.psd");
        std::fs::write(&p, &psd).unwrap();
        assert_eq!(embedded_art(&p, "psd").unwrap(), pic);
    }
}
