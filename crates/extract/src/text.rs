//! Text extraction for content search. Output is plain UTF-8, capped in size, with
//! paragraph breaks preserved as newlines where the format has them.

use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;

use crate::guarded;
use crate::meta::{html_unescape, read_zip_text};

pub const DEFAULT_MAX_TEXT: usize = 1024 * 1024;

const PLAIN_EXTS: &[&str] = &[
    "txt", "md", "markdown", "csv", "tsv", "json", "xml", "yaml", "yml", "toml", "ini", "cfg",
    "log", "tex", "rtf", "srt", "vtt", "nfo", "rs", "py", "js", "ts", "tsx", "jsx", "mjs", "cjs",
    "mts", "cts", "svelte", "vue", "css", "scss", "less", "c", "h", "cpp", "hpp", "cc", "cs",
    "java", "kt", "swift", "go", "rb", "php", "sh", "ps1", "bat", "cmd", "sql", "lua", "r", "dart",
    "scala", "zig", "ex", "exs", "erl", "hs", "jl", "pl", "vb", "fs", "gd", "glsl", "hlsl",
];

/// Whether `ext` has a text extractor. `.env` files are deliberately absent: they hold
/// passwords and keys, and catalogues get copied onto drives.
pub fn has_extractor(ext: &str) -> bool {
    matches!(
        ext,
        "pdf"
            | "docx"
            | "docm"
            | "xlsx"
            | "xlsm"
            | "pptx"
            | "pptm"
            | "odt"
            | "ods"
            | "odp"
            | "eml"
            | "html"
            | "htm"
            | "xhtml"
    ) || PLAIN_EXTS.contains(&ext)
}

/// Extract text for a file by extension. `max_bytes` bounds the result (and, for plain
/// files, how much is read).
pub fn extract_text(path: &Path, ext: &str, max_bytes: usize) -> Option<String> {
    let p = path.to_path_buf();
    let e = ext.to_string();
    guarded("text", path, move || {
        let t = match e.as_str() {
            "pdf" => pdf_text(&p, max_bytes),
            "docx" | "docm" => ooxml_text(&p, &["word/document.xml"], "w:t", "</w:p>", max_bytes),
            "xlsx" | "xlsm" => ooxml_text(&p, &["xl/sharedStrings.xml"], "t", "</si>", max_bytes),
            "pptx" | "pptm" => pptx_text(&p, max_bytes),
            "odt" | "ods" | "odp" => odf_text(&p, max_bytes),
            "eml" => eml_text(&p, max_bytes),
            "html" | "htm" | "xhtml" => html_text(&p, max_bytes),
            ext if PLAIN_EXTS.contains(&ext) => plain_text(&p, max_bytes),
            _ => None,
        }?;
        let t = normalise(&t, max_bytes);
        if t.trim().is_empty() {
            None
        } else {
            Some(t)
        }
    })
}

pub(crate) fn normalise(s: &str, max_bytes: usize) -> String {
    let mut out = String::with_capacity(s.len().min(max_bytes));
    let mut last_blank = false;
    for line in s.lines() {
        let line = line.trim_end();
        let blank = line.trim().is_empty();
        if blank && last_blank {
            continue;
        }
        out.push_str(line);
        out.push('\n');
        last_blank = blank;
        if out.len() >= max_bytes {
            break;
        }
    }
    truncate_utf8(out, max_bytes)
}

fn truncate_utf8(mut s: String, max: usize) -> String {
    if s.len() > max {
        let mut cut = max;
        while !s.is_char_boundary(cut) {
            cut -= 1;
        }
        s.truncate(cut);
    }
    s
}

fn plain_text(path: &Path, max_bytes: usize) -> Option<String> {
    let f = File::open(path).ok()?;
    let mut buf = Vec::new();
    BufReader::new(f)
        .take(max_bytes as u64)
        .read_to_end(&mut buf)
        .ok()?;
    if buf.is_empty() {
        return None;
    }
    // Binary sniff: NULs or too many control bytes mean this is not text.
    let sample = &buf[..buf.len().min(8192)];
    let bad = sample
        .iter()
        .filter(|&&b| b == 0 || (b < 0x09) || (b > 0x0d && b < 0x20))
        .count();
    if bad * 50 > sample.len() {
        return None;
    }
    // UTF-16 with BOM (common for Windows exports).
    if buf.len() >= 2 && ((buf[0] == 0xFF && buf[1] == 0xFE) || (buf[0] == 0xFE && buf[1] == 0xFF))
    {
        let le = buf[0] == 0xFF;
        let units: Vec<u16> = buf[2..]
            .chunks_exact(2)
            .map(|c| {
                if le {
                    u16::from_le_bytes([c[0], c[1]])
                } else {
                    u16::from_be_bytes([c[0], c[1]])
                }
            })
            .collect();
        return Some(String::from_utf16_lossy(&units));
    }
    Some(String::from_utf8_lossy(&buf).into_owned())
}

fn html_text(path: &Path, max_bytes: usize) -> Option<String> {
    let raw = plain_text(path, max_bytes.saturating_mul(3))?;
    Some(strip_tags(&raw))
}

/// Remove tags, scripts and styles; decode the common entities.
pub fn strip_tags(html: &str) -> String {
    let mut out = String::with_capacity(html.len() / 2);
    let mut rest = html;
    let lower = html.to_ascii_lowercase();
    let mut pos = 0usize;
    while let Some(i) = rest.find('<') {
        out.push_str(&rest[..i]);
        pos += i;
        let after = &rest[i..];
        let after_lower = &lower[pos..];
        let skip_to = if after_lower.starts_with("<script") {
            after_lower.find("</script>").map(|e| e + "</script>".len())
        } else if after_lower.starts_with("<style") {
            after_lower.find("</style>").map(|e| e + "</style>".len())
        } else if after_lower.starts_with("<!--") {
            after_lower.find("-->").map(|e| e + 3)
        } else {
            after.find('>').map(|e| e + 1)
        };
        let Some(n) = skip_to else { break };
        if after_lower.starts_with("</p")
            || after_lower.starts_with("<br")
            || after_lower.starts_with("</div")
            || after_lower.starts_with("</h")
            || after_lower.starts_with("</li")
            || after_lower.starts_with("</tr")
        {
            out.push('\n');
        } else if !out.ends_with(char::is_whitespace) {
            out.push(' ');
        }
        rest = &after[n..];
        pos += n;
    }
    out.push_str(rest);
    html_unescape(&out)
}

fn pdf_text(path: &Path, max_bytes: usize) -> Option<String> {
    if std::fs::metadata(path).ok()?.len() > crate::pdf::PDF_MAX_BYTES {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    crate::pdf::pdf_meta_and_text(path, &bytes, false, true, max_bytes).1
}

/// Pull the text runs of an Office Open XML part. `tag` is the run element (e.g. `w:t`),
/// `para_end` the closing element that ends a paragraph.
fn ooxml_text(
    path: &Path,
    parts: &[&str],
    tag: &str,
    para_end: &str,
    max_bytes: usize,
) -> Option<String> {
    let f = File::open(path).ok()?;
    let mut zip = zip::ZipArchive::new(BufReader::new(f)).ok()?;
    let mut out = String::new();
    for part in parts {
        if let Some(xml) = read_zip_text(&mut zip, part, (max_bytes as u64) * 8) {
            collect_runs(&xml, tag, para_end, &mut out, max_bytes);
        }
        if out.len() >= max_bytes {
            break;
        }
    }
    Some(out)
}

fn pptx_text(path: &Path, max_bytes: usize) -> Option<String> {
    let f = File::open(path).ok()?;
    let mut zip = zip::ZipArchive::new(BufReader::new(f)).ok()?;
    let mut names: Vec<String> = (0..zip.len())
        .filter_map(|i| zip.by_index(i).ok().map(|e| e.name().to_string()))
        .filter(|n| n.starts_with("ppt/slides/slide") && n.ends_with(".xml"))
        .collect();
    names.sort_by_key(|n| {
        n.trim_start_matches("ppt/slides/slide")
            .trim_end_matches(".xml")
            .parse::<u32>()
            .unwrap_or(0)
    });
    let mut out = String::new();
    for n in names {
        if let Some(xml) = read_zip_text(&mut zip, &n, (max_bytes as u64) * 4) {
            collect_runs(&xml, "a:t", "</a:p>", &mut out, max_bytes);
            out.push('\n');
        }
        if out.len() >= max_bytes {
            break;
        }
    }
    Some(out)
}

fn odf_text(path: &Path, max_bytes: usize) -> Option<String> {
    let f = File::open(path).ok()?;
    let mut zip = zip::ZipArchive::new(BufReader::new(f)).ok()?;
    let xml = read_zip_text(&mut zip, "content.xml", (max_bytes as u64) * 8)?;
    // ODF text lives in text:p / text:h / text:span / table:cell; strip everything.
    let stripped = strip_tags(
        &xml.replace("</text:p>", "</text:p>\n")
            .replace("</text:h>", "</text:h>\n")
            .replace("<text:line-break/>", "\n")
            .replace("<text:tab/>", "\t"),
    );
    Some(truncate_utf8(stripped, max_bytes))
}

/// Scan `xml` for `<tag ...>text</tag>` runs, appending them; emit a newline at paragraph ends.
fn collect_runs(xml: &str, tag: &str, para_end: &str, out: &mut String, max_bytes: usize) {
    let open = format!("<{tag}");
    let close = format!("</{tag}>");
    let mut rest = xml;
    loop {
        if out.len() >= max_bytes {
            return;
        }
        let next_open = rest.find(&open);
        let next_para = rest.find(para_end);
        let open_first = match (next_open, next_para) {
            (None, None) => return,
            (Some(_), None) => true,
            (None, Some(_)) => false,
            (Some(o), Some(p)) => o < p,
        };
        match (next_open, next_para) {
            (Some(o), _) if open_first => {
                let after = &rest[o + open.len()..];
                // Ensure it is exactly this tag (not a prefix of a longer name).
                if !after.starts_with(['>', ' ', '/']) {
                    rest = after;
                    continue;
                }
                let Some(gt) = after.find('>') else { return };
                if after[..gt].ends_with('/') {
                    rest = &after[gt + 1..];
                    continue;
                }
                let body = &after[gt + 1..];
                let Some(end) = body.find(&close) else { return };
                out.push_str(&html_unescape(&body[..end]));
                rest = &body[end + close.len()..];
            }
            (_, Some(p)) => {
                if !out.ends_with('\n') {
                    out.push('\n');
                }
                rest = &rest[p + para_end.len()..];
            }
            (_, None) => return,
        }
    }
}

fn eml_text(path: &Path, max_bytes: usize) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    let msg = mail_parser::MessageParser::default().parse(&bytes)?;
    let mut out = String::new();
    if let Some(s) = msg.subject() {
        out.push_str("Subject: ");
        out.push_str(s);
        out.push('\n');
    }
    if let Some(from) = msg.from() {
        out.push_str("From: ");
        for a in from.iter() {
            if let Some(n) = a.name() {
                out.push_str(n);
                out.push(' ');
            }
            if let Some(e) = a.address() {
                out.push_str(e);
                out.push(' ');
            }
        }
        out.push('\n');
    }
    if let Some(to) = msg.to() {
        out.push_str("To: ");
        for a in to.iter() {
            if let Some(e) = a.address() {
                out.push_str(e);
                out.push(' ');
            }
        }
        out.push('\n');
    }
    if let Some(d) = msg.date() {
        out.push_str(&format!("Date: {}\n", d.to_rfc3339()));
    }
    out.push('\n');
    if let Some(body) = msg.body_text(0) {
        out.push_str(&body);
    } else if let Some(html) = msg.body_html(0) {
        out.push_str(&strip_tags(&html));
    }
    Some(truncate_utf8(out, max_bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A one-page PDF whose font has a ToUnicode CMap with a 3-byte `bfrange`
    /// destination. pdf-extract's CMap parser panics on that ("bad length of
    /// hexstring"), which is what took down 0.9.0 release builds mid-enrichment.
    fn pdf_with_bad_cmap() -> Vec<u8> {
        let cmap = "/CIDInit /ProcSet findresource begin\n12 dict begin\nbegincmap\n\
                    1 begincodespacerange\n<00> <FF>\nendcodespacerange\n\
                    1 beginbfrange\n<41> <41> <004100>\nendbfrange\nendcmap\nend\nend\n";
        let content = "BT /F1 12 Tf 10 10 Td (A) Tj ET";
        let objects = [
            "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
             /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >>"
                .to_string(),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /ToUnicode 6 0 R >>".to_string(),
            format!(
                "<< /Length {} >>\nstream\n{content}\nendstream",
                content.len()
            ),
            format!("<< /Length {} >>\nstream\n{cmap}\nendstream", cmap.len()),
        ];
        let mut pdf = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::new();
        for (i, body) in objects.iter().enumerate() {
            offsets.push(pdf.len());
            pdf.extend_from_slice(format!("{} 0 obj\n{body}\nendobj\n", i + 1).as_bytes());
        }
        let xref = pdf.len();
        pdf.extend_from_slice(
            format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes(),
        );
        for o in offsets {
            pdf.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
        }
        pdf.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
                objects.len() + 1
            )
            .as_bytes(),
        );
        pdf
    }

    #[test]
    fn malformed_pdf_is_skipped_not_fatal() {
        let pdf = pdf_with_bad_cmap();
        // The fixture really does make the third-party parser panic...
        let raw = std::panic::catch_unwind(|| pdf_extract::extract_text_from_mem(&pdf));
        assert!(raw.is_err(), "fixture no longer triggers the parser panic");

        // ...and Storecat turns that into "no text" for this one file.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("broken.pdf");
        std::fs::write(&path, &pdf).unwrap();
        assert_eq!(extract_text(&path, "pdf", DEFAULT_MAX_TEXT), None);
        assert_eq!(
            crate::guarded_file(),
            None,
            "guard must be cleared afterwards"
        );
    }

    /// `guarded` only works if panics unwind. Keep `panic = "abort"` out of the
    /// workspace's release profile.
    #[test]
    fn release_builds_unwind_panics() {
        let manifest = include_str!("../../../Cargo.toml");
        let release = manifest
            .split("[profile.release]")
            .nth(1)
            .expect("workspace Cargo.toml has a release profile");
        let release = release.split("\n[").next().unwrap_or(release);
        assert!(
            !release.replace(' ', "").contains("panic=\"abort\""),
            "release profile must not abort on panic: parser panics would kill the app"
        );
    }

    #[test]
    fn strips_html() {
        let t = strip_tags("<html><head><style>p{}</style></head><body><p>Hello &amp; <b>bye</b></p><script>x()</script></body></html>");
        assert!(t.contains("Hello & bye"));
        assert!(!t.contains("x()"));
    }

    #[test]
    fn collects_docx_runs() {
        let xml = r#"<w:body><w:p><w:r><w:t xml:space="preserve">Hello </w:t></w:r><w:r><w:t>world</w:t></w:r></w:p><w:p><w:r><w:t>Next</w:t></w:r></w:p></w:body>"#;
        let mut out = String::new();
        collect_runs(xml, "w:t", "</w:p>", &mut out, 1_000);
        assert_eq!(out, "Hello world\nNext\n");
    }

    #[test]
    fn plain_text_and_binary_sniff() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("a.txt");
        std::fs::write(&p, "line one\n\n\n\nline two\n").unwrap();
        assert_eq!(
            extract_text(&p, "txt", 1000).unwrap(),
            "line one\n\nline two\n"
        );
        let b = tmp.path().join("b.txt");
        std::fs::write(&b, [0u8, 1, 2, 3, 0, 0, 65, 66]).unwrap();
        assert!(extract_text(&b, "txt", 1000).is_none());
    }
}
