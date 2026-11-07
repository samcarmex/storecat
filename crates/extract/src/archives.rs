//! List the entries inside archives so they are searchable without extracting anything.
//! RAR is deliberately absent: the unrar licence is not GPL-compatible.

use std::fs::File;
use std::io::BufReader;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::guarded;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchiveEntry {
    /// `/`-separated path inside the archive.
    pub path: String,
    pub size: u64,
    pub mtime: i64,
    pub is_dir: bool,
}

pub const DEFAULT_MAX_ENTRIES: usize = 20_000;
const MAX_ARCHIVE_BYTES: u64 = 8 * 1024 * 1024 * 1024;

pub fn list_archive(path: &Path, ext: &str, max_entries: usize) -> Option<Vec<ArchiveEntry>> {
    let p = path.to_path_buf();
    let e = ext.to_string();
    let name = path
        .file_name()
        .map(|s| s.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    guarded("archive", move || {
        // Disc images list from their directory sectors only, so their size doesn't matter.
        if e == "iso" {
            return crate::iso::iso_entries(&p, max_entries);
        }
        if std::fs::metadata(&p).ok()?.len() > MAX_ARCHIVE_BYTES {
            return None;
        }
        match e.as_str() {
            "zip" | "jar" | "cbz" | "epub" | "ipa" | "apk" | "xpi" => zip_entries(&p, max_entries),
            "7z" => sevenz_entries(&p, max_entries),
            "tar" => tar_entries(File::open(&p).ok()?, max_entries),
            "tgz" => tar_entries(
                flate2::read::GzDecoder::new(File::open(&p).ok()?),
                max_entries,
            ),
            "gz" if name.ends_with(".tar.gz") => tar_entries(
                flate2::read::GzDecoder::new(File::open(&p).ok()?),
                max_entries,
            ),
            _ => None,
        }
    })
    .filter(|v| !v.is_empty())
}

fn zip_entries(path: &Path, max: usize) -> Option<Vec<ArchiveEntry>> {
    let f = File::open(path).ok()?;
    let mut zip = zip::ZipArchive::new(BufReader::new(f)).ok()?;
    let mut out = Vec::with_capacity(zip.len().min(max));
    for i in 0..zip.len().min(max) {
        let Ok(entry) = zip.by_index_raw(i) else {
            continue;
        };
        let name = entry.name().trim_end_matches('/').to_string();
        if name.is_empty() {
            continue;
        }
        let mtime = entry
            .last_modified()
            .and_then(|dt| time::OffsetDateTime::try_from(dt).ok())
            .map(|t| t.unix_timestamp())
            .unwrap_or(0);
        out.push(ArchiveEntry {
            path: name,
            size: entry.size(),
            mtime,
            is_dir: entry.is_dir(),
        });
    }
    Some(out)
}

fn tar_entries<R: std::io::Read>(reader: R, max: usize) -> Option<Vec<ArchiveEntry>> {
    let mut archive = tar::Archive::new(reader);
    let mut out = Vec::new();
    for entry in archive.entries().ok()? {
        if out.len() >= max {
            break;
        }
        let Ok(entry) = entry else { continue };
        let header = entry.header();
        let Ok(path) = entry.path() else { continue };
        let p = path.to_string_lossy().trim_end_matches('/').to_string();
        if p.is_empty() {
            continue;
        }
        out.push(ArchiveEntry {
            path: p,
            size: header.size().unwrap_or(0),
            mtime: header.mtime().unwrap_or(0) as i64,
            is_dir: header.entry_type().is_dir(),
        });
    }
    Some(out)
}

fn sevenz_entries(path: &Path, max: usize) -> Option<Vec<ArchiveEntry>> {
    let f = File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    let archive = sevenz_rust::Archive::read(&mut BufReader::new(f), len, &[]).ok()?;
    let mut out = Vec::new();
    for e in archive.files.iter().take(max) {
        let name = e.name().trim_end_matches('/').to_string();
        if name.is_empty() {
            continue;
        }
        let mtime = if e.has_last_modified_date {
            filetime_to_unix(e.last_modified_date.into())
        } else {
            0
        };
        out.push(ArchiveEntry {
            path: name,
            size: e.size(),
            mtime,
            is_dir: e.is_directory(),
        });
    }
    Some(out)
}

/// Windows FILETIME (100 ns ticks since 1601) to unix seconds.
fn filetime_to_unix(ft: u64) -> i64 {
    const EPOCH_DIFF: i64 = 11_644_473_600;
    (ft / 10_000_000) as i64 - EPOCH_DIFF
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn lists_zip_entries() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("a.zip");
        {
            let f = File::create(&p).unwrap();
            let mut z = zip::ZipWriter::new(f);
            let opts = zip::write::SimpleFileOptions::default();
            z.add_directory("docs/", opts).unwrap();
            z.start_file("docs/readme.txt", opts).unwrap();
            z.write_all(b"hello").unwrap();
            z.start_file("photo.jpg", opts).unwrap();
            z.write_all(&[0u8; 100]).unwrap();
            z.finish().unwrap();
        }
        let entries = list_archive(&p, "zip", 100).unwrap();
        let names: Vec<&str> = entries.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(names, vec!["docs", "docs/readme.txt", "photo.jpg"]);
        assert!(entries[0].is_dir);
        assert_eq!(entries[1].size, 5);
    }
}
