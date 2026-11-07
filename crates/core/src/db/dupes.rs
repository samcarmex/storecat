//! `dupes.db`: quick hashes of every catalogued file above a size threshold, across all
//! volumes, so duplicate groups are one indexed GROUP BY away. Kept apart from `master.db`
//! so the always-open database stays tiny; rebuilt per volume after each enrichment run.

use std::path::Path;

use rusqlite::{params, Connection};

use crate::error::Result;
use crate::model::VolumeId;

pub struct DupesDb {
    conn: Connection,
}

#[derive(Debug, Clone)]
pub struct RawGroup {
    pub hash: Vec<u8>,
    pub size: u64,
    pub count: u64,
    pub members: Vec<(VolumeId, i64)>,
}

impl DupesDb {
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             CREATE TABLE IF NOT EXISTS entries (
                 hash      BLOB NOT NULL,
                 size      INTEGER NOT NULL,
                 volume_id INTEGER NOT NULL,
                 file_id   INTEGER NOT NULL,
                 PRIMARY KEY (volume_id, file_id)
             ) WITHOUT ROWID;
             CREATE INDEX IF NOT EXISTS entries_hash ON entries(hash);",
        )?;
        Ok(Self { conn })
    }

    /// Replace all entries of a volume.
    pub fn replace_volume(&self, volume_id: VolumeId, rows: &[(Vec<u8>, u64, i64)]) -> Result<()> {
        self.conn.execute_batch("BEGIN")?;
        let r = (|| -> Result<()> {
            self.conn
                .execute("DELETE FROM entries WHERE volume_id = ?1", [volume_id])?;
            let mut ins = self.conn.prepare_cached(
                "INSERT OR REPLACE INTO entries (hash, size, volume_id, file_id) VALUES (?1, ?2, ?3, ?4)",
            )?;
            for (hash, size, file_id) in rows {
                ins.execute(params![hash, *size as i64, volume_id, file_id])?;
            }
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

    pub fn remove_volume(&self, volume_id: VolumeId) -> Result<()> {
        self.conn
            .execute("DELETE FROM entries WHERE volume_id = ?1", [volume_id])?;
        Ok(())
    }

    /// Groups with more than one member, biggest reclaimable space first.
    pub fn groups(&self, min_size: u64, limit: usize, offset: usize) -> Result<Vec<RawGroup>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT hash, size, COUNT(*) AS c FROM entries WHERE size >= ?1
             GROUP BY hash HAVING c > 1
             ORDER BY (c - 1) * size DESC LIMIT ?2 OFFSET ?3",
        )?;
        let heads: Vec<(Vec<u8>, u64, u64)> = stmt
            .query_map(params![min_size as i64, limit as i64, offset as i64], |r| {
                Ok((
                    r.get(0)?,
                    r.get::<_, i64>(1)? as u64,
                    r.get::<_, i64>(2)? as u64,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let mut out = Vec::with_capacity(heads.len());
        for (hash, size, count) in heads {
            out.push(RawGroup {
                members: self.members(&hash)?,
                hash,
                size,
                count,
            });
        }
        Ok(out)
    }

    pub fn members(&self, hash: &[u8]) -> Result<Vec<(VolumeId, i64)>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT volume_id, file_id FROM entries WHERE hash = ?1 ORDER BY volume_id, file_id",
        )?;
        let rows = stmt.query_map([hash], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub fn summary(&self, min_size: u64) -> Result<(u64, u64, u64)> {
        let (groups, files, waste): (i64, i64, i64) = self.conn.query_row(
            "SELECT COUNT(*), COALESCE(SUM(c), 0), COALESCE(SUM((c - 1) * size), 0) FROM (
                 SELECT size, COUNT(*) AS c FROM entries WHERE size >= ?1 GROUP BY hash HAVING c > 1
             )",
            [min_size as i64],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        Ok((groups as u64, files as u64, waste as u64))
    }
}
