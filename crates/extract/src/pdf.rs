//! PDFs: metadata and text from one parse of bytes already in memory, so a PDF is read
//! from disk once and the slow part (parsing) can run away from the disk.

use std::path::Path;

use crate::guarded;
use crate::meta::FileMeta;

/// PDFs larger than this are hashed and catalogued but not parsed.
pub const PDF_MAX_BYTES: u64 = 64 * 1024 * 1024;

/// Metadata and/or text of a PDF held in memory. `path` only labels log lines. Text stops
/// once `max_text` bytes are collected, so long documents don't parse every page.
pub fn pdf_meta_and_text(
    path: &Path,
    bytes: &[u8],
    want_meta: bool,
    want_text: bool,
    max_text: usize,
) -> (Option<FileMeta>, Option<String>) {
    if bytes.len() as u64 > PDF_MAX_BYTES || !(want_meta || want_text) {
        return (None, None);
    }
    guarded("pdf", path, move || {
        let mut doc = lopdf::Document::load_mem(bytes).ok()?;
        if doc.is_encrypted() {
            // Most "encrypted" PDFs only restrict printing and open with an empty password.
            let _ = doc.decrypt("");
        }
        let meta = want_meta
            .then(|| crate::meta::pdf_meta_of(&doc))
            .filter(|m| !m.is_empty());
        let text = if want_text && !doc.is_encrypted() {
            page_text(&doc, max_text)
        } else {
            None
        };
        Some((meta, text))
    })
    .unwrap_or((None, None))
}

fn page_text(doc: &lopdf::Document, max_text: usize) -> Option<String> {
    let pages: Vec<u32> = doc.get_pages().keys().copied().collect();
    let mut out = String::new();
    for n in pages {
        {
            let mut sink = pdf_extract::PlainTextOutput::new(&mut out);
            // A page that fails to decode is skipped; the rest of the document still counts.
            let _ = pdf_extract::output_doc_page(doc, &mut sink, n);
        }
        if out.len() >= max_text {
            break;
        }
    }
    let t = crate::text::normalise(&out, max_text);
    (!t.trim().is_empty()).then_some(t)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A one-page PDF with a title and the words "Hello zebra".
    fn simple_pdf() -> Vec<u8> {
        let content = "BT /F1 12 Tf 10 50 Td (Hello zebra) Tj ET";
        let objects = [
            "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] \
             /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >>"
                .to_string(),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
            format!(
                "<< /Length {} >>\nstream\n{content}\nendstream",
                content.len()
            ),
            "<< /Title (Zoo plan) /Author (Sam) >>".to_string(),
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
                "trailer\n<< /Size {} /Root 1 0 R /Info 6 0 R >>\nstartxref\n{xref}\n%%EOF\n",
                objects.len() + 1
            )
            .as_bytes(),
        );
        pdf
    }

    #[test]
    fn meta_and_text_from_one_parse() {
        let pdf = simple_pdf();
        let p = Path::new("memo.pdf");
        let (meta, text) = pdf_meta_and_text(p, &pdf, true, true, 1 << 20);
        let meta = meta.unwrap();
        assert_eq!(meta.title.as_deref(), Some("Zoo plan"));
        assert_eq!(meta.pages, Some(1));
        assert!(text.unwrap().contains("Hello zebra"));
        // Only what was asked for.
        let (meta, text) = pdf_meta_and_text(p, &pdf, true, false, 1 << 20);
        assert!(meta.is_some() && text.is_none());
        // Not a PDF: nothing, and no panic.
        assert_eq!(
            pdf_meta_and_text(p, b"not a pdf", true, true, 100),
            (None, None)
        );
    }
}
