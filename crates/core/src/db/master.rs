//! `master.db`: the small always-open database. Volumes, settings, scan history.
//! Opens in well under 10 ms; nothing per-file lives here.

use std::collections::HashMap;
use std::path::Path;

use rusqlite::{params, Connection, OptionalExtension, Row};

use crate::error::Result;
use crate::model::{
    now_unix, DetectedVolume, Health, MediaType, Role, ScanStats, Volume, VolumeId, VolumePatch,
    VolumeSource,
};

const SCHEMA_VERSION: i64 = 4;

pub struct MasterDb {
    conn: Connection,
}

/// Fields needed to register a new volume.
pub struct NewVolume<'a> {
    pub identity: &'a str,
    pub source: VolumeSource,
    pub label: &'a str,
    pub fs: &'a str,
    pub capacity: u64,
    pub free: u64,
    pub media_type: MediaType,
    pub mount_path: &'a str,
}

impl<'a> NewVolume<'a> {
    pub fn from_detected(d: &'a DetectedVolume) -> Self {
        Self {
            identity: &d.identity,
            source: VolumeSource::Device,
            label: &d.label,
            fs: &d.fs,
            capacity: d.capacity,
            free: d.free,
            media_type: d.media_type,
            mount_path: &d.mount_path,
        }
    }
}

impl MasterDb {
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA foreign_keys = ON;
             PRAGMA temp_store = MEMORY;",
        )?;
        let db = Self { conn };
        db.migrate()?;
        Ok(db)
    }

    fn migrate(&self) -> Result<()> {
        let version: i64 = self
            .conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version < 1 {
            self.conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS volumes (
                    id              INTEGER PRIMARY KEY,
                    identity        TEXT NOT NULL UNIQUE,
                    source          TEXT NOT NULL DEFAULT 'device',
                    label           TEXT NOT NULL DEFAULT '',
                    fs              TEXT NOT NULL DEFAULT '',
                    capacity        INTEGER NOT NULL DEFAULT 0,
                    free            INTEGER NOT NULL DEFAULT 0,
                    media_type      TEXT NOT NULL DEFAULT 'unknown',
                    role            TEXT NOT NULL DEFAULT 'unassigned',
                    notes           TEXT NOT NULL DEFAULT '',
                    location        TEXT NOT NULL DEFAULT '',
                    last_scan_at    INTEGER,
                    last_seen_at    INTEGER,
                    last_mount_path TEXT,
                    file_count      INTEGER NOT NULL DEFAULT 0,
                    dir_count       INTEGER NOT NULL DEFAULT 0,
                    total_bytes     INTEGER NOT NULL DEFAULT 0,
                    scan_seconds    REAL NOT NULL DEFAULT 0,
                    created_at      INTEGER NOT NULL
                );
                CREATE TABLE IF NOT EXISTS scans (
                    id          INTEGER PRIMARY KEY,
                    volume_id   INTEGER NOT NULL REFERENCES volumes(id) ON DELETE CASCADE,
                    started_at  INTEGER NOT NULL,
                    finished_at INTEGER,
                    files       INTEGER NOT NULL DEFAULT 0,
                    dirs        INTEGER NOT NULL DEFAULT 0,
                    bytes       INTEGER NOT NULL DEFAULT 0,
                    errors      INTEGER NOT NULL DEFAULT 0,
                    status      TEXT NOT NULL DEFAULT 'running',
                    message     TEXT
                );
                CREATE INDEX IF NOT EXISTS scans_volume ON scans(volume_id, started_at);
                CREATE TABLE IF NOT EXISTS settings (
                    key   TEXT PRIMARY KEY,
                    value TEXT NOT NULL
                );",
            )?;
        }
        if version < 2 {
            self.conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS jobs (
                    id               INTEGER PRIMARY KEY,
                    kind             TEXT NOT NULL,
                    name             TEXT NOT NULL,
                    status           TEXT NOT NULL DEFAULT 'planned',
                    source_volume_id INTEGER,
                    dest_volume_id   INTEGER,
                    dest_root        TEXT NOT NULL DEFAULT '',
                    conflict         TEXT NOT NULL DEFAULT 'rename',
                    verify           INTEGER NOT NULL DEFAULT 1,
                    files_total      INTEGER NOT NULL DEFAULT 0,
                    bytes_total      INTEGER NOT NULL DEFAULT 0,
                    files_done       INTEGER NOT NULL DEFAULT 0,
                    bytes_done       INTEGER NOT NULL DEFAULT 0,
                    files_failed     INTEGER NOT NULL DEFAULT 0,
                    files_skipped    INTEGER NOT NULL DEFAULT 0,
                    created_at       INTEGER NOT NULL,
                    started_at       INTEGER,
                    finished_at      INTEGER,
                    error            TEXT
                );
                CREATE TABLE IF NOT EXISTS job_items (
                    id      INTEGER PRIMARY KEY,
                    job_id  INTEGER NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,
                    source  TEXT NOT NULL,
                    dest    TEXT NOT NULL DEFAULT '',
                    size    INTEGER NOT NULL DEFAULT 0,
                    mtime   INTEGER NOT NULL DEFAULT 0,
                    status  TEXT NOT NULL DEFAULT 'pending',
                    hash    BLOB,
                    error   TEXT
                );
                CREATE INDEX IF NOT EXISTS job_items_job ON job_items(job_id, status);
                CREATE TABLE IF NOT EXISTS tags (
                    id   INTEGER PRIMARY KEY,
                    name TEXT NOT NULL UNIQUE COLLATE NOCASE
                );
                CREATE TABLE IF NOT EXISTS item_tags (
                    volume_id INTEGER NOT NULL REFERENCES volumes(id) ON DELETE CASCADE,
                    path      TEXT NOT NULL,
                    tag_id    INTEGER NOT NULL REFERENCES tags(id) ON DELETE CASCADE,
                    PRIMARY KEY (volume_id, path, tag_id)
                );
                CREATE INDEX IF NOT EXISTS item_tags_tag ON item_tags(tag_id);
                CREATE TABLE IF NOT EXISTS saved_searches (
                    id         INTEGER PRIMARY KEY,
                    name       TEXT NOT NULL,
                    query      TEXT NOT NULL,
                    created_at INTEGER NOT NULL
                );
                ALTER TABLE volumes ADD COLUMN photo INTEGER NOT NULL DEFAULT 0;",
            )?;
        }
        if version < 3 {
            self.conn.execute_batch(
                "ALTER TABLE volumes ADD COLUMN bus TEXT NOT NULL DEFAULT '';
                 ALTER TABLE volumes ADD COLUMN model TEXT NOT NULL DEFAULT '';
                 ALTER TABLE volumes ADD COLUMN health TEXT NOT NULL DEFAULT 'unknown';
                 ALTER TABLE volumes ADD COLUMN health_at INTEGER;
                 CREATE TABLE IF NOT EXISTS suggestion_state (
                     key   TEXT PRIMARY KEY,
                     state TEXT NOT NULL,
                     until INTEGER
                 );",
            )?;
        }
        if version < 4 {
            self.conn.execute_batch(
                "ALTER TABLE volumes ADD COLUMN fault TEXT;
                 ALTER TABLE volumes ADD COLUMN fault_at INTEGER;",
            )?;
        }
        if version < SCHEMA_VERSION {
            self.conn
                .pragma_update(None, "user_version", SCHEMA_VERSION)?;
        }
        Ok(())
    }

    pub fn insert_volume(&self, v: &NewVolume) -> Result<VolumeId> {
        let now = now_unix();
        self.conn.execute(
            "INSERT INTO volumes (identity, source, label, fs, capacity, free, media_type,
                                  last_seen_at, last_mount_path, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?8)",
            params![
                v.identity,
                v.source.as_str(),
                v.label,
                v.fs,
                v.capacity as i64,
                v.free as i64,
                v.media_type.as_str(),
                now,
                v.mount_path,
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn list_volumes(&self) -> Result<Vec<Volume>> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT * FROM volumes ORDER BY created_at ASC, id ASC")?;
        let rows = stmt.query_map([], row_to_volume)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub fn get_volume(&self, id: VolumeId) -> Result<Option<Volume>> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT * FROM volumes WHERE id = ?1")?;
        Ok(stmt.query_row([id], row_to_volume).optional()?)
    }

    pub fn find_by_identity(&self, identity: &str) -> Result<Option<Volume>> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT * FROM volumes WHERE identity = ?1")?;
        Ok(stmt.query_row([identity], row_to_volume).optional()?)
    }

    /// Record that a volume was seen online, with its current free space, mount and drive
    /// profile. A media type the user never set (still "unknown") takes the detected one.
    pub fn touch_seen(&self, id: VolumeId, d: &DetectedVolume) -> Result<()> {
        let now = now_unix();
        let health_at = (d.health != Health::Unknown).then_some(now);
        self.conn.execute(
            "UPDATE volumes SET last_seen_at = ?2, last_mount_path = ?3, free = ?4,
                                capacity = CASE WHEN ?5 > 0 THEN ?5 ELSE capacity END,
                                bus = CASE WHEN ?6 <> '' THEN ?6 ELSE bus END,
                                model = CASE WHEN ?7 <> '' THEN ?7 ELSE model END,
                                health = CASE WHEN ?8 <> 'unknown' THEN ?8 ELSE health END,
                                health_at = COALESCE(?9, health_at),
                                media_type = CASE WHEN media_type = 'unknown' THEN ?10
                                                  ELSE media_type END
             WHERE id = ?1",
            params![
                id,
                now,
                d.mount_path,
                d.free as i64,
                d.capacity as i64,
                d.bus,
                d.model,
                d.health.as_str(),
                health_at,
                d.media_type.as_str()
            ],
        )?;
        Ok(())
    }

    /// Free space / capacity and media type of a folder volume, as last measured.
    pub fn set_folder_space(
        &self,
        id: VolumeId,
        free: u64,
        capacity: u64,
        media: Option<MediaType>,
        device: Option<&DetectedVolume>,
    ) -> Result<()> {
        let now = now_unix();
        self.conn.execute(
            "UPDATE volumes SET free = ?2, capacity = ?3, last_seen_at = ?4,
                                media_type = CASE WHEN ?5 IS NOT NULL AND media_type = 'unknown'
                                                  THEN ?5 ELSE media_type END
             WHERE id = ?1",
            params![
                id,
                free as i64,
                capacity as i64,
                now,
                media.map(|m| m.as_str())
            ],
        )?;
        // A folder shares the profile and health of the drive it lives on.
        if let Some(d) = device {
            self.conn.execute(
                "UPDATE volumes SET bus = ?2, model = ?3,
                                    health = CASE WHEN ?4 <> 'unknown' THEN ?4 ELSE health END,
                                    health_at = CASE WHEN ?4 <> 'unknown' THEN ?5 ELSE health_at END
                 WHERE id = ?1",
                params![id, d.bus, d.model, d.health.as_str(), now],
            )?;
        }
        Ok(())
    }

    // --- suggestion state (dismissed / snoozed) ---

    pub fn suggestion_states(&self) -> Result<HashMap<String, (String, Option<i64>)>> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT key, state, until FROM suggestion_state")?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, (r.get(1)?, r.get(2)?))))?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    pub fn set_suggestion_state(&self, key: &str, state: &str, until: Option<i64>) -> Result<()> {
        if state.is_empty() {
            self.conn
                .execute("DELETE FROM suggestion_state WHERE key = ?1", [key])?;
        } else {
            self.conn.execute(
                "INSERT INTO suggestion_state (key, state, until) VALUES (?1, ?2, ?3)
                 ON CONFLICT(key) DO UPDATE SET state = excluded.state, until = excluded.until",
                params![key, state, until],
            )?;
        }
        Ok(())
    }

    pub fn update_volume(&self, id: VolumeId, patch: &VolumePatch) -> Result<()> {
        if let Some(label) = &patch.label {
            self.conn.execute(
                "UPDATE volumes SET label = ?2 WHERE id = ?1",
                params![id, label],
            )?;
        }
        if let Some(mt) = patch.media_type {
            self.conn.execute(
                "UPDATE volumes SET media_type = ?2 WHERE id = ?1",
                params![id, mt.as_str()],
            )?;
        }
        if let Some(role) = patch.role {
            self.conn.execute(
                "UPDATE volumes SET role = ?2 WHERE id = ?1",
                params![id, role.as_str()],
            )?;
        }
        if let Some(notes) = &patch.notes {
            self.conn.execute(
                "UPDATE volumes SET notes = ?2 WHERE id = ?1",
                params![id, notes],
            )?;
        }
        if let Some(location) = &patch.location {
            self.conn.execute(
                "UPDATE volumes SET location = ?2 WHERE id = ?1",
                params![id, location],
            )?;
        }
        Ok(())
    }

    pub fn delete_volume(&self, id: VolumeId) -> Result<()> {
        self.conn
            .execute("DELETE FROM volumes WHERE id = ?1", [id])?;
        Ok(())
    }

    pub fn begin_scan(&self, volume_id: VolumeId) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO scans (volume_id, started_at) VALUES (?1, ?2)",
            params![volume_id, now_unix()],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Apply live changes to a volume's totals without a new scan.
    pub fn adjust_counts(
        &self,
        volume_id: VolumeId,
        files: i64,
        dirs: i64,
        bytes: i64,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE volumes SET file_count = MAX(0, file_count + ?2),
                                dir_count = MAX(0, dir_count + ?3),
                                total_bytes = MAX(0, total_bytes + ?4)
             WHERE id = ?1",
            params![volume_id, files, dirs, bytes],
        )?;
        Ok(())
    }

    pub fn finish_scan(&self, scan_id: i64, volume_id: VolumeId, stats: &ScanStats) -> Result<()> {
        let now = now_unix();
        self.conn.execute(
            "UPDATE scans SET finished_at = ?2, files = ?3, dirs = ?4, bytes = ?5, errors = ?6,
                              status = 'done'
             WHERE id = ?1",
            params![
                scan_id,
                now,
                stats.files as i64,
                stats.dirs as i64,
                stats.bytes as i64,
                stats.errors as i64
            ],
        )?;
        self.conn.execute(
            "UPDATE volumes SET last_scan_at = ?2, file_count = ?3, dir_count = ?4,
                                total_bytes = ?5, scan_seconds = ?6
             WHERE id = ?1",
            params![
                volume_id,
                now,
                stats.files as i64,
                stats.dirs as i64,
                stats.bytes as i64,
                stats.elapsed_ms as f64 / 1000.0
            ],
        )?;
        Ok(())
    }

    pub fn fail_scan(&self, scan_id: i64, message: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE scans SET finished_at = ?2, status = 'failed', message = ?3 WHERE id = ?1",
            params![scan_id, now_unix(), message],
        )?;
        Ok(())
    }

    pub fn setting(&self, key: &str) -> Result<Option<String>> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT value FROM settings WHERE key = ?1")?;
        Ok(stmt.query_row([key], |r| r.get(0)).optional()?)
    }

    pub fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }
}

fn row_to_volume(r: &Row) -> rusqlite::Result<Volume> {
    let media: String = r.get("media_type")?;
    let role: String = r.get("role")?;
    let source: String = r.get("source")?;
    Ok(Volume {
        id: r.get("id")?,
        identity: r.get("identity")?,
        source: VolumeSource::parse(&source),
        label: r.get("label")?,
        fs: r.get("fs")?,
        capacity: r.get::<_, i64>("capacity")? as u64,
        free: r.get::<_, i64>("free")? as u64,
        media_type: MediaType::parse(&media),
        role: Role::parse(&role),
        notes: r.get("notes")?,
        location: r.get("location")?,
        last_scan_at: r.get("last_scan_at")?,
        last_seen_at: r.get("last_seen_at")?,
        last_mount_path: r.get("last_mount_path")?,
        file_count: r.get::<_, i64>("file_count")? as u64,
        dir_count: r.get::<_, i64>("dir_count")? as u64,
        total_bytes: r.get::<_, i64>("total_bytes")? as u64,
        scan_seconds: r.get("scan_seconds")?,
        created_at: r.get("created_at")?,
        photo: r.get::<_, i64>("photo").unwrap_or(0) != 0,
        bus: r.get("bus").unwrap_or_default(),
        model: r.get("model").unwrap_or_default(),
        health: Health::parse(&r.get::<_, String>("health").unwrap_or_default()),
        health_at: r.get("health_at").unwrap_or(None),
        fault: r.get("fault").unwrap_or(None),
        fault_at: r.get("fault_at").unwrap_or(None),
        online: false,
        mount_path: None,
    })
}

// ----- jobs, tags, saved searches -----------------------------------------------------------

use crate::model::{
    ConflictPolicy, ItemStatus, Job, JobItem, JobKind, JobStatus, SavedSearch, Tag,
};

impl MasterDb {
    /// Record (or with `None`, clear) read trouble Storecat saw on a drive.
    pub fn set_fault(&self, id: VolumeId, fault: Option<&str>) -> Result<()> {
        self.conn.execute(
            "UPDATE volumes SET fault = ?2, fault_at = CASE WHEN ?2 IS NULL THEN NULL ELSE ?3 END
             WHERE id = ?1",
            params![id, fault, now_unix()],
        )?;
        Ok(())
    }

    pub fn set_photo_flag(&self, id: VolumeId, has: bool) -> Result<()> {
        self.conn.execute(
            "UPDATE volumes SET photo = ?2 WHERE id = ?1",
            params![id, has as i64],
        )?;
        Ok(())
    }

    // --- jobs ---

    #[allow(clippy::too_many_arguments)]
    pub fn insert_job(
        &self,
        kind: JobKind,
        name: &str,
        source_volume_id: Option<VolumeId>,
        dest_volume_id: Option<VolumeId>,
        dest_root: &str,
        conflict: ConflictPolicy,
        verify: bool,
    ) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO jobs (kind, name, status, source_volume_id, dest_volume_id, dest_root,
                               conflict, verify, created_at)
             VALUES (?1, ?2, 'planned', ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                kind.as_str(),
                name,
                source_volume_id,
                dest_volume_id,
                dest_root,
                conflict.as_str(),
                verify as i64,
                now_unix()
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn insert_job_items(
        &self,
        job_id: i64,
        items: &[(String, String, u64, i64)],
    ) -> Result<()> {
        self.conn.execute_batch("BEGIN")?;
        let r = (|| -> Result<()> {
            let mut ins = self.conn.prepare_cached(
                "INSERT INTO job_items (job_id, source, dest, size, mtime) VALUES (?1, ?2, ?3, ?4, ?5)",
            )?;
            let mut files = 0i64;
            let mut bytes = 0i64;
            for (src, dst, size, mtime) in items {
                ins.execute(params![job_id, src, dst, *size as i64, mtime])?;
                files += 1;
                bytes += *size as i64;
            }
            self.conn.execute(
                "UPDATE jobs SET files_total = files_total + ?2, bytes_total = bytes_total + ?3 WHERE id = ?1",
                params![job_id, files, bytes],
            )?;
            Ok(())
        })();
        match r {
            Ok(()) => self.conn.execute_batch("COMMIT")?,
            Err(e) => {
                let _ = self.conn.execute_batch("ROLLBACK");
                return Err(e);
            }
        }
        Ok(())
    }

    pub fn jobs(&self) -> Result<Vec<Job>> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT * FROM jobs ORDER BY created_at DESC, id DESC")?;
        let rows = stmt.query_map([], row_to_job)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub fn job(&self, id: i64) -> Result<Option<Job>> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT * FROM jobs WHERE id = ?1")?;
        Ok(stmt.query_row([id], row_to_job).optional()?)
    }

    pub fn job_items(
        &self,
        job_id: i64,
        status: Option<ItemStatus>,
        limit: usize,
    ) -> Result<Vec<JobItem>> {
        let sql = match status {
            Some(_) => {
                "SELECT id, job_id, source, dest, size, mtime, status, error FROM job_items
                        WHERE job_id = ?1 AND status = ?2 ORDER BY id LIMIT ?3"
            }
            None => {
                "SELECT id, job_id, source, dest, size, mtime, status, error FROM job_items
                     WHERE job_id = ?1 AND ?2 = '' ORDER BY id LIMIT ?3"
            }
        };
        let mut stmt = self.conn.prepare_cached(sql)?;
        let status_s = status.map(|s| s.as_str()).unwrap_or("");
        let rows = stmt.query_map(params![job_id, status_s, limit as i64], |r| {
            let st: String = r.get(6)?;
            Ok(JobItem {
                id: r.get(0)?,
                job_id: r.get(1)?,
                source: r.get(2)?,
                dest: r.get(3)?,
                size: r.get::<_, i64>(4)? as u64,
                mtime: r.get(5)?,
                status: ItemStatus::parse(&st),
                error: r.get(7)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub fn set_item_status(
        &self,
        item_id: i64,
        status: ItemStatus,
        hash: Option<&[u8]>,
        error: Option<&str>,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE job_items SET status = ?2, hash = COALESCE(?3, hash), error = ?4 WHERE id = ?1",
            params![item_id, status.as_str(), hash, error],
        )?;
        Ok(())
    }

    pub fn item_hash(&self, item_id: i64) -> Result<Option<Vec<u8>>> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT hash FROM job_items WHERE id = ?1")?;
        Ok(stmt
            .query_row([item_id], |r| r.get(0))
            .optional()?
            .flatten())
    }

    pub fn set_item_dest(&self, item_id: i64, dest: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE job_items SET dest = ?2 WHERE id = ?1",
            params![item_id, dest],
        )?;
        Ok(())
    }

    pub fn set_job_status(&self, id: i64, status: JobStatus, error: Option<&str>) -> Result<()> {
        let now = now_unix();
        match status {
            JobStatus::Running => self.conn.execute(
                "UPDATE jobs SET status = ?2, started_at = COALESCE(started_at, ?3), error = NULL WHERE id = ?1",
                params![id, status.as_str(), now],
            )?,
            JobStatus::Done | JobStatus::Failed | JobStatus::Cancelled => self.conn.execute(
                "UPDATE jobs SET status = ?2, finished_at = ?3, error = ?4 WHERE id = ?1",
                params![id, status.as_str(), now, error],
            )?,
            _ => self.conn.execute(
                "UPDATE jobs SET status = ?2, error = ?3 WHERE id = ?1",
                params![id, status.as_str(), error],
            )?,
        };
        Ok(())
    }

    /// Recount progress columns from the items (one indexed aggregate).
    pub fn refresh_job_counts(&self, id: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE jobs SET
                files_done = (SELECT COUNT(*) FROM job_items WHERE job_id = ?1 AND status IN ('done', 'verified', 'copied')),
                bytes_done = (SELECT COALESCE(SUM(size), 0) FROM job_items WHERE job_id = ?1 AND status IN ('done', 'verified', 'copied')),
                files_failed = (SELECT COUNT(*) FROM job_items WHERE job_id = ?1 AND status = 'failed'),
                files_skipped = (SELECT COUNT(*) FROM job_items WHERE job_id = ?1 AND status = 'skipped')
             WHERE id = ?1",
            [id],
        )?;
        Ok(())
    }

    pub fn delete_job(&self, id: i64) -> Result<()> {
        self.conn.execute("DELETE FROM jobs WHERE id = ?1", [id])?;
        Ok(())
    }

    // --- tags ---

    pub fn tags(&self) -> Result<Vec<Tag>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT t.id, t.name, COUNT(it.tag_id) FROM tags t
             LEFT JOIN item_tags it ON it.tag_id = t.id
             GROUP BY t.id ORDER BY t.name COLLATE NOCASE",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(Tag {
                id: r.get(0)?,
                name: r.get(1)?,
                count: r.get::<_, i64>(2)? as u64,
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub fn tag_item(&self, volume_id: VolumeId, path: &str, tag: &str) -> Result<()> {
        let name = tag.trim();
        self.conn
            .execute("INSERT OR IGNORE INTO tags (name) VALUES (?1)", [name])?;
        let tag_id: i64 = self.conn.query_row(
            "SELECT id FROM tags WHERE name = ?1 COLLATE NOCASE",
            [name],
            |r| r.get(0),
        )?;
        self.conn.execute(
            "INSERT OR IGNORE INTO item_tags (volume_id, path, tag_id) VALUES (?1, ?2, ?3)",
            params![volume_id, path, tag_id],
        )?;
        Ok(())
    }

    pub fn untag_item(&self, volume_id: VolumeId, path: &str, tag: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM item_tags WHERE volume_id = ?1 AND path = ?2
             AND tag_id = (SELECT id FROM tags WHERE name = ?3 COLLATE NOCASE)",
            params![volume_id, path, tag.trim()],
        )?;
        self.conn.execute(
            "DELETE FROM tags WHERE id NOT IN (SELECT DISTINCT tag_id FROM item_tags)",
            [],
        )?;
        Ok(())
    }

    pub fn item_tags(&self, volume_id: VolumeId, path: &str) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT t.name FROM item_tags it JOIN tags t ON t.id = it.tag_id
             WHERE it.volume_id = ?1 AND it.path = ?2 ORDER BY t.name COLLATE NOCASE",
        )?;
        let rows = stmt.query_map(params![volume_id, path], |r| r.get::<_, String>(0))?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Every tagged path of a volume: `path -> tags`.
    pub fn tag_map(
        &self,
        volume_id: VolumeId,
    ) -> Result<std::collections::HashMap<String, Vec<String>>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT it.path, t.name FROM item_tags it JOIN tags t ON t.id = it.tag_id
             WHERE it.volume_id = ?1 ORDER BY it.path, t.name COLLATE NOCASE",
        )?;
        let mut map: std::collections::HashMap<String, Vec<String>> =
            std::collections::HashMap::new();
        let rows = stmt.query_map([volume_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?;
        for row in rows {
            let (path, tag) = row?;
            map.entry(path).or_default().push(tag);
        }
        Ok(map)
    }

    // --- saved searches ---

    pub fn saved_searches(&self) -> Result<Vec<SavedSearch>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT id, name, query, created_at FROM saved_searches ORDER BY name COLLATE NOCASE",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(SavedSearch {
                id: r.get(0)?,
                name: r.get(1)?,
                query: r.get(2)?,
                created_at: r.get(3)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub fn save_search(&self, name: &str, query: &str) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO saved_searches (name, query, created_at) VALUES (?1, ?2, ?3)",
            params![name.trim(), query.trim(), now_unix()],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn delete_saved_search(&self, id: i64) -> Result<()> {
        self.conn
            .execute("DELETE FROM saved_searches WHERE id = ?1", [id])?;
        Ok(())
    }
}

fn row_to_job(r: &Row) -> rusqlite::Result<Job> {
    let kind: String = r.get("kind")?;
    let status: String = r.get("status")?;
    let conflict: String = r.get("conflict")?;
    Ok(Job {
        id: r.get("id")?,
        kind: JobKind::parse(&kind),
        name: r.get("name")?,
        status: JobStatus::parse(&status),
        source_volume_id: r.get("source_volume_id")?,
        dest_volume_id: r.get("dest_volume_id")?,
        dest_root: r.get("dest_root")?,
        conflict: ConflictPolicy::parse(&conflict),
        verify: r.get::<_, i64>("verify")? != 0,
        files_total: r.get::<_, i64>("files_total")? as u64,
        bytes_total: r.get::<_, i64>("bytes_total")? as u64,
        files_done: r.get::<_, i64>("files_done")? as u64,
        bytes_done: r.get::<_, i64>("bytes_done")? as u64,
        files_failed: r.get::<_, i64>("files_failed")? as u64,
        files_skipped: r.get::<_, i64>("files_skipped")? as u64,
        created_at: r.get("created_at")?,
        started_at: r.get("started_at")?,
        finished_at: r.get("finished_at")?,
        error: r.get("error")?,
    })
}
