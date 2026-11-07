//! Storage overview: folder sizes (for the treemap and the suggestions), content-type
//! breakdown per drive, and reachability of folder volumes such as network shares.
//!
//! Folder sizes are computed once per scan into a `dir_sizes` table in the volume's own
//! catalogue (about a second for a million files), so the treemap and the rules engine
//! read precomputed rows instead of summing files on every request.

use std::collections::HashMap;
use std::path::Path;
use std::sync::mpsc;
use std::time::Duration;

use rusqlite::{params, Connection, OptionalExtension};

use crate::catalog::{Catalog, FolderStatus};
use crate::db::volume::VolumeDb;
use crate::error::{Error, Result};
use crate::model::*;

const KINDS: usize = 11;
/// Bumped when the `dir_sizes` layout changes; stale tables are rebuilt on first use.
const DIR_SIZES_VERSION: &str = "1";
/// Largest number of folders / files returned for one treemap level.
const MAX_CHILDREN: usize = 300;
const MAX_FILES: usize = 60;
/// A network share that doesn't answer within this long counts as offline.
const REACH_TIMEOUT: Duration = Duration::from_secs(3);

pub(crate) fn is_network_path(p: &str, media: MediaType) -> bool {
    media == MediaType::Network || p.starts_with("\\\\") || p.starts_with("//")
}

/// (Re)build `dir_sizes` and the per-kind totals for one volume catalogue.
pub(crate) fn build_dir_sizes(conn: &Connection) -> Result<()> {
    // Directory tree.
    let mut ids: Vec<i64> = Vec::new();
    let mut parents: Vec<Option<i64>> = Vec::new();
    {
        let mut stmt = conn.prepare("SELECT id, parent_id FROM dirs")?;
        let rows = stmt.query_map([], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, Option<i64>>(1)?))
        })?;
        for row in rows {
            let (id, parent) = row?;
            ids.push(id);
            parents.push(parent);
        }
    }
    let index: HashMap<i64, usize> = ids.iter().enumerate().map(|(i, &id)| (id, i)).collect();
    let parent_idx: Vec<Option<usize>> = parents
        .iter()
        .map(|p| p.and_then(|p| index.get(&p).copied()))
        .collect();

    // Own files per folder, split by kind.
    let n = ids.len();
    let mut kind_bytes = vec![[0u64; KINDS]; n];
    let mut own_bytes = vec![0u64; n];
    let mut own_files = vec![0u64; n];
    let mut newest = vec![0i64; n];
    {
        let mut stmt = conn.prepare("SELECT dir_id, kind, size, mtime FROM files")?;
        let mut rows = stmt.query([])?;
        while let Some(r) = rows.next()? {
            let Some(&i) = index.get(&r.get::<_, i64>(0)?) else {
                continue;
            };
            let kind = (r.get::<_, i64>(1)? as usize).min(KINDS - 1);
            let size = r.get::<_, i64>(2)?.max(0) as u64;
            let mtime: i64 = r.get(3)?;
            kind_bytes[i][kind] += size;
            own_bytes[i] += size;
            own_files[i] += 1;
            newest[i] = newest[i].max(mtime);
        }
    }

    // Children before parents: breadth-first from the roots, then walk it backwards.
    let mut children: Vec<Vec<usize>> = vec![Vec::new(); n];
    let mut order: Vec<usize> = Vec::with_capacity(n);
    for (i, p) in parent_idx.iter().enumerate() {
        match p {
            Some(p) if *p != i => children[*p].push(i),
            _ => order.push(i),
        }
    }
    let mut head = 0;
    while head < order.len() {
        let i = order[head];
        head += 1;
        order.extend(children[i].iter().copied());
    }
    let mut bytes = own_bytes.clone();
    let mut files = own_files.clone();
    let mut dirs = vec![0u64; n];
    for &i in order.iter().rev() {
        if let Some(p) = parent_idx[i].filter(|&p| p != i) {
            bytes[p] += bytes[i];
            files[p] += files[i];
            dirs[p] += dirs[i] + 1;
            newest[p] = newest[p].max(newest[i]);
            let kb = kind_bytes[i];
            for (k, b) in kb.iter().enumerate() {
                kind_bytes[p][k] += b;
            }
        }
    }

    let tx = conn.unchecked_transaction()?;
    tx.execute_batch(
        "DROP TABLE IF EXISTS dir_sizes;
         CREATE TABLE dir_sizes (
             dir_id    INTEGER PRIMARY KEY,
             bytes     INTEGER NOT NULL,
             files     INTEGER NOT NULL,
             dirs      INTEGER NOT NULL,
             newest    INTEGER NOT NULL,
             own_bytes INTEGER NOT NULL,
             own_files INTEGER NOT NULL,
             top_kind  INTEGER NOT NULL
         );",
    )?;
    {
        let mut ins = tx.prepare(
            "INSERT INTO dir_sizes (dir_id, bytes, files, dirs, newest, own_bytes, own_files, top_kind)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        )?;
        for i in 0..n {
            let top = kind_bytes[i]
                .iter()
                .enumerate()
                .max_by_key(|(_, b)| **b)
                .filter(|(_, b)| **b > 0)
                .map(|(k, _)| k as i64)
                .unwrap_or(0);
            ins.execute(params![
                ids[i],
                bytes[i] as i64,
                files[i] as i64,
                dirs[i] as i64,
                newest[i],
                own_bytes[i] as i64,
                own_files[i] as i64,
                top
            ])?;
        }
    }
    // Volume totals by kind, from the roots.
    let mut totals = [(0u64, 0u64); KINDS];
    {
        let mut stmt = tx.prepare("SELECT kind, COUNT(*), SUM(size) FROM files GROUP BY kind")?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, Option<i64>>(2)?,
            ))
        })?;
        for row in rows {
            let (k, c, b) = row?;
            let k = (k as usize).min(KINDS - 1);
            totals[k].0 += c as u64;
            totals[k].1 += b.unwrap_or(0).max(0) as u64;
        }
    }
    let kinds: Vec<KindBytes> = totals
        .iter()
        .enumerate()
        .filter(|(_, (c, _))| *c > 0)
        .map(|(k, (c, b))| KindBytes {
            kind: Kind::from_u8(k as u8),
            files: *c,
            bytes: *b,
        })
        .collect();
    tx.execute(
        "INSERT INTO info (key, value) VALUES ('kinds', ?1)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        [serde_json::to_string(&kinds).unwrap_or_default()],
    )?;
    tx.execute(
        "INSERT INTO info (key, value) VALUES ('dir_sizes', ?1)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        [DIR_SIZES_VERSION],
    )?;
    tx.commit()?;
    Ok(())
}

fn has_dir_sizes(db: &VolumeDb) -> bool {
    db.info("dir_sizes").ok().flatten().as_deref() == Some(DIR_SIZES_VERSION)
}

impl Catalog {
    /// Make sure a volume's folder sizes exist (catalogues from before Phase 4 lack them).
    pub fn ensure_dir_sizes(&self, volume_id: VolumeId) -> Result<()> {
        let db = self.volume_db(volume_id)?;
        if has_dir_sizes(&db) {
            return Ok(());
        }
        drop(db);
        let path = self.dirs.volume_db(volume_id);
        let rw = VolumeDb::open_rw(&path)?;
        rw.conn().busy_timeout(Duration::from_secs(30))?;
        build_dir_sizes(rw.conn())
    }

    /// Bytes and file counts per content type on one drive.
    pub fn kind_breakdown(&self, volume_id: VolumeId) -> Result<Vec<KindBytes>> {
        self.ensure_dir_sizes(volume_id)?;
        let db = self.volume_db(volume_id)?;
        let mut kinds: Vec<KindBytes> = db
            .info("kinds")?
            .and_then(|j| serde_json::from_str(&j).ok())
            .unwrap_or_default();
        kinds.sort_by_key(|k| std::cmp::Reverse(k.bytes));
        Ok(kinds)
    }

    /// One level of the folder treemap: a folder, its subfolders by size and its largest
    /// files. `dir_id = None` starts at the root of the drive.
    pub fn folder_tree(&self, volume_id: VolumeId, dir_id: Option<i64>) -> Result<FolderTree> {
        self.ensure_dir_sizes(volume_id)?;
        let db = self.volume_db(volume_id)?;
        let conn = db.conn();
        let dir_id = match dir_id {
            Some(d) => d,
            None => conn
                .query_row(
                    "SELECT id FROM dirs WHERE parent_id IS NULL ORDER BY id LIMIT 1",
                    [],
                    |r| r.get(0),
                )
                .optional()?
                .ok_or_else(|| Error::Other("this drive has not been scanned yet".into()))?,
        };
        let node_sql =
            "SELECT d.id, d.name, d.path, s.bytes, s.files, s.dirs, s.newest, s.top_kind,
                               s.own_bytes, s.own_files, d.parent_id
                        FROM dirs d JOIN dir_sizes s ON s.dir_id = d.id";
        let row_to_node =
            |r: &rusqlite::Row| -> rusqlite::Result<(FolderNode, i64, i64, Option<i64>)> {
                Ok((
                    FolderNode {
                        id: r.get(0)?,
                        name: r.get(1)?,
                        path: r.get(2)?,
                        bytes: r.get::<_, i64>(3)? as u64,
                        files: r.get::<_, i64>(4)? as u64,
                        dirs: r.get::<_, i64>(5)? as u64,
                        newest: r.get(6)?,
                        top_kind: Kind::from_u8(r.get::<_, i64>(7)? as u8),
                    },
                    r.get(8)?,
                    r.get(9)?,
                    r.get(10)?,
                ))
            };
        let (node, own_bytes, own_files, mut parent) = conn
            .query_row(
                &format!("{node_sql} WHERE d.id = ?1"),
                [dir_id],
                row_to_node,
            )
            .optional()?
            .ok_or_else(|| Error::Other(format!("folder {dir_id} not found")))?;

        let mut crumbs = vec![FolderCrumb {
            id: node.id,
            name: node.name.clone(),
        }];
        while let Some(p) = parent {
            let row: Option<(String, Option<i64>)> = conn
                .query_row("SELECT name, parent_id FROM dirs WHERE id = ?1", [p], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })
                .optional()?;
            let Some((name, next)) = row else { break };
            crumbs.push(FolderCrumb { id: p, name });
            parent = next.filter(|&n| n != p);
        }
        crumbs.reverse();
        if crumbs[0].name.is_empty() {
            if let Some(v) = self.master().get_volume(volume_id)? {
                crumbs[0].name = v.label;
            }
        }

        let mut stmt = conn.prepare(&format!(
            "{node_sql} WHERE d.parent_id = ?1 AND s.bytes > 0 ORDER BY s.bytes DESC LIMIT {}",
            MAX_CHILDREN + 1
        ))?;
        let mut children: Vec<FolderNode> = stmt
            .query_map([dir_id], row_to_node)?
            .map(|r| r.map(|t| t.0))
            .collect::<std::result::Result<_, _>>()?;
        let mut rest_bytes = 0u64;
        let mut rest_count = 0u64;
        if children.len() > MAX_CHILDREN {
            // The tail is summed below into "everything else".
            children.truncate(MAX_CHILDREN);
            let shown: u64 = children.iter().map(|c| c.bytes).sum();
            let all: i64 = conn.query_row(
                "SELECT COALESCE(SUM(s.bytes), 0) FROM dirs d JOIN dir_sizes s ON s.dir_id = d.id
                 WHERE d.parent_id = ?1",
                [dir_id],
                |r| r.get(0),
            )?;
            rest_bytes += (all as u64).saturating_sub(shown);
            let count: i64 = conn.query_row(
                "SELECT COUNT(*) FROM dirs WHERE parent_id = ?1",
                [dir_id],
                |r| r.get(0),
            )?;
            rest_count += (count as u64).saturating_sub(MAX_CHILDREN as u64);
        }

        let mut fstmt = conn.prepare(&format!(
            "SELECT id, name, size, kind, mtime FROM files WHERE dir_id = ?1 AND size > 0
             ORDER BY size DESC LIMIT {MAX_FILES}"
        ))?;
        let files: Vec<FileLeaf> = fstmt
            .query_map([dir_id], |r| {
                Ok(FileLeaf {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    size: r.get::<_, i64>(2)? as u64,
                    kind: Kind::from_u8(r.get::<_, i64>(3)? as u8),
                    mtime: r.get(4)?,
                })
            })?
            .collect::<std::result::Result<_, _>>()?;
        let shown_files: u64 = files.iter().map(|f| f.size).sum();
        rest_bytes += (own_bytes as u64).saturating_sub(shown_files);
        rest_count += (own_files as u64).saturating_sub(files.len() as u64);

        Ok(FolderTree {
            volume_id,
            node,
            crumbs,
            children,
            files,
            rest_bytes,
            rest_count,
        })
    }

    /// Check every folder volume (local folders and network shares) for reachability and
    /// free space, each on its own short-lived thread with a timeout. Returns whether any
    /// volume came online or went offline. `is_remote` comes from the platform layer.
    pub fn refresh_folders(&self, is_remote: &(dyn Fn(&str) -> bool + Sync)) -> Result<bool> {
        let folders: Vec<Volume> = self
            .master()
            .list_volumes()?
            .into_iter()
            .filter(|v| v.source == VolumeSource::Folder)
            .collect();
        let detected = self.detected();
        let mut fresh: HashMap<VolumeId, FolderStatus> = HashMap::new();
        for v in &folders {
            let Some(path) = v.last_mount_path.clone() else {
                continue;
            };
            let remote = is_network_path(&path, v.media_type) || is_remote(&path);
            let status = if remote {
                let (tx, rx) = mpsc::channel();
                let p = path.clone();
                // Detached on purpose: if the share hangs, the thread finishes (or not) on
                // its own and we move on after the timeout.
                let _ = std::thread::Builder::new()
                    .name("storecat-reach".into())
                    .spawn(move || {
                        let _ = tx.send(probe(&p));
                    });
                rx.recv_timeout(REACH_TIMEOUT).unwrap_or(FolderStatus {
                    online: false,
                    free: 0,
                    capacity: 0,
                })
            } else {
                probe(&path)
            };
            if status.online && status.capacity > 0 {
                // A share is network storage; a local folder is whatever drive it lives on.
                let device = (!remote)
                    .then(|| containing_device(&detected, &path))
                    .flatten();
                let media = if remote {
                    Some(MediaType::Network)
                } else {
                    device
                        .map(|d| d.media_type)
                        .filter(|m| *m != MediaType::Unknown)
                };
                self.master().set_folder_space(
                    v.id,
                    status.free,
                    status.capacity,
                    media,
                    device,
                )?;
            }
            fresh.insert(v.id, status);
        }
        let mut cur = self
            .folder_status
            .write()
            .unwrap_or_else(|e| e.into_inner());
        let changed = fresh
            .iter()
            .any(|(id, s)| cur.get(id).map(|c| c.online) != Some(s.online));
        *cur = fresh;
        Ok(changed)
    }
}

/// The mounted drive whose mount point is the longest prefix of `path`.
fn containing_device<'a>(detected: &'a [DetectedVolume], path: &str) -> Option<&'a DetectedVolume> {
    let norm = |s: &str| s.replace('\\', "/").trim_end_matches('/').to_lowercase();
    let p = norm(path);
    detected
        .iter()
        .filter(|d| {
            let m = norm(&d.mount_path);
            p == m || p.starts_with(&format!("{m}/")) || m.is_empty()
        })
        .max_by_key(|d| d.mount_path.len())
}

fn probe(path: &str) -> FolderStatus {
    let p = Path::new(path);
    if !p.is_dir() {
        return FolderStatus {
            online: false,
            free: 0,
            capacity: 0,
        };
    }
    FolderStatus {
        online: true,
        free: fs4::available_space(p).unwrap_or(0),
        capacity: fs4::total_space(p).unwrap_or(0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::ScanOptions;

    #[test]
    fn folder_sizes_roll_up_and_treemap_levels() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("drive");
        std::fs::create_dir_all(root.join("Videos/2019/trip")).unwrap();
        std::fs::create_dir_all(root.join("Docs")).unwrap();
        std::fs::write(root.join("Videos/2019/trip/a.mp4"), vec![0u8; 5000]).unwrap();
        std::fs::write(root.join("Videos/2019/b.mkv"), vec![0u8; 3000]).unwrap();
        std::fs::write(root.join("Docs/c.pdf"), vec![0u8; 100]).unwrap();
        std::fs::write(root.join("top.txt"), vec![0u8; 10]).unwrap();

        let cat = Catalog::open(crate::DataDirs::at(tmp.path().join("data"))).unwrap();
        let v = cat.add_folder(&root, Some("Test")).unwrap();
        let opts = ScanOptions {
            threads: 2,
            ..Default::default()
        };
        cat.scan(v.id, &opts, &|_| {}).unwrap();

        let tree = cat.folder_tree(v.id, None).unwrap();
        assert_eq!(tree.node.bytes, 8110);
        assert_eq!(tree.node.files, 4);
        assert_eq!(tree.children[0].name, "Videos");
        assert_eq!(tree.children[0].bytes, 8000);
        assert_eq!(tree.children[0].top_kind, Kind::Video);
        assert_eq!(tree.files.len(), 1);
        assert_eq!(tree.crumbs.len(), 1);

        let videos = cat.folder_tree(v.id, Some(tree.children[0].id)).unwrap();
        let y2019 = cat.folder_tree(v.id, Some(videos.children[0].id)).unwrap();
        assert_eq!(y2019.crumbs.len(), 3);
        assert_eq!(y2019.children[0].name, "trip");
        assert_eq!(y2019.files[0].name, "b.mkv");

        let kinds = cat.kind_breakdown(v.id).unwrap();
        assert_eq!(kinds[0].kind, Kind::Video);
        assert_eq!(kinds[0].bytes, 8000);

        // Older catalogues get the table built on first use.
        let path = cat.dirs.volume_db(v.id);
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch("DROP TABLE dir_sizes; DELETE FROM info WHERE key = 'dir_sizes';")
            .unwrap();
        drop(conn);
        assert_eq!(cat.folder_tree(v.id, None).unwrap().node.bytes, 8110);
    }
}
