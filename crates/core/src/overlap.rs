//! Cross-drive overlap: how much of each drive's content exists elsewhere, what each pair
//! of drives shares, and which folders are copies of each other under any name. Built from
//! the quick hashes in `dupes.db` and each catalogue's folder tree, so no drive needs to be
//! connected. Copies inside app-managed folders are left out, as in the duplicates list.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

use serde::Serialize;

use crate::catalog::Catalog;
use crate::error::Result;
use crate::model::{Kind, Volume, VolumeId};

/// Folders smaller than this aren't matched: the list is about space worth reclaiming.
pub const MIN_FOLDER_BYTES: u64 = 1 << 30;
/// Share of a folder that must be in the other for "mostly inside".
const MOSTLY: f64 = 0.95;
/// Matches listed, most space first.
const MAX_MATCHES: usize = 400;

#[derive(Debug, Clone, Default, Serialize)]
pub struct OverlapReport {
    /// Distinct content across all drives (each file counted once however many copies).
    pub distinct: u64,
    /// Space freed by keeping exactly one copy of everything.
    pub reclaimable: u64,
    /// Distinct content held on one drive only (copies within that drive included).
    pub single_copy: u64,
    /// Distinct content on two or more drives.
    pub multi_copy: u64,
    /// Single-copy content by kind, largest first.
    pub single_by_kind: Vec<KindBytes>,
    pub drives: Vec<DriveOverlap>,
    /// Pairs of drives that share content, most first.
    pub pairs: Vec<PairOverlap>,
    /// Folders that are copies of each other, most reclaimable space first.
    pub folders: Vec<FolderMatch>,
    /// Matches found beyond those listed.
    pub more_folders: usize,
    /// Files smaller than this have no hash and are not counted.
    pub min_file_bytes: u64,
    pub min_folder_bytes: u64,
    pub elapsed_ms: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct KindBytes {
    pub kind: Kind,
    pub bytes: u64,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct DriveOverlap {
    pub volume_id: VolumeId,
    /// Hashed files on the drive, every copy counted.
    pub hashed: u64,
    /// Distinct content on the drive.
    pub distinct: u64,
    /// Content found on no other drive.
    pub only_here: u64,
    /// Extra copies within the drive itself.
    pub within: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct PairOverlap {
    pub a: VolumeId,
    pub b: VolumeId,
    /// Distinct content found on both.
    pub shared: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct FolderSide {
    pub volume_id: VolumeId,
    /// The folder's id in the drive's catalogue.
    pub dir_id: i64,
    /// Relative to the drive's root, `/`-separated.
    pub path: String,
    /// Distinct content in the folder and its subfolders.
    pub bytes: u64,
    pub files: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Relation {
    /// The same files, whatever the folders are called.
    Identical,
    /// Everything in `a` is also in `b`, which holds more.
    Inside,
    /// At least 95% of `a` is also in `b`.
    MostlyInside,
}

#[derive(Debug, Clone, Serialize)]
pub struct FolderMatch {
    pub a: FolderSide,
    pub b: FolderSide,
    pub relation: Relation,
    /// Content of `a` also in `b`: the space removing one side's copies frees.
    pub shared: u64,
    /// The side suggested to keep, "a" or "b".
    pub keep: &'static str,
    /// Matches that share a folder (the same footage in three or four places) have the
    /// same group, numbered from 0 in list order.
    pub group: usize,
}

impl Catalog {
    /// The overlap report, worked out again only when the hashes behind it have changed.
    pub fn overlap_report(&self) -> Result<Arc<OverlapReport>> {
        let key = (
            self.dupes
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .generation(),
            self.hash_min_bytes(),
        );
        if let Some((k, r)) = &*self.overlap.lock().unwrap_or_else(|e| e.into_inner()) {
            if *k == key {
                return Ok(Arc::clone(r));
            }
        }
        let report = Arc::new(build(self, MIN_FOLDER_BYTES)?);
        log::info!(
            "overlap report: {} folder matches in {} ms",
            report.folders.len() + report.more_folders,
            report.elapsed_ms
        );
        *self.overlap.lock().unwrap_or_else(|e| e.into_inner()) = Some((key, Arc::clone(&report)));
        Ok(report)
    }
}

/// A folder in the combined tree of every drive.
struct Node {
    vol: VolumeId,
    dir: i64,
    parent: Option<u32>,
    depth: u16,
    path: String,
}

const UNSET: u16 = u16::MAX;

pub(crate) fn build(cat: &Catalog, min_folder: u64) -> Result<OverlapReport> {
    let started = Instant::now();
    let min_file = cat.hash_min_bytes();
    let vols = cat.volumes()?;
    let nested: HashSet<VolumeId> = crate::suggest::overlaps(&vols)
        .into_iter()
        .map(|(inner, _)| inner)
        .collect();
    let counted: HashSet<VolumeId> = vols
        .iter()
        .map(|v| v.id)
        .filter(|id| !nested.contains(id))
        .collect();

    // Content keys and where each hashed file is.
    let entries = cat
        .dupes
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .all_entries(min_file)?;
    let mut key_of: HashMap<Vec<u8>, u32> = HashMap::new();
    let mut key_size: Vec<u64> = Vec::new();
    let mut by_vol: HashMap<VolumeId, Vec<(u32, i64)>> = HashMap::new();
    for (hash, size, vid, fid) in entries {
        if !counted.contains(&vid) {
            continue;
        }
        let next = key_size.len() as u32;
        let k = *key_of.entry(hash).or_insert_with(|| {
            key_size.push(size);
            next
        });
        by_vol.entry(vid).or_default().push((k, fid));
    }
    drop(key_of);

    let mut nodes: Vec<Node> = Vec::new();
    // (key, folder node, kind)
    let mut locs: Vec<(u32, u32, Kind)> = Vec::new();
    for v in vols.iter().filter(|v| by_vol.contains_key(&v.id)) {
        if !cat.dirs.volume_db(v.id).exists() {
            continue;
        }
        let db = cat.volume_db(v.id)?;
        let tree = db.dir_tree()?;
        let base = nodes.len() as u32;
        let idx: HashMap<i64, u32> = tree
            .iter()
            .enumerate()
            .map(|(i, (id, _, _))| (*id, base + i as u32))
            .collect();
        for (dir, parent, path) in tree {
            nodes.push(Node {
                vol: v.id,
                dir,
                parent: parent.and_then(|p| idx.get(&p).copied()),
                depth: UNSET,
                path,
            });
        }
        let places: HashMap<i64, (i64, Kind)> = db
            .file_places(min_file)?
            .into_iter()
            .map(|(f, d, k)| (f, (d, k)))
            .collect();
        let mut managed: HashMap<u32, bool> = HashMap::new();
        for (k, fid) in &by_vol[&v.id] {
            let Some((dir, kind)) = places.get(fid) else {
                continue;
            };
            let Some(&n) = idx.get(dir) else { continue };
            let app = *managed
                .entry(n)
                .or_insert_with(|| crate::places::is_app_managed(&nodes[n as usize].path));
            if !app {
                locs.push((*k, n, *kind));
            }
        }
    }
    set_depths(&mut nodes);

    let mut report = OverlapReport {
        min_file_bytes: min_file,
        min_folder_bytes: min_folder,
        ..Default::default()
    };
    let mut drives: HashMap<VolumeId, DriveOverlap> = HashMap::new();
    let mut pairs: HashMap<(VolumeId, VolumeId), u64> = HashMap::new();
    let mut single_kind: HashMap<Kind, u64> = HashMap::new();
    // Distinct content and file count under each folder.
    let mut dbytes: Vec<u64> = vec![0; nodes.len()];
    let mut dfiles: Vec<u64> = vec![0; nodes.len()];

    locs.sort_unstable_by_key(|l| (l.0, l.1));
    let groups: Vec<&[(u32, u32, Kind)]> = locs.chunk_by(|a, b| a.0 == b.0).collect();
    let mut anc: Vec<u32> = Vec::new();
    for g in &groups {
        let size = key_size[g[0].0 as usize];
        let mut per_vol: Vec<(VolumeId, u64)> = Vec::new();
        for (_, n, _) in g.iter() {
            let vol = nodes[*n as usize].vol;
            match per_vol.iter_mut().find(|(v, _)| *v == vol) {
                Some((_, c)) => *c += 1,
                None => per_vol.push((vol, 1)),
            }
        }
        report.distinct += size;
        report.reclaimable += size * (g.len() as u64 - 1);
        if per_vol.len() == 1 {
            report.single_copy += size;
            *single_kind.entry(g[0].2).or_default() += size;
        } else {
            report.multi_copy += size;
        }
        for (vol, copies) in &per_vol {
            let d = drives.entry(*vol).or_insert_with(|| DriveOverlap {
                volume_id: *vol,
                ..Default::default()
            });
            d.hashed += size * copies;
            d.distinct += size;
            d.within += size * (copies - 1);
            if per_vol.len() == 1 {
                d.only_here += size;
            }
        }
        for i in 0..per_vol.len() {
            for j in i + 1..per_vol.len() {
                let (a, b) = order(per_vol[i].0, per_vol[j].0);
                *pairs.entry((a, b)).or_default() += size;
            }
        }
        // Every folder holding this content, counted once each.
        anc.clear();
        for (_, n, _) in g.iter() {
            ancestors(&nodes, *n, &mut anc);
        }
        anc.sort_unstable();
        anc.dedup();
        for &a in &anc {
            dbytes[a as usize] += size;
            dfiles[a as usize] += 1;
        }
    }

    // Content shared by pairs of folders big enough to matter.
    let mut shared: HashMap<(u32, u32), u64> = HashMap::new();
    let mut big: Vec<u32> = Vec::new();
    for g in groups.iter().filter(|g| g.len() > 1) {
        let size = key_size[g[0].0 as usize];
        big.clear();
        for (_, n, _) in g.iter() {
            anc.clear();
            ancestors(&nodes, *n, &mut anc);
            big.extend(anc.iter().filter(|&&a| dbytes[a as usize] >= min_folder));
        }
        big.sort_unstable();
        big.dedup();
        for i in 0..big.len() {
            for j in i + 1..big.len() {
                let (x, y) = (big[i], big[j]);
                if !is_nested(&nodes, x, y) {
                    *shared.entry((x, y)).or_default() += size;
                }
            }
        }
    }

    // Classify, then keep the outermost match of each nest.
    let mut cands: Vec<(u32, u32, Relation, u64)> = Vec::new();
    for ((x, y), s) in shared {
        if s < min_folder {
            continue;
        }
        let (bx, by) = (dbytes[x as usize], dbytes[y as usize]);
        // `a` is the folder (mostly) inside the other.
        let (a, b, ba, bb) = if bx <= by {
            (x, y, bx, by)
        } else {
            (y, x, by, bx)
        };
        let relation = if s == ba && s == bb {
            Relation::Identical
        } else if s == ba {
            Relation::Inside
        } else if s as f64 >= MOSTLY * ba as f64 {
            Relation::MostlyInside
        } else {
            continue;
        };
        cands.push((a, b, relation, s));
    }
    // Most shared first; then the tightest container (Equinox inside "All videos/Equinox"
    // rather than inside "All videos"); then the outermost folder inside it.
    cands.sort_by(|p, q| {
        q.3.cmp(&p.3)
            .then(dbytes[p.1 as usize].cmp(&dbytes[q.1 as usize]))
            .then(nodes[p.0 as usize].depth.cmp(&nodes[q.0 as usize].depth))
            .then(nodes[p.1 as usize].depth.cmp(&nodes[q.1 as usize].depth))
    });
    let mut accepted: HashSet<(u32, u32)> = HashSet::new();
    // A folder, or one inside it, is matched once against each other drive.
    let mut placed: HashSet<(u32, VolumeId)> = HashSet::new();
    // Listed container -> the folders listed as inside it.
    let mut inside_of: HashMap<u32, Vec<u32>> = HashMap::new();
    let mut kept: Vec<(u32, u32, Relation, u64)> = Vec::new();
    let (mut ax, mut ay) = (Vec::new(), Vec::new());
    for c in cands {
        ax.clear();
        ay.clear();
        ancestors(&nodes, c.0, &mut ax);
        ancestors(&nodes, c.1, &mut ay);
        let (va, vb) = (nodes[c.0 as usize].vol, nodes[c.1 as usize].vol);
        let covered = ax.iter().any(|&p| {
            placed.contains(&(p, vb))
                || ay.iter().any(|&q| {
                    (p, q) != (c.0, c.1)
                        && (accepted.contains(&(p, q)) || accepted.contains(&(q, p)))
                })
        }) || (c.2 == Relation::Identical && ay.iter().any(|&q| placed.contains(&(q, va))))
            // The same pair seen from the other side: a listed folder inside `a` (or `a`
            // itself) already holds a folder that sits inside `b`.
            || ax.iter().any(|&q| {
                inside_of
                    .get(&q)
                    .is_some_and(|ps| ps.iter().any(|&p| is_nested(&nodes, p, c.1)))
            });
        if covered {
            continue;
        }
        accepted.insert((c.0, c.1));
        placed.insert((c.0, vb));
        if c.2 == Relation::Identical {
            placed.insert((c.1, va));
        }
        inside_of.entry(c.1).or_default().push(c.0);
        kept.push(c);
    }
    report.more_folders = kept.len().saturating_sub(MAX_MATCHES);
    kept.truncate(MAX_MATCHES);
    let groups = group_matches(&kept);
    let side = |n: u32| FolderSide {
        volume_id: nodes[n as usize].vol,
        dir_id: nodes[n as usize].dir,
        path: nodes[n as usize].path.clone(),
        bytes: dbytes[n as usize],
        files: dfiles[n as usize],
    };
    report.folders = kept
        .into_iter()
        .zip(groups)
        .map(|((a, b, relation, s), group)| {
            let (a, b) = (side(a), side(b));
            let keep = keep_side(&a, &b, relation, &vols);
            FolderMatch {
                a,
                b,
                relation,
                shared: s,
                keep,
                group,
            }
        })
        .collect();

    let mut drives: Vec<DriveOverlap> = drives.into_values().collect();
    drives.sort_by_key(|d| std::cmp::Reverse(d.distinct));
    report.drives = drives;
    let mut pairs: Vec<PairOverlap> = pairs
        .into_iter()
        .map(|((a, b), shared)| PairOverlap { a, b, shared })
        .collect();
    pairs.sort_by_key(|p| std::cmp::Reverse(p.shared));
    report.pairs = pairs;
    let mut kinds: Vec<KindBytes> = single_kind
        .into_iter()
        .map(|(kind, bytes)| KindBytes { kind, bytes })
        .collect();
    kinds.sort_by_key(|k| std::cmp::Reverse(k.bytes));
    report.single_by_kind = kinds;
    report.elapsed_ms = started.elapsed().as_millis() as u64;
    Ok(report)
}

/// Number the connected sets of matches (folders linked by any match), in list order.
fn group_matches(kept: &[(u32, u32, Relation, u64)]) -> Vec<usize> {
    fn find(parent: &mut HashMap<u32, u32>, x: u32) -> u32 {
        let mut r = x;
        while let Some(&p) = parent.get(&r) {
            if p == r {
                break;
            }
            r = p;
        }
        parent.insert(x, r);
        r
    }
    let mut parent: HashMap<u32, u32> = HashMap::new();
    for (a, b, _, _) in kept {
        parent.entry(*a).or_insert(*a);
        parent.entry(*b).or_insert(*b);
        let (ra, rb) = (find(&mut parent, *a), find(&mut parent, *b));
        if ra != rb {
            parent.insert(rb, ra);
        }
    }
    let mut number: HashMap<u32, usize> = HashMap::new();
    kept.iter()
        .map(|(a, _, _, _)| {
            let root = find(&mut parent, *a);
            let next = number.len();
            *number.entry(root).or_insert(next)
        })
        .collect()
}

fn order(a: VolumeId, b: VolumeId) -> (VolumeId, VolumeId) {
    if a <= b {
        (a, b)
    } else {
        (b, a)
    }
}

fn set_depths(nodes: &mut [Node]) {
    let mut chain: Vec<u32> = Vec::new();
    for i in 0..nodes.len() {
        if nodes[i].depth != UNSET {
            continue;
        }
        chain.clear();
        let mut cur = Some(i as u32);
        let mut base = 0u16;
        while let Some(c) = cur {
            let n = &nodes[c as usize];
            if n.depth != UNSET {
                base = n.depth + 1;
                break;
            }
            chain.push(c);
            if chain.len() > 4096 {
                break; // a broken parent link; depth stays approximate
            }
            cur = n.parent;
        }
        for (k, &c) in chain.iter().rev().enumerate() {
            nodes[c as usize].depth = base + k as u16;
        }
    }
}

/// `n` and the folders above it, stopping short of the drive's root.
fn ancestors(nodes: &[Node], n: u32, out: &mut Vec<u32>) {
    let mut cur = Some(n);
    let mut steps = 0;
    while let Some(c) = cur {
        let node = &nodes[c as usize];
        if node.parent.is_none() || steps > 4096 {
            break;
        }
        out.push(c);
        cur = node.parent;
        steps += 1;
    }
}

/// Whether one folder is inside the other (or they are the same).
fn is_nested(nodes: &[Node], x: u32, y: u32) -> bool {
    if nodes[x as usize].vol != nodes[y as usize].vol {
        return false;
    }
    let (hi, mut lo) = if nodes[x as usize].depth <= nodes[y as usize].depth {
        (x, y)
    } else {
        (y, x)
    };
    let target = nodes[hi as usize].depth;
    while nodes[lo as usize].depth > target {
        match nodes[lo as usize].parent {
            Some(p) => lo = p,
            None => return false,
        }
    }
    lo == hi
}

/// Which copy to suggest keeping: a project folder over a backup copy, a healthy drive
/// over one with read errors, the folder holding more over the one inside it, and the
/// bigger drive (where things can be gathered) over a smaller one.
fn keep_side(a: &FolderSide, b: &FolderSide, relation: Relation, vols: &[Volume]) -> &'static str {
    let vol = |s: &FolderSide| vols.iter().find(|v| v.id == s.volume_id);
    let score = |s: &FolderSide, holds_more: bool| -> i32 {
        let p = s.path.to_lowercase();
        let mut n = 0;
        if ["backup", "copy of", "recovered", "old ", "temp", "tmp"]
            .iter()
            .any(|w| p.contains(w))
        {
            n += 2;
        }
        if vol(s).is_some_and(|v| v.fault.is_some()) {
            n += 4;
        }
        if holds_more {
            n -= 1;
        }
        n
    };
    let cap = |s: &FolderSide| vol(s).map(|v| v.capacity).unwrap_or(0);
    let (mut sa, mut sb) = (score(a, false), score(b, relation != Relation::Identical));
    if cap(a) < cap(b) {
        sa += 1;
    } else if cap(b) < cap(a) {
        sb += 1;
    }
    if sa != sb {
        return if sa < sb { "a" } else { "b" };
    }
    if cap(a) > cap(b) {
        "a"
    } else {
        "b"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DataDirs, EnrichOptions, ScanOptions};

    fn write(dir: &std::path::Path, rel: &str, seed: u8, len: usize) {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        let data: Vec<u8> = (0..len)
            .map(|i| (i as u8).wrapping_mul(31) ^ seed)
            .collect();
        std::fs::write(p, data).unwrap();
    }

    #[test]
    fn matches_folders_across_and_within_drives() {
        let data = tempfile::tempdir().unwrap();
        let one = tempfile::tempdir().unwrap();
        let two = tempfile::tempdir().unwrap();
        // The same shoot under different names on two drives, one with an extra file.
        for (i, seed) in [1u8, 2, 3].iter().enumerate() {
            write(
                one.path(),
                &format!("Clients/Equinox/Edits/clip{i}.mov"),
                *seed,
                5000,
            );
            write(
                two.path(),
                &format!("All videos/Equinox/clip{i}.mov"),
                *seed,
                5000,
            );
        }
        write(two.path(), "All videos/Equinox/extra.mov", 9, 5000);
        // A laptop backup on drive one repeating a project folder of drive one.
        write(one.path(), "Projects/Mpav/a.mp4", 20, 8000);
        write(one.path(), "Projects/Mpav/b.mp4", 21, 8000);
        write(one.path(), "Backup 2022/C Drive/Mpav/a.mp4", 20, 8000);
        write(one.path(), "Backup 2022/C Drive/Mpav/b.mp4", 21, 8000);
        // Only on drive two; and a copy in an app folder, which doesn't count.
        write(two.path(), "Solo/only.wav", 40, 7000);
        write(one.path(), "node_modules/pkg/only.wav", 40, 7000);
        // Other work beside each folder, so the parents don't match too.
        write(one.path(), "Clients/Other/x.mov", 50, 20000);
        write(two.path(), "All videos/Other/y.mov", 51, 20000);
        write(one.path(), "Projects/Other/p.mov", 52, 20000);
        write(one.path(), "Backup 2022/D Drive/q.mov", 53, 20000);

        let cat = Catalog::open(DataDirs::at(data.path())).unwrap();
        cat.set_setting(crate::settings::HASH_MIN_BYTES, "1000")
            .unwrap();
        let opts = EnrichOptions {
            threads: 2,
            hash_min_bytes: 1000,
            ..Default::default()
        };
        let mut ids = Vec::new();
        for (dir, name) in [(one.path(), "One"), (two.path(), "Two")] {
            let v = cat.add_folder(dir, Some(name)).unwrap();
            cat.scan(v.id, &ScanOptions::default(), &|_| {}).unwrap();
            cat.enrich(v.id, &opts, &|_| {}).unwrap();
            ids.push(v.id);
        }
        let r = build(&cat, 1).unwrap();
        // Distinct: 4 clips (5000 each), 2 mp4s (8000), the solo wav (7000), 4 others.
        let others = 4 * 20000;
        assert_eq!(r.distinct, 4 * 5000 + 2 * 8000 + 7000 + others);
        assert_eq!(
            r.reclaimable,
            3 * 5000 + 2 * 8000,
            "a copy of each shared file"
        );
        assert_eq!(r.multi_copy, 3 * 5000);
        assert_eq!(r.single_copy, 5000 + 2 * 8000 + 7000 + others);
        assert_eq!(r.pairs.len(), 1);
        assert_eq!(r.pairs[0].shared, 3 * 5000);
        let one_d = r.drives.iter().find(|d| d.volume_id == ids[0]).unwrap();
        assert_eq!(one_d.within, 2 * 8000);
        assert_eq!(
            one_d.only_here,
            2 * 8000 + 3 * 20000,
            "Mpav (twice) and the others"
        );

        let find = |p: &str| {
            r.folders
                .iter()
                .find(|m| m.a.path.ends_with(p) || m.b.path.ends_with(p))
        };
        // Clients/Equinox holds only Edits, so the match is reported at Clients/Equinox.
        let eq = find("Equinox").expect("the Equinox shoot is matched");
        assert_eq!(eq.relation, Relation::Inside, "{eq:?}");
        assert_eq!(eq.a.path, "Clients/Equinox");
        assert_eq!(eq.b.path, "All videos/Equinox");
        assert_eq!(eq.shared, 3 * 5000);
        let mpav = find("Mpav").expect("the backup copy is matched");
        assert_eq!(mpav.relation, Relation::Identical);
        let keep = if mpav.keep == "a" { &mpav.a } else { &mpav.b };
        assert_eq!(
            keep.path, "Projects/Mpav",
            "the project folder over the backup"
        );
        // Nested matches collapse into the outermost pair.
        assert_eq!(
            r.folders
                .iter()
                .filter(|m| m.a.path.contains("Mpav") || m.b.path.contains("Mpav"))
                .count(),
            1,
            "{:?}",
            r.folders
        );
        assert!(r
            .folders
            .iter()
            .all(|m| !m.a.path.contains("node_modules") && !m.b.path.contains("node_modules")));
    }

    #[test]
    fn report_is_cached_until_hashes_change() {
        let data = tempfile::tempdir().unwrap();
        let tree = tempfile::tempdir().unwrap();
        write(tree.path(), "a.bin", 1, 3000);
        let cat = Catalog::open(DataDirs::at(data.path())).unwrap();
        cat.set_setting(crate::settings::HASH_MIN_BYTES, "1000")
            .unwrap();
        let v = cat.add_folder(tree.path(), Some("T")).unwrap();
        cat.scan(v.id, &ScanOptions::default(), &|_| {}).unwrap();
        let opts = EnrichOptions {
            threads: 2,
            hash_min_bytes: 1000,
            ..Default::default()
        };
        cat.enrich(v.id, &opts, &|_| {}).unwrap();
        let first = cat.overlap_report().unwrap();
        assert!(Arc::ptr_eq(&first, &cat.overlap_report().unwrap()));
        write(tree.path(), "b.bin", 2, 3000);
        cat.scan(v.id, &ScanOptions::default(), &|_| {}).unwrap();
        cat.enrich(v.id, &opts, &|_| {}).unwrap();
        let second = cat.overlap_report().unwrap();
        assert!(!Arc::ptr_eq(&first, &second));
        assert_eq!(second.distinct, 6000);
    }
}
