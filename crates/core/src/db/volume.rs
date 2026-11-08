//! One SQLite file per catalogued volume: `volumes/<id>.db`, plus `volumes/<id>.thumbs.db`
//! for thumbnails so the catalogue itself stays small and exportable.
//!
//! The scan writer builds a fresh file with durability off (it is renamed into place only
//! on success). Enrichment then opens it read-write in WAL mode. Search never touches it.

use std::path::Path;

use rusqlite::{params, Connection, OpenFlags, OptionalExtension};

use crate::error::Result;
use crate::model::Kind;
use crate::scan::DirBatch;

pub const VOLUME_SCHEMA: i64 = 2;

pub struct VolumeDbWriter {
    conn: Connection,
    in_tx: bool,
    pending: usize,
}

const ROWS_PER_TX: usize = 50_000;

const ENRICHMENT_TABLES: &str = "
    CREATE TABLE IF NOT EXISTS hashes (
        file_id  INTEGER PRIMARY KEY,
        quick    BLOB,
        full     BLOB,
        quick_at INTEGER,
        full_at  INTEGER
    );
    CREATE TABLE IF NOT EXISTS meta (
        file_id INTEGER PRIMARY KEY,
        json    TEXT NOT NULL
    );
    CREATE TABLE IF NOT EXISTS text (
        file_id INTEGER PRIMARY KEY,
        content TEXT NOT NULL
    );
    CREATE TABLE IF NOT EXISTS archive_entries (
        id      INTEGER PRIMARY KEY,
        file_id INTEGER NOT NULL,
        path    TEXT NOT NULL,
        size    INTEGER NOT NULL DEFAULT 0,
        mtime   INTEGER NOT NULL DEFAULT 0,
        is_dir  INTEGER NOT NULL DEFAULT 0
    );
    CREATE TABLE IF NOT EXISTS enrich (
        file_id INTEGER PRIMARY KEY,
        done    INTEGER NOT NULL DEFAULT 0,
        stages  INTEGER NOT NULL DEFAULT 0,
        error   TEXT,
        at      INTEGER
    );
    -- Where each file's data starts on a spinning disk (NULL: it has no clusters of its
    -- own), so a resumed enrichment doesn't look every file up again.
    CREATE TABLE IF NOT EXISTS diskpos (
        file_id INTEGER PRIMARY KEY,
        lcn     INTEGER
    );";

impl VolumeDbWriter {
    pub fn create(path: &Path) -> Result<Self> {
        if path.exists() {
            std::fs::remove_file(path)?;
        }
        let conn = Connection::open(path)?;
        conn.execute_batch(
            "PRAGMA journal_mode = OFF;
             PRAGMA synchronous = OFF;
             PRAGMA temp_store = MEMORY;
             PRAGMA cache_size = -65536;
             PRAGMA page_size = 4096;
             CREATE TABLE dirs (
                 id        INTEGER PRIMARY KEY,
                 parent_id INTEGER,
                 name      TEXT NOT NULL,
                 path      TEXT NOT NULL,
                 mtime     INTEGER NOT NULL DEFAULT 0
             );
             CREATE TABLE files (
                 id     INTEGER PRIMARY KEY,
                 dir_id INTEGER NOT NULL,
                 name   TEXT NOT NULL,
                 ext    TEXT NOT NULL DEFAULT '',
                 size   INTEGER NOT NULL DEFAULT 0,
                 mtime  INTEGER NOT NULL DEFAULT 0,
                 ctime  INTEGER NOT NULL DEFAULT 0,
                 attrs  INTEGER NOT NULL DEFAULT 0,
                 kind   INTEGER NOT NULL DEFAULT 0
             );
             CREATE TABLE info (
                 key   TEXT PRIMARY KEY,
                 value TEXT NOT NULL
             );",
        )?;
        conn.execute_batch(ENRICHMENT_TABLES)?;
        let mut w = Self {
            conn,
            in_tx: false,
            pending: 0,
        };
        w.set_info("schema", &VOLUME_SCHEMA.to_string())?;
        Ok(w)
    }

    fn ensure_tx(&mut self) -> Result<()> {
        if !self.in_tx {
            self.conn.execute_batch("BEGIN")?;
            self.in_tx = true;
        }
        Ok(())
    }

    fn maybe_commit(&mut self) -> Result<()> {
        if self.in_tx && self.pending >= ROWS_PER_TX {
            self.conn.execute_batch("COMMIT")?;
            self.in_tx = false;
            self.pending = 0;
        }
        Ok(())
    }

    pub fn write_batch(&mut self, batch: &DirBatch) -> Result<()> {
        self.ensure_tx()?;
        {
            let mut dir_stmt = self.conn.prepare_cached(
                "INSERT INTO dirs (id, parent_id, name, path, mtime) VALUES (?1, ?2, ?3, ?4, ?5)",
            )?;
            dir_stmt.execute(params![
                batch.dir_id,
                batch.parent_id,
                batch.name,
                batch.rel_path,
                batch.mtime
            ])?;
            let mut file_stmt = self.conn.prepare_cached(
                "INSERT INTO files (id, dir_id, name, ext, size, mtime, ctime, attrs, kind)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            )?;
            for f in &batch.files {
                file_stmt.execute(params![
                    f.id,
                    batch.dir_id,
                    f.name,
                    f.ext,
                    f.size as i64,
                    f.mtime,
                    f.ctime,
                    f.attrs as i64,
                    f.kind as u8 as i64
                ])?;
            }
        }
        self.pending += 1 + batch.files.len();
        self.maybe_commit()
    }

    pub fn set_info(&mut self, key: &str, value: &str) -> Result<()> {
        self.ensure_tx()?;
        self.conn.execute(
            "INSERT INTO info (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    /// Commit and build indexes. Indexes are created last: bulk inserts into an unindexed
    /// table are several times faster.
    pub fn finish(mut self) -> Result<()> {
        if self.in_tx {
            self.conn.execute_batch("COMMIT")?;
            self.in_tx = false;
        }
        self.conn.execute_batch(
            "CREATE INDEX files_dir ON files(dir_id);
             CREATE INDEX files_dir_name ON files(dir_id, name);
             CREATE INDEX dirs_parent ON dirs(parent_id);
             CREATE INDEX dirs_path ON dirs(path);
             CREATE INDEX archive_entries_file ON archive_entries(file_id);",
        )?;
        crate::storage::build_dir_sizes(&self.conn)?;
        self.conn.execute_batch("PRAGMA journal_mode = DELETE;")?;
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct FileRecord {
    pub id: i64,
    pub dir_id: i64,
    pub name: String,
    pub path: String,
    pub ext: String,
    pub size: u64,
    pub mtime: i64,
    pub ctime: i64,
    pub attrs: u32,
    pub kind: Kind,
}

/// The columns enrichment planning needs, without the paths.
#[derive(Debug, Clone)]
pub struct PlanRow {
    pub id: i64,
    pub dir_id: i64,
    pub ext: String,
    pub size: u64,
    pub kind: Kind,
    pub attrs: u32,
    pub done: u8,
    pub stages: u32,
}

#[derive(Debug, Clone)]
pub struct DirRecord {
    pub id: i64,
    pub parent_id: Option<i64>,
    pub name: String,
    pub path: String,
    pub mtime: i64,
}

#[derive(Debug, Clone)]
pub struct ArchiveEntryRecord {
    pub id: i64,
    pub file_id: i64,
    pub path: String,
    pub size: u64,
    pub mtime: i64,
    pub is_dir: bool,
}

/// `(quick, full)` hashes of a file, either of which may be missing.
pub type HashPair = (Option<Vec<u8>>, Option<Vec<u8>>);

/// Enrichment payload for one file, written in one transaction by the enrichment writer.
#[derive(Debug, Default)]
pub struct EnrichRow {
    pub file_id: i64,
    pub quick_hash: Option<[u8; 32]>,
    pub meta_json: Option<String>,
    pub text: Option<String>,
    pub thumb: Option<(Vec<u8>, u32, u32)>,
    pub entries: Option<Vec<storecat_extract::ArchiveEntry>>,
    pub stages: u32,
    pub error: Option<String>,
}

/// Open connection to a volume catalogue. Read-only by default; `open_rw` for enrichment.
pub struct VolumeDb {
    conn: Connection,
}

const FILE_COLS: &str =
    "f.id, f.dir_id, f.name, d.path, f.ext, f.size, f.mtime, f.ctime, f.attrs, f.kind";

fn row_to_file(r: &rusqlite::Row) -> rusqlite::Result<FileRecord> {
    Ok(FileRecord {
        id: r.get(0)?,
        dir_id: r.get(1)?,
        name: r.get(2)?,
        path: r.get(3)?,
        ext: r.get(4)?,
        size: r.get::<_, i64>(5)? as u64,
        mtime: r.get(6)?,
        ctime: r.get(7)?,
        attrs: r.get::<_, i64>(8)? as u32,
        kind: Kind::from_u8(r.get::<_, i64>(9)? as u8),
    })
}

fn row_to_dir(r: &rusqlite::Row) -> rusqlite::Result<DirRecord> {
    Ok(DirRecord {
        id: r.get(0)?,
        parent_id: r.get(1)?,
        name: r.get(2)?,
        path: r.get(3)?,
        mtime: r.get(4)?,
    })
}

impl VolumeDb {
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        Ok(Self { conn })
    }

    /// Schema version recorded in the file (1 for Phase 1 catalogues).
    pub fn schema_version(path: &Path) -> Result<i64> {
        let db = Self::open(path)?;
        Ok(db.info("schema")?.and_then(|s| s.parse().ok()).unwrap_or(1))
    }

    /// Bring an older catalogue up to the current schema in place.
    pub fn ensure_schema(path: &Path) -> Result<()> {
        if Self::schema_version(path)? < VOLUME_SCHEMA {
            let db = Self::open_rw(path)?;
            db.set_info("schema", &VOLUME_SCHEMA.to_string())?;
        }
        Ok(())
    }

    /// Read-write, WAL, with the enrichment tables present (upgrades schema-1 catalogues).
    pub fn open_rw(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA temp_store = MEMORY;
             PRAGMA cache_size = -32768;
             PRAGMA journal_size_limit = 67108864;",
        )?;
        conn.execute_batch(ENRICHMENT_TABLES)?;
        conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS archive_entries_file ON archive_entries(file_id);
             CREATE INDEX IF NOT EXISTS dirs_path ON dirs(path);
             CREATE INDEX IF NOT EXISTS files_dir_name ON files(dir_id, name);",
        )?;
        Ok(Self { conn })
    }

    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    pub fn file(&self, id: i64) -> Result<Option<FileRecord>> {
        let mut stmt = self.conn.prepare_cached(&format!(
            "SELECT {FILE_COLS} FROM files f JOIN dirs d ON d.id = f.dir_id WHERE f.id = ?1"
        ))?;
        Ok(stmt.query_row([id], row_to_file).optional()?)
    }

    /// Look a file up by its catalogue path (`dir/path/name`, `/`-separated).
    pub fn file_by_path(&self, path: &str) -> Result<Option<FileRecord>> {
        let (dir, name) = match path.rfind('/') {
            Some(i) => (&path[..i], &path[i + 1..]),
            None => ("", path),
        };
        let mut stmt = self.conn.prepare_cached(&format!(
            "SELECT {FILE_COLS} FROM files f JOIN dirs d ON d.id = f.dir_id
             WHERE d.path = ?1 AND f.name = ?2 LIMIT 1"
        ))?;
        Ok(stmt.query_row(params![dir, name], row_to_file).optional()?)
    }

    pub fn dir_by_path(&self, path: &str) -> Result<Option<DirRecord>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT id, parent_id, name, path, mtime FROM dirs WHERE path = ?1 LIMIT 1",
        )?;
        Ok(stmt.query_row([path], row_to_dir).optional()?)
    }

    pub fn dir(&self, id: i64) -> Result<Option<DirRecord>> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT id, parent_id, name, path, mtime FROM dirs WHERE id = ?1")?;
        Ok(stmt.query_row([id], row_to_dir).optional()?)
    }

    pub fn archive_entry(&self, id: i64) -> Result<Option<ArchiveEntryRecord>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT id, file_id, path, size, mtime, is_dir FROM archive_entries WHERE id = ?1",
        )?;
        Ok(stmt
            .query_row([id], |r| {
                Ok(ArchiveEntryRecord {
                    id: r.get(0)?,
                    file_id: r.get(1)?,
                    path: r.get(2)?,
                    size: r.get::<_, i64>(3)? as u64,
                    mtime: r.get(4)?,
                    is_dir: r.get::<_, i64>(5)? != 0,
                })
            })
            .optional()?)
    }

    pub fn archive_entry_count(&self, file_id: i64) -> Result<u64> {
        let n: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM archive_entries WHERE file_id = ?1",
            [file_id],
            |r| r.get(0),
        )?;
        Ok(n as u64)
    }

    /// Direct children counts of a directory.
    pub fn child_counts(&self, dir_id: i64) -> Result<(u64, u64)> {
        let dirs: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM dirs WHERE parent_id = ?1",
            [dir_id],
            |r| r.get(0),
        )?;
        let files: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM files WHERE dir_id = ?1",
            [dir_id],
            |r| r.get(0),
        )?;
        Ok((dirs as u64, files as u64))
    }

    pub fn children(&self, dir_id: i64) -> Result<(Vec<DirRecord>, Vec<FileRecord>)> {
        let mut ds = self.conn.prepare_cached(
            "SELECT id, parent_id, name, path, mtime FROM dirs WHERE parent_id = ?1 ORDER BY name COLLATE NOCASE",
        )?;
        let dirs = ds
            .query_map([dir_id], row_to_dir)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let mut fs = self.conn.prepare_cached(&format!(
            "SELECT {FILE_COLS} FROM files f JOIN dirs d ON d.id = f.dir_id WHERE f.dir_id = ?1 ORDER BY f.name COLLATE NOCASE"
        ))?;
        let files = fs
            .query_map([dir_id], row_to_file)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok((dirs, files))
    }

    pub fn info(&self, key: &str) -> Result<Option<String>> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT value FROM info WHERE key = ?1")?;
        Ok(stmt.query_row([key], |r| r.get(0)).optional()?)
    }

    pub fn set_info(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO info (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn counts(&self) -> Result<(u64, u64)> {
        let files: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))?;
        let dirs: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM dirs", [], |r| r.get(0))?;
        Ok((files as u64, dirs as u64))
    }

    // ----- enrichment reads ---------------------------------------------------------------

    pub fn meta_json(&self, file_id: i64) -> Result<Option<String>> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT json FROM meta WHERE file_id = ?1")?;
        Ok(stmt.query_row([file_id], |r| r.get(0)).optional()?)
    }

    pub fn text(&self, file_id: i64) -> Result<Option<String>> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT content FROM text WHERE file_id = ?1")?;
        Ok(stmt.query_row([file_id], |r| r.get(0)).optional()?)
    }

    pub fn hashes(&self, file_id: i64) -> Result<HashPair> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT quick, full FROM hashes WHERE file_id = ?1")?;
        Ok(stmt
            .query_row([file_id], |r| Ok((r.get(0)?, r.get(1)?)))
            .optional()?
            .unwrap_or((None, None)))
    }

    pub fn has_thumb_flag(&self, file_id: i64) -> Result<bool> {
        let stages: Option<i64> = self
            .conn
            .query_row(
                "SELECT stages FROM enrich WHERE file_id = ?1",
                [file_id],
                |r| r.get(0),
            )
            .optional()?;
        Ok(stages.map(|s| s & STAGE_THUMB as i64 != 0).unwrap_or(false))
    }

    /// `done` counts fully enriched files; the stage counts include files whose second pass
    /// (text, video thumbnails) is still to come.
    pub fn coverage(&self) -> Result<crate::model::EnrichCoverage> {
        let files: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))?;
        let (done, thumbs, texts, hashed): (i64, i64, i64, i64) = self.conn.query_row(
            "SELECT SUM(CASE WHEN done = 1 THEN 1 ELSE 0 END),
                    SUM(CASE WHEN stages & ?1 THEN 1 ELSE 0 END),
                    SUM(CASE WHEN stages & ?2 THEN 1 ELSE 0 END),
                    SUM(CASE WHEN stages & ?3 THEN 1 ELSE 0 END)
             FROM enrich WHERE done >= 1",
            params![STAGE_THUMB as i64, STAGE_TEXT as i64, STAGE_HASH as i64],
            |r| {
                Ok((
                    r.get::<_, Option<i64>>(0)?.unwrap_or(0),
                    r.get::<_, Option<i64>>(1)?.unwrap_or(0),
                    r.get::<_, Option<i64>>(2)?.unwrap_or(0),
                    r.get::<_, Option<i64>>(3)?.unwrap_or(0),
                ))
            },
        )?;
        Ok(crate::model::EnrichCoverage {
            files: files as u64,
            done: done as u64,
            thumbs: thumbs as u64,
            texts: texts as u64,
            hashed: hashed as u64,
        })
    }

    /// Every file whose enrichment is not finished, streamed to `each` in id order. One
    /// pass over the table per enrichment pass, instead of one query per batch.
    pub fn plan_rows(&self, mut each: impl FnMut(PlanRow) -> Result<()>) -> Result<()> {
        let mut stmt = self.conn.prepare(
            "SELECT f.id, f.dir_id, f.ext, f.size, f.kind, f.attrs,
                    COALESCE(e.done, 0), COALESCE(e.stages, 0)
             FROM files f LEFT JOIN enrich e ON e.file_id = f.id
             WHERE e.file_id IS NULL OR e.done != 1",
        )?;
        let mut rows = stmt.query([])?;
        while let Some(r) = rows.next()? {
            each(PlanRow {
                id: r.get(0)?,
                dir_id: r.get(1)?,
                ext: r.get(2)?,
                size: r.get::<_, i64>(3)? as u64,
                kind: Kind::from_u8(r.get::<_, i64>(4)? as u8),
                attrs: r.get::<_, i64>(5)? as u32,
                done: r.get::<_, i64>(6)? as u8,
                stages: r.get::<_, i64>(7)? as u32,
            })?;
        }
        Ok(())
    }

    /// Everything the index needs to (re)build documents for this volume, streamed in
    /// batches by file id so memory stays flat.
    pub fn files_after(&self, after_id: i64, limit: usize) -> Result<Vec<FileRecord>> {
        let mut stmt = self.conn.prepare_cached(&format!(
            "SELECT {FILE_COLS} FROM files f JOIN dirs d ON d.id = f.dir_id
             WHERE f.id > ?1 ORDER BY f.id LIMIT ?2"
        ))?;
        let rows = stmt.query_map(params![after_id, limit as i64], row_to_file)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub fn dirs_after(&self, after_id: i64, limit: usize) -> Result<Vec<DirRecord>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT id, parent_id, name, path, mtime FROM dirs WHERE id > ?1 ORDER BY id LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![after_id, limit as i64], row_to_dir)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub fn entries_for(&self, file_id: i64) -> Result<Vec<ArchiveEntryRecord>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT id, file_id, path, size, mtime, is_dir FROM archive_entries WHERE file_id = ?1 ORDER BY id",
        )?;
        let rows = stmt.query_map([file_id], |r| {
            Ok(ArchiveEntryRecord {
                id: r.get(0)?,
                file_id: r.get(1)?,
                path: r.get(2)?,
                size: r.get::<_, i64>(3)? as u64,
                mtime: r.get(4)?,
                is_dir: r.get::<_, i64>(5)? != 0,
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Stage bits and existence of enrichment data for a file (for index flags).
    pub fn stages(&self, file_id: i64) -> Result<u32> {
        let s: Option<i64> = self
            .conn
            .query_row(
                "SELECT stages FROM enrich WHERE file_id = ?1",
                [file_id],
                |r| r.get(0),
            )
            .optional()?;
        Ok(s.unwrap_or(0) as u32)
    }

    /// Ids of files that have any enrichment worth re-indexing (text, meta, thumb, entries).
    pub fn enriched_file_ids(&self) -> Result<Vec<i64>> {
        let mut stmt = self.conn.prepare(
            "SELECT file_id FROM enrich WHERE done >= 1 AND stages & ?1 != 0 ORDER BY file_id",
        )?;
        let mask = (STAGE_THUMB | STAGE_TEXT | STAGE_META | STAGE_ARCHIVE | STAGE_HASH) as i64;
        let rows = stmt.query_map([mask], |r| r.get::<_, i64>(0))?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Quick hashes for the duplicates index: files at or above `min_size`.
    pub fn quick_hashes(&self, min_size: u64) -> Result<Vec<(Vec<u8>, u64, i64)>> {
        let mut stmt = self.conn.prepare(
            "SELECT h.quick, f.size, f.id FROM hashes h JOIN files f ON f.id = h.file_id
             WHERE h.quick IS NOT NULL AND f.size >= ?1",
        )?;
        let rows = stmt.query_map([min_size as i64], |r| {
            Ok((
                r.get::<_, Vec<u8>>(0)?,
                r.get::<_, i64>(1)? as u64,
                r.get::<_, i64>(2)?,
            ))
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    // ----- enrichment writes -------------------------------------------------------------

    pub fn begin(&self) -> Result<()> {
        self.conn.execute_batch("BEGIN")?;
        Ok(())
    }

    pub fn commit(&self) -> Result<()> {
        self.conn.execute_batch("COMMIT")?;
        Ok(())
    }

    /// Throw away the open transaction (the drive went away mid-batch).
    pub fn rollback(&self) -> Result<()> {
        self.conn.execute_batch("ROLLBACK")?;
        Ok(())
    }

    /// Write one file's enrichment. Archive entries are replaced; returns their new ids.
    /// `done` is [`ENRICH_DONE`] or [`ENRICH_DETAIL_PENDING`]; stage bits accumulate.
    /// Returns `None`, writing nothing, when the file has left the catalogue meanwhile
    /// (deleted or changed on disk while enrichment read it).
    pub fn write_enrichment(
        &self,
        row: &EnrichRow,
        done: u8,
        now: i64,
    ) -> Result<Option<Vec<(i64, storecat_extract::ArchiveEntry)>>> {
        if !self.has_file(row.file_id)? {
            return Ok(None);
        }
        if let Some(q) = &row.quick_hash {
            self.conn.execute(
                "INSERT INTO hashes (file_id, quick, quick_at) VALUES (?1, ?2, ?3)
                 ON CONFLICT(file_id) DO UPDATE SET quick = excluded.quick, quick_at = excluded.quick_at",
                params![row.file_id, &q[..], now],
            )?;
        }
        if let Some(j) = &row.meta_json {
            self.conn.execute(
                "INSERT INTO meta (file_id, json) VALUES (?1, ?2)
                 ON CONFLICT(file_id) DO UPDATE SET json = excluded.json",
                params![row.file_id, j],
            )?;
        }
        if let Some(t) = &row.text {
            self.conn.execute(
                "INSERT INTO text (file_id, content) VALUES (?1, ?2)
                 ON CONFLICT(file_id) DO UPDATE SET content = excluded.content",
                params![row.file_id, t],
            )?;
        }
        let mut new_entries = Vec::new();
        if let Some(entries) = &row.entries {
            self.conn.execute(
                "DELETE FROM archive_entries WHERE file_id = ?1",
                [row.file_id],
            )?;
            let mut ins = self.conn.prepare_cached(
                "INSERT INTO archive_entries (file_id, path, size, mtime, is_dir) VALUES (?1, ?2, ?3, ?4, ?5)",
            )?;
            for e in entries {
                ins.execute(params![
                    row.file_id,
                    e.path,
                    e.size as i64,
                    e.mtime,
                    e.is_dir as i64
                ])?;
                new_entries.push((self.conn.last_insert_rowid(), e.clone()));
            }
        }
        self.conn.execute(
            "INSERT INTO enrich (file_id, done, stages, error, at) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(file_id) DO UPDATE SET done = excluded.done,
                 stages = enrich.stages | excluded.stages,
                 error = COALESCE(excluded.error, enrich.error), at = excluded.at",
            params![row.file_id, done as i64, row.stages as i64, row.error, now],
        )?;
        Ok(Some(new_entries))
    }

    /// Disk positions remembered from earlier enrichment runs.
    pub fn disk_positions(&self) -> Result<std::collections::HashMap<i64, Option<u64>>> {
        let mut stmt = self.conn.prepare("SELECT file_id, lcn FROM diskpos")?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, Option<i64>>(1)?.map(|n| n as u64),
            ))
        })?;
        let mut out = std::collections::HashMap::new();
        for row in rows {
            let (id, lcn) = row?;
            out.insert(id, lcn);
        }
        Ok(out)
    }

    /// Remember disk positions looked up while planning.
    pub fn save_disk_positions(&self, found: &[(i64, Option<u64>)]) -> Result<()> {
        if found.is_empty() {
            return Ok(());
        }
        self.conn.execute_batch("BEGIN")?;
        let result = (|| -> Result<()> {
            let mut ins = self
                .conn
                .prepare_cached("INSERT OR REPLACE INTO diskpos (file_id, lcn) VALUES (?1, ?2)")?;
            for (id, lcn) in found {
                ins.execute(params![id, lcn.map(|n| n as i64)])?;
            }
            Ok(())
        })();
        match result {
            Ok(()) => {
                self.conn.execute_batch("COMMIT")?;
                Ok(())
            }
            Err(e) => {
                let _ = self.conn.execute_batch("ROLLBACK");
                Err(e)
            }
        }
    }

    /// Forget disk positions once nothing is left to read (and for files that are gone).
    pub fn clear_disk_positions(&self) -> Result<()> {
        self.conn.execute_batch("DELETE FROM diskpos")?;
        Ok(())
    }

    pub fn has_file(&self, file_id: i64) -> Result<bool> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT EXISTS(SELECT 1 FROM files WHERE id = ?1)")?;
        Ok(stmt.query_row([file_id], |r| r.get::<_, i64>(0))? != 0)
    }

    /// Record files that need no reading at all (or only the second pass) in one go.
    /// Call inside `begin`/`commit`.
    /// Files whose enrichment was written at or after `at` (unix seconds).
    pub fn enriched_since(&self, at: i64) -> Result<Vec<i64>> {
        let mut stmt = self
            .conn
            .prepare("SELECT file_id FROM enrich WHERE done >= 1 AND at >= ?1")?;
        let ids = stmt
            .query_map([at], |r| r.get(0))?
            .collect::<std::result::Result<Vec<i64>, _>>()?;
        Ok(ids)
    }

    pub fn mark_enriched(&self, file_id: i64, done: u8, stages: u32, now: i64) -> Result<()> {
        let mut stmt = self.conn.prepare_cached(
            "INSERT INTO enrich (file_id, done, stages, at)
             SELECT ?1, ?2, ?3, ?4 WHERE EXISTS (SELECT 1 FROM files WHERE id = ?1)
             ON CONFLICT(file_id) DO UPDATE SET done = excluded.done,
                 stages = enrich.stages | excluded.stages, at = excluded.at",
        )?;
        stmt.execute(params![file_id, done as i64, stages as i64, now])?;
        Ok(())
    }

    pub fn set_full_hash(&self, file_id: i64, full: &[u8; 32], now: i64) -> Result<()> {
        self.conn.execute(
            "INSERT INTO hashes (file_id, full, full_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(file_id) DO UPDATE SET full = excluded.full, full_at = excluded.full_at",
            params![file_id, &full[..], now],
        )?;
        Ok(())
    }

    /// Files per (kind, extension), leaving out macOS `._` companions. Cached in `info`
    /// until the catalogue changes (a rescan writes a fresh one).
    pub fn extension_counts(&self) -> Result<Vec<(u8, String, u64)>> {
        let stamp = self.info("scanned_at")?.unwrap_or_default();
        if let (Some(at), Some(json)) = (self.info("exts_at")?, self.info("exts")?) {
            if at == stamp {
                if let Ok(v) = serde_json::from_str(&json) {
                    return Ok(v);
                }
            }
        }
        let mut stmt = self.conn.prepare(
            "SELECT kind, ext, COUNT(*) FROM files
             WHERE ext != '' AND substr(name, 1, 2) != '._'
             GROUP BY kind, ext",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)? as u8,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)? as u64,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        // Read-only connections can't cache; that only costs the query next time.
        if let Ok(json) = serde_json::to_string(&rows) {
            let _ = self.set_info("exts", &json);
            let _ = self.set_info("exts_at", &stamp);
        }
        Ok(rows)
    }

    /// Bring enrichment from an older format up to date without redoing it: files whose
    /// format carries artwork (album art, document previews) but that have no thumbnail
    /// are queued for the thumbnail alone. Returns how many were queued.
    pub fn upgrade_enrichment(&self) -> Result<usize> {
        let format = self
            .info(ENRICH_FORMAT_KEY)?
            .and_then(|v| v.parse::<u32>().ok())
            .unwrap_or(1);
        if format >= ENRICH_FORMAT {
            return Ok(0);
        }
        let mut queued = 0;
        if format < 3 {
            // FFmpeg read nothing from Flash videos whose tag sizes were stored backwards
            // (it now ignores them): look at those again.
            queued += self.conn.execute(
                "UPDATE enrich SET done = ?1 WHERE done = 1 AND (stages & ?2) = 0
                 AND file_id IN (SELECT id FROM files WHERE ext = 'flv')",
                params![ENRICH_DETAIL_PENDING as i64, STAGE_THUMB as i64],
            )?;
        }
        if format >= 2 {
            self.set_info(ENRICH_FORMAT_KEY, &ENRICH_FORMAT.to_string())?;
            return Ok(queued);
        }
        let exts: Vec<String> = {
            let mut stmt = self.conn.prepare(
                "SELECT DISTINCT f.ext FROM files f JOIN enrich e ON e.file_id = f.id
                 WHERE e.done = 1 AND (e.stages & ?1) = 0",
            )?;
            let all = stmt
                .query_map([STAGE_THUMB as i64], |r| r.get::<_, String>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            all.into_iter()
                .filter(|e| storecat_extract::covers::has_embedded_art(e))
                .collect()
        };
        for ext in &exts {
            queued += self.conn.execute(
                "UPDATE enrich SET done = ?1 WHERE done = 1 AND (stages & ?2) = 0
                 AND file_id IN (SELECT id FROM files WHERE ext = ?3)",
                params![ENRICH_ART_PENDING as i64, STAGE_THUMB as i64, ext],
            )?;
        }
        self.set_info(ENRICH_FORMAT_KEY, &ENRICH_FORMAT.to_string())?;
        Ok(queued)
    }

    /// Forget enrichment so the runner redoes it (e.g. after FFmpeg was installed).
    pub fn reset_enrichment(&self, keep_hashes: bool) -> Result<()> {
        self.conn.execute_batch(
            "DELETE FROM enrich; DELETE FROM meta; DELETE FROM text; DELETE FROM archive_entries;",
        )?;
        if !keep_hashes {
            self.conn.execute_batch("DELETE FROM hashes;")?;
        }
        Ok(())
    }

    /// Copy enrichment from the previous catalogue of the same volume for files that are
    /// unchanged (same path, name, size and mtime). Thumbnails come along when both
    /// thumbnail databases are given.
    pub fn carry_over(
        &self,
        old_db: &Path,
        old_thumbs: Option<&Path>,
        new_thumbs: Option<&Path>,
    ) -> Result<u64> {
        let old = old_db.to_string_lossy().replace('\'', "''");
        self.conn
            .execute_batch(&format!("ATTACH DATABASE '{old}' AS old;"))?;
        let has = |table: &str| -> bool {
            self.conn
                .query_row(
                    "SELECT COUNT(*) FROM old.sqlite_master WHERE type = 'table' AND name = ?1",
                    [table],
                    |r| r.get::<_, i64>(0),
                )
                .map(|n| n > 0)
                .unwrap_or(false)
        };
        let result = (|| -> Result<u64> {
            self.conn.execute_batch(
                "CREATE TEMP TABLE idmap AS
                 SELECT ofl.id AS old_id, nf.id AS new_id
                 FROM old.files ofl
                 JOIN old.dirs od ON od.id = ofl.dir_id
                 JOIN main.dirs nd ON nd.path = od.path
                 JOIN main.files nf ON nf.dir_id = nd.id AND nf.name = ofl.name
                      AND nf.size = ofl.size AND nf.mtime = ofl.mtime;
                 CREATE INDEX temp.idmap_old ON idmap(old_id);",
            )?;
            let mapped: i64 = self
                .conn
                .query_row("SELECT COUNT(*) FROM temp.idmap", [], |r| r.get(0))?;
            if has("hashes") {
                self.conn.execute_batch(
                    "INSERT OR IGNORE INTO main.hashes (file_id, quick, full, quick_at, full_at)
                     SELECT m.new_id, h.quick, h.full, h.quick_at, h.full_at
                     FROM old.hashes h JOIN temp.idmap m ON m.old_id = h.file_id;",
                )?;
            }
            if has("meta") {
                self.conn.execute_batch(
                    "INSERT OR IGNORE INTO main.meta (file_id, json)
                     SELECT m.new_id, x.json FROM old.meta x JOIN temp.idmap m ON m.old_id = x.file_id;",
                )?;
            }
            if has("text") {
                self.conn.execute_batch(
                    "INSERT OR IGNORE INTO main.text (file_id, content)
                     SELECT m.new_id, t.content FROM old.text t JOIN temp.idmap m ON m.old_id = t.file_id;",
                )?;
            }
            if has("archive_entries") {
                self.conn.execute_batch(
                    "INSERT INTO main.archive_entries (file_id, path, size, mtime, is_dir)
                     SELECT m.new_id, a.path, a.size, a.mtime, a.is_dir
                     FROM old.archive_entries a JOIN temp.idmap m ON m.old_id = a.file_id;",
                )?;
            }
            if has("enrich") {
                self.conn.execute_batch(
                    "INSERT OR IGNORE INTO main.enrich (file_id, done, stages, error, at)
                     SELECT m.new_id, e.done, e.stages, e.error, e.at
                     FROM old.enrich e JOIN temp.idmap m ON m.old_id = e.file_id;",
                )?;
            }
            if has("diskpos") {
                self.conn.execute_batch(
                    "INSERT OR IGNORE INTO main.diskpos (file_id, lcn)
                     SELECT m.new_id, p.lcn FROM old.diskpos p JOIN temp.idmap m ON m.old_id = p.file_id;",
                )?;
            }
            if let (Some(ot), Some(nt)) = (old_thumbs, new_thumbs) {
                if ot.exists() {
                    ThumbDb::create(nt)?; // ensure schema
                    let ot = ot.to_string_lossy().replace('\'', "''");
                    let nt = nt.to_string_lossy().replace('\'', "''");
                    self.conn.execute_batch(&format!(
                        "ATTACH DATABASE '{ot}' AS ot; ATTACH DATABASE '{nt}' AS nt;
                         INSERT OR IGNORE INTO nt.thumbs (file_id, jpeg, w, h)
                         SELECT m.new_id, t.jpeg, t.w, t.h FROM ot.thumbs t JOIN temp.idmap m ON m.old_id = t.file_id;
                         DETACH DATABASE ot; DETACH DATABASE nt;"
                    ))?;
                }
            }
            self.conn.execute_batch("DROP TABLE temp.idmap;")?;
            Ok(mapped as u64)
        })();
        let _ = self.conn.execute_batch("DETACH DATABASE old;");
        result
    }
}

/// `enrich.done`: finished.
pub const ENRICH_DONE: u8 = 1;
/// `enrich.done`: first pass finished, text or video thumbnails still to do.
pub const ENRICH_DETAIL_PENDING: u8 = 2;
/// `enrich.done`: enriched before Storecat read embedded artwork; only the thumbnail is
/// still to do.
pub const ENRICH_ART_PENDING: u8 = 3;
/// What enrichment produces. Catalogues enriched by an older format get the new parts
/// filled in (see `Catalog::upgrade_enrichment`).
pub const ENRICH_FORMAT: u32 = 3;
const ENRICH_FORMAT_KEY: &str = "enrich_format";

// Stage bits recorded in `enrich.stages`.
pub const STAGE_HASH: u32 = 1;
pub const STAGE_META: u32 = 2;
pub const STAGE_THUMB: u32 = 4;
pub const STAGE_TEXT: u32 = 8;
pub const STAGE_ARCHIVE: u32 = 16;
pub const STAGE_SKIPPED_PLACEHOLDER: u32 = 32;

/// `volumes/<id>.thumbs.db`
pub struct ThumbDb {
    conn: Connection,
}

impl ThumbDb {
    pub fn create(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             CREATE TABLE IF NOT EXISTS thumbs (
                 file_id INTEGER PRIMARY KEY,
                 jpeg    BLOB NOT NULL,
                 w       INTEGER NOT NULL,
                 h       INTEGER NOT NULL
             );",
        )?;
        Ok(Self { conn })
    }

    pub fn open_ro(path: &Path) -> Result<Self> {
        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        Ok(Self { conn })
    }

    /// Drop the thumbnails of files that left the catalogue.
    pub fn delete(&self, file_ids: &[i64]) -> Result<()> {
        self.conn.busy_timeout(std::time::Duration::from_secs(30))?;
        let tx = self.conn.unchecked_transaction()?;
        {
            let mut stmt = tx.prepare_cached("DELETE FROM thumbs WHERE file_id = ?1")?;
            for id in file_ids {
                stmt.execute([id])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn put(&self, file_id: i64, jpeg: &[u8], w: u32, h: u32) -> Result<()> {
        self.conn.execute(
            "INSERT INTO thumbs (file_id, jpeg, w, h) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(file_id) DO UPDATE SET jpeg = excluded.jpeg, w = excluded.w, h = excluded.h",
            params![file_id, jpeg, w as i64, h as i64],
        )?;
        Ok(())
    }

    pub fn get(&self, file_id: i64) -> Result<Option<Vec<u8>>> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT jpeg FROM thumbs WHERE file_id = ?1")?;
        Ok(stmt.query_row([file_id], |r| r.get(0)).optional()?)
    }

    pub fn begin(&self) -> Result<()> {
        self.conn.execute_batch("BEGIN")?;
        Ok(())
    }

    pub fn commit(&self) -> Result<()> {
        self.conn.execute_batch("COMMIT")?;
        Ok(())
    }

    /// Throw away the open transaction (the drive went away mid-batch).
    pub fn rollback(&self) -> Result<()> {
        self.conn.execute_batch("ROLLBACK")?;
        Ok(())
    }
}
