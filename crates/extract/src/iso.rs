//! ISO 9660 disc images (CD/DVD rips, Linux installers, backups burned to disc).
//!
//! Reads only the volume descriptors and directory sectors, never file contents, so a
//! 50 GB image lists in milliseconds. Prefers Joliet names (Unicode, long), falls back to
//! Rock Ridge `NM` names, then plain ISO 9660 names. UDF-only images (some newer DVDs and
//! Windows installers) carry a stub ISO 9660 tree and list just that stub.

use std::collections::HashSet;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use crate::archives::ArchiveEntry;

const SECTOR: u64 = 2048;
const MAX_DEPTH: usize = 64;
/// Directories larger than this are not real (corrupt image); stop rather than read GBs.
const MAX_DIR_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Clone, Copy)]
struct Extent {
    lba: u32,
    len: u32,
}

pub fn iso_entries(path: &Path, max: usize) -> Option<Vec<ArchiveEntry>> {
    let mut f = File::open(path).ok()?;
    let (root, joliet) = find_root(&mut f)?;
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    walk(&mut f, root, "", joliet, 0, max, &mut seen, &mut out);
    Some(out)
}

/// Root directory extent from the Joliet supplementary descriptor if present, else the
/// primary one. Returns (root, is_joliet).
fn find_root(f: &mut File) -> Option<(Extent, bool)> {
    let mut primary = None;
    let mut joliet = None;
    for i in 16..64u64 {
        let mut d = [0u8; 2048];
        f.seek(SeekFrom::Start(i * SECTOR)).ok()?;
        f.read_exact(&mut d).ok()?;
        if &d[1..6] != b"CD001" {
            return None;
        }
        match d[0] {
            1 if primary.is_none() => primary = Some(root_extent(&d)),
            2 if matches!(&d[88..91], b"%/@" | b"%/C" | b"%/E") => joliet = Some(root_extent(&d)),
            255 => break,
            _ => {}
        }
    }
    match (joliet, primary) {
        (Some(j), _) => Some((j, true)),
        (None, Some(p)) => Some((p, false)),
        _ => None,
    }
}

fn root_extent(d: &[u8]) -> Extent {
    let r = &d[156..190];
    Extent {
        lba: le32(&r[2..6]),
        len: le32(&r[10..14]),
    }
}

fn le32(b: &[u8]) -> u32 {
    u32::from_le_bytes([b[0], b[1], b[2], b[3]])
}

#[allow(clippy::too_many_arguments)]
fn walk(
    f: &mut File,
    dir: Extent,
    prefix: &str,
    joliet: bool,
    depth: usize,
    max: usize,
    seen: &mut HashSet<u32>,
    out: &mut Vec<ArchiveEntry>,
) {
    if depth > MAX_DEPTH || out.len() >= max || !seen.insert(dir.lba) {
        return;
    }
    let len = (dir.len as u64).min(MAX_DIR_BYTES) as usize;
    let mut buf = vec![0u8; len];
    if f.seek(SeekFrom::Start(dir.lba as u64 * SECTOR)).is_err() || f.read_exact(&mut buf).is_err()
    {
        return;
    }
    let mut subdirs = Vec::new();
    let mut pos = 0usize;
    while pos < buf.len() && out.len() < max {
        let rec_len = buf[pos] as usize;
        if rec_len == 0 {
            // Records never straddle sectors: skip the padding to the next one.
            pos = (pos / SECTOR as usize + 1) * SECTOR as usize;
            continue;
        }
        if rec_len < 34 || pos + rec_len > buf.len() {
            break;
        }
        let r = &buf[pos..pos + rec_len];
        pos += rec_len;
        let name_len = r[32] as usize;
        if 33 + name_len > r.len() {
            continue;
        }
        let raw = &r[33..33 + name_len];
        if name_len == 1 && (raw[0] == 0 || raw[0] == 1) {
            continue; // "." and ".."
        }
        let is_dir = r[25] & 0x02 != 0;
        let name = if joliet {
            ucs2_be(raw)
        } else {
            rock_ridge_name(r, name_len).unwrap_or_else(|| iso_name(raw, is_dir))
        };
        if name.is_empty() {
            continue;
        }
        let path = if prefix.is_empty() {
            name
        } else {
            format!("{prefix}/{name}")
        };
        let extent = Extent {
            lba: le32(&r[2..6]),
            len: le32(&r[10..14]),
        };
        out.push(ArchiveEntry {
            path: path.clone(),
            size: if is_dir { 0 } else { extent.len as u64 },
            mtime: record_time(&r[18..25]),
            is_dir,
        });
        if is_dir {
            subdirs.push((extent, path));
        }
    }
    for (extent, path) in subdirs {
        walk(f, extent, &path, joliet, depth + 1, max, seen, out);
    }
}

fn ucs2_be(b: &[u8]) -> String {
    let units: Vec<u16> = b
        .chunks_exact(2)
        .map(|c| u16::from_be_bytes([c[0], c[1]]))
        .collect();
    let s = String::from_utf16_lossy(&units);
    strip_version(&s).to_string()
}

fn iso_name(raw: &[u8], is_dir: bool) -> String {
    let s = String::from_utf8_lossy(raw);
    let s = strip_version(&s);
    // "README." -> "README" (ISO 9660 always has the dot).
    let s = if is_dir {
        s
    } else {
        s.strip_suffix('.').unwrap_or(s)
    };
    s.to_string()
}

fn strip_version(s: &str) -> &str {
    match s.rfind(';') {
        Some(i) if s[i + 1..].chars().all(|c| c.is_ascii_digit()) => &s[..i],
        _ => s,
    }
}

/// Rock Ridge alternate name (`NM` entries in the System Use area), if present.
fn rock_ridge_name(r: &[u8], name_len: usize) -> Option<String> {
    let mut p = 33 + name_len + (name_len % 2 == 0) as usize;
    let mut name = String::new();
    while p + 4 <= r.len() {
        let sig = &r[p..p + 2];
        let len = r[p + 2] as usize;
        if len < 4 || p + len > r.len() {
            break;
        }
        if sig == b"NM" && len >= 5 {
            let flags = r[p + 4];
            if flags & 0x06 == 0 {
                name.push_str(&String::from_utf8_lossy(&r[p + 5..p + len]));
            }
        }
        p += len;
    }
    (!name.is_empty()).then_some(name)
}

/// 7-byte directory record date: years since 1900, month, day, h, m, s, GMT offset (15 min).
fn record_time(d: &[u8]) -> i64 {
    let year = 1900 + d[0] as i32;
    let Ok(month) = time::Month::try_from(d[1].max(1)) else {
        return 0;
    };
    let Ok(date) = time::Date::from_calendar_date(year, month, d[2].max(1)) else {
        return 0;
    };
    let Ok(t) = time::Time::from_hms(d[3].min(23), d[4].min(59), d[5].min(59)) else {
        return 0;
    };
    let offset = (d[6] as i8) as i64 * 15 * 60;
    time::PrimitiveDateTime::new(date, t)
        .assume_utc()
        .unix_timestamp()
        - offset
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A minimal ISO 9660 image: PVD at 16, terminator at 17, root dir at 18 with one
    /// subdirectory at 19 holding one file. Optionally a Joliet SVD with UCS-2 names.
    pub(crate) fn tiny_iso(joliet: bool) -> Vec<u8> {
        let mut img = vec![0u8; 24 * 2048];
        let rec = |lba: u32, len: u32, flags: u8, name: &[u8]| -> Vec<u8> {
            let mut r = vec![0u8; 33 + name.len() + (name.len() % 2 == 0) as usize];
            r[0] = r.len() as u8;
            r[2..6].copy_from_slice(&lba.to_le_bytes());
            r[6..10].copy_from_slice(&lba.to_be_bytes());
            r[10..14].copy_from_slice(&len.to_le_bytes());
            r[14..18].copy_from_slice(&len.to_be_bytes());
            r[18..25].copy_from_slice(&[121, 6, 15, 12, 30, 0, 0]); // 2021-06-15 12:30 UTC
            r[25] = flags;
            r[32] = name.len() as u8;
            r[33..33 + name.len()].copy_from_slice(name);
            r
        };
        let put = |img: &mut Vec<u8>, at: usize, bytes: &[u8]| {
            img[at..at + bytes.len()].copy_from_slice(bytes)
        };
        let ucs = |s: &str| -> Vec<u8> { s.encode_utf16().flat_map(|u| u.to_be_bytes()).collect() };

        // Primary volume descriptor -> plain tree at 18/19.
        let mut pvd = vec![0u8; 2048];
        pvd[0] = 1;
        pvd[1..6].copy_from_slice(b"CD001");
        pvd[6] = 1;
        pvd[156..190].copy_from_slice(&rec(18, 2048, 2, &[0])[..34]);
        put(&mut img, 16 * 2048, &pvd);
        let mut sector = 17;
        if joliet {
            let mut svd = pvd.clone();
            svd[0] = 2;
            svd[88..91].copy_from_slice(b"%/E");
            svd[156..190].copy_from_slice(&rec(20, 2048, 2, &[0])[..34]);
            put(&mut img, 17 * 2048, &svd);
            sector = 18;
        }
        let mut term = vec![0u8; 2048];
        term[0] = 255;
        term[1..6].copy_from_slice(b"CD001");
        // Terminator goes right after the descriptors; the plain tree starts at 18, so
        // for the Joliet image move it to sector 21+.
        let (root, sub) = if joliet { (20u32, 21u32) } else { (18, 19) };
        put(&mut img, sector * 2048, &term);
        if joliet {
            // Shift the PVD's tree so it doesn't collide with the terminator at 18.
            let mut pvd2 = pvd.clone();
            pvd2[156..190].copy_from_slice(&rec(22, 2048, 2, &[0])[..34]);
            put(&mut img, 16 * 2048, &pvd2);
        }

        let mut d = Vec::new();
        d.extend(rec(root, 2048, 2, &[0]));
        d.extend(rec(root, 2048, 2, &[1]));
        if joliet {
            d.extend(rec(sub, 2048, 2, &ucs("Holiday Photos 2021")));
        } else {
            d.extend(rec(sub, 2048, 2, b"PHOTOS"));
        }
        put(&mut img, root as usize * 2048, &d);

        let mut s = Vec::new();
        s.extend(rec(sub, 2048, 2, &[0]));
        s.extend(rec(root, 2048, 2, &[1]));
        if joliet {
            s.extend(rec(23, 5000, 0, &ucs("Beach day — sunset.jpg;1")));
        } else {
            s.extend(rec(23, 5000, 0, b"IMG_0001.JPG;1"));
        }
        put(&mut img, sub as usize * 2048, &s);
        img
    }

    #[test]
    fn lists_plain_iso9660() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("disc.iso");
        std::fs::write(&p, tiny_iso(false)).unwrap();
        let e = iso_entries(&p, 100).unwrap();
        assert_eq!(e.len(), 2, "{e:?}");
        assert_eq!(e[0].path, "PHOTOS");
        assert!(e[0].is_dir);
        assert_eq!(e[1].path, "PHOTOS/IMG_0001.JPG");
        assert_eq!(e[1].size, 5000);
        assert_eq!(e[1].mtime, 1_623_760_200);
    }

    #[test]
    fn prefers_joliet_names() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("disc.iso");
        std::fs::write(&p, tiny_iso(true)).unwrap();
        let e = iso_entries(&p, 100).unwrap();
        assert_eq!(e[1].path, "Holiday Photos 2021/Beach day — sunset.jpg");
    }

    #[test]
    fn rejects_non_iso() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("x.iso");
        std::fs::write(&p, vec![7u8; 40_000]).unwrap();
        assert!(iso_entries(&p, 100).is_none());
    }
}
