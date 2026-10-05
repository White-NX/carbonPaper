//! Versioned blind postings. Large, actively updated postings are split into
//! bounded ID ranges; small and cold postings retain their existing encoding.

use roaring::RoaringBitmap;
use rusqlite::{params, Connection, OptionalExtension};
use std::ops::Bound::{Excluded, Unbounded};
use std::time::{Duration, Instant};

const FORMAT_KEY: &str = "blind_bitmap_storage_format";
const FORMAT_VERSION: &str = "2";
const LEGACY_GUARD: &str = "carbonpaper_blind_index_v2_required";
pub(super) const PROMOTION_BYTES: usize = 64 * 1024;
pub(super) const SHARD_BITS: u32 = 20;
const SHARD_MASK: u32 = (1 << SHARD_BITS) - 1;
const SQL_CHUNK: usize = 500;
const SIZE_EXPR: &str = "payload_bytes + CASE WHEN run_containers = 0
    THEN 8 + 8 * containers
    ELSE 4 + (containers + 7) / 8 + 4 * containers
         + CASE WHEN containers >= 4 THEN 4 * containers ELSE 0 END END";

#[derive(Default)]
pub(super) struct MutationStats {
    pub read: Duration,
    pub merge: Duration,
    pub serialize: Duration,
    pub write: Duration,
    pub existing_tokens: usize,
    pub fragments_written: usize,
    pub bytes_read: usize,
    pub bytes_written: usize,
}

pub(super) struct Posting {
    pub bitmap: RoaringBitmap,
    pub bytes: usize,
    pub fragments: usize,
}

pub(super) struct Probe {
    pub hash: String,
    pub bytes: usize,
    pub cardinality: Option<u64>,
    pub chunked: bool,
}

pub(super) struct ProbeBatch {
    pub postings: Vec<Probe>,
    pub statements: usize,
}

#[derive(Clone, Copy, Default)]
struct Metrics {
    payload: i64,
    containers: i64,
    runs: i64,
    cardinality: i64,
}

fn timed<T>(duration: &mut Duration, operation: impl FnOnce() -> T) -> T {
    let started = Instant::now();
    let result = operation();
    *duration += started.elapsed();
    result
}

fn error(context: &str, e: impl std::fmt::Display) -> String {
    format!("Blind index {context}: {e}")
}

fn object(conn: &Connection, name: &str) -> Result<Option<(String, String)>, String> {
    conn.query_row(
        "SELECT type, COALESCE(sql, '') FROM sqlite_master WHERE name = ?1",
        [name],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
    .optional()
    .map_err(|e| error("schema lookup", e))
}

/// Validate or atomically upgrade the storage layout. No posting is rewritten
/// here: promotion happens within the transaction that next modifies a token.
pub(super) fn check_supported_format(conn: &Connection) -> Result<(), String> {
    if object(conn, "app_metadata")?.is_some() {
        let version: Option<String> = conn
            .query_row(
                "SELECT value FROM app_metadata WHERE key=?1",
                [FORMAT_KEY],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| error("format compatibility", e))?;
        if let Some(version) = version {
            if version != FORMAT_VERSION {
                return Err(format!("Unsupported blind index storage format: {version}"));
            }
        }
    }
    Ok(())
}

pub(super) fn ensure_schema(conn: &Connection) -> Result<(), String> {
    check_supported_format(conn)?;
    let version: Option<String> = conn
        .query_row(
            "SELECT value FROM app_metadata WHERE key = ?1",
            [FORMAT_KEY],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| error("format version", e))?;
    let legacy = object(conn, "blind_bitmap_index")?;
    if let Some(version) = version {
        if version != FORMAT_VERSION {
            return Err(format!("Unsupported blind index storage format: {version}"));
        }
        if !matches!(&legacy, Some((kind, sql)) if kind == "view" && sql.contains(LEGACY_GUARD)) {
            return Err("Blind index format marker does not match its compatibility guard".into());
        }
        for table in [
            "blind_bitmap_inline",
            "blind_bitmap_directory",
            "blind_bitmap_chunks",
        ] {
            if !matches!(object(conn, table)?, Some((kind, _)) if kind == "table") {
                return Err(format!("Blind index format is missing table {table}"));
            }
        }
        return Ok(());
    }
    if !matches!(legacy, Some((kind, _)) if kind == "table") {
        return Err("Unrecognized legacy blind index layout".into());
    }
    for table in [
        "blind_bitmap_inline",
        "blind_bitmap_directory",
        "blind_bitmap_chunks",
    ] {
        if object(conn, table)?.is_some() {
            return Err("Incomplete or unrecognized blind index format migration".into());
        }
    }
    // A savepoint also works when schema initialization is already inside a
    // caller-owned transaction. DDL, the guard, and the version publish together.
    conn.execute_batch("SAVEPOINT carbonpaper_blind_index_format")
        .map_err(|e| error("begin format upgrade", e))?;
    let result = (|| {
        conn.execute_batch(
            "ALTER TABLE blind_bitmap_index RENAME TO blind_bitmap_inline;
             CREATE TABLE blind_bitmap_directory (
                token_hash TEXT PRIMARY KEY,
                payload_bytes INTEGER NOT NULL CHECK(payload_bytes >= 0),
                containers INTEGER NOT NULL CHECK(containers > 0),
                run_containers INTEGER NOT NULL CHECK(run_containers >= 0 AND run_containers <= containers),
                cardinality INTEGER NOT NULL CHECK(cardinality > 0)
             );
             CREATE TABLE blind_bitmap_chunks (
                token_hash TEXT NOT NULL,
                shard_id INTEGER NOT NULL CHECK(shard_id >= 0 AND shard_id <= 4095),
                postings_blob BLOB NOT NULL,
                PRIMARY KEY(token_hash, shard_id),
                FOREIGN KEY(token_hash) REFERENCES blind_bitmap_directory(token_hash) ON DELETE CASCADE
             );
             CREATE TRIGGER blind_inline_excludes_chunks BEFORE INSERT ON blind_bitmap_inline
             WHEN EXISTS(SELECT 1 FROM blind_bitmap_directory WHERE token_hash=NEW.token_hash)
             BEGIN SELECT RAISE(ABORT, 'blind posting already uses chunks'); END;
             CREATE TRIGGER blind_chunks_exclude_inline BEFORE INSERT ON blind_bitmap_directory
             WHEN EXISTS(SELECT 1 FROM blind_bitmap_inline WHERE token_hash=NEW.token_hash)
             BEGIN SELECT RAISE(ABORT, 'blind posting still uses inline storage'); END;
             CREATE VIEW blind_bitmap_index AS
                SELECT token_hash, carbonpaper_blind_index_v2_required() AS postings_blob
                FROM blind_bitmap_inline
                UNION ALL
                SELECT token_hash, carbonpaper_blind_index_v2_required() AS postings_blob
                FROM blind_bitmap_directory;",
        )
        .map_err(|e| error("upgrade layout", e))?;
        // The intentionally unavailable function makes old readers and writers
        // fail at statement preparation instead of silently ignoring chunks.
        conn.execute(
            "INSERT INTO app_metadata(key, value) VALUES (?1, ?2)",
            params![FORMAT_KEY, FORMAT_VERSION],
        )
        .map_err(|e| error("publish format", e))?;
        conn.execute_batch("RELEASE carbonpaper_blind_index_format")
            .map_err(|e| error("commit format upgrade", e))
    })();
    if result.is_err() {
        if let Err(e) = conn.execute_batch(
            "ROLLBACK TO carbonpaper_blind_index_format; RELEASE carbonpaper_blind_index_format",
        ) {
            return Err(format!("{}; rollback failed: {e}", result.unwrap_err()));
        }
    }
    result
}

pub(super) struct BlindIndex<'a> {
    conn: &'a Connection,
    v2: bool,
}

impl<'a> BlindIndex<'a> {
    /// Application connections have already passed ensure_schema. The legacy
    /// branch also supports old snapshots and focused storage fixtures.
    pub fn open(conn: &'a Connection) -> Result<Self, String> {
        match object(conn, "blind_bitmap_index")? {
            Some((kind, _)) if kind == "table" => Ok(Self { conn, v2: false }),
            Some((kind, sql)) if kind == "view" && sql.contains(LEGACY_GUARD) => {
                Ok(Self { conn, v2: true })
            }
            _ => Err("Unsupported blind index layout".into()),
        }
    }

    fn inline_table(&self) -> &'static str {
        if self.v2 {
            "blind_bitmap_inline"
        } else {
            "blind_bitmap_index"
        }
    }

    fn require_transaction(&self) -> Result<(), String> {
        if self.conn.is_autocommit() {
            Err("Blind index mutation requires a caller-owned transaction".into())
        } else {
            Ok(())
        }
    }

    pub fn probe(&self, hashes: &[String]) -> Result<ProbeBatch, String> {
        let mut result = ProbeBatch {
            postings: Vec::new(),
            statements: 0,
        };
        for batch in hashes.chunks(SQL_CHUNK) {
            let placeholders = (1..=batch.len())
                .map(|n| format!("?{n}"))
                .collect::<Vec<_>>()
                .join(",");
            let mut sql = format!("SELECT token_hash, length(postings_blob), NULL, 0 FROM {} WHERE token_hash IN ({placeholders})", self.inline_table());
            if self.v2 {
                sql.push_str(&format!(" UNION ALL SELECT token_hash, {SIZE_EXPR}, cardinality, 1 FROM blind_bitmap_directory WHERE token_hash IN ({placeholders})"));
            }
            let mut statement = self
                .conn
                .prepare(&sql)
                .map_err(|e| error("prepare probe", e))?;
            let rows = statement
                .query_map(rusqlite::params_from_iter(batch.iter()), |row| {
                    let cardinality: Option<i64> = row.get(2)?;
                    if let Some(value) = cardinality {
                        if value < 0 {
                            return Err(rusqlite::Error::IntegralValueOutOfRange(2, value));
                        }
                    }
                    Ok(Probe {
                        hash: row.get(0)?,
                        bytes: row.get::<_, i64>(1)?.max(0) as usize,
                        cardinality: cardinality.map(|n| n as u64),
                        chunked: row.get(3)?,
                    })
                })
                .map_err(|e| error("probe", e))?;
            for row in rows {
                let posting = row.map_err(|e| error("decode probe", e))?;
                if posting.chunked && !posting.cardinality.is_some_and(|count| count > 0) {
                    return Err("Invalid blind index directory cardinality".into());
                }
                result.postings.push(posting);
            }
            result.statements += 1;
        }
        Ok(result)
    }

    /// IDF needs only cardinalities. Fetch inline blobs in batches, retaining
    /// the legacy scorer's bounded statement count; chunked postings need only
    /// directory metadata, so their payload never leaves SQLite here.
    pub fn cardinalities(&self, hashes: &[String]) -> Result<Vec<(String, u64)>, String> {
        let mut result = Vec::new();
        for batch in hashes.chunks(SQL_CHUNK) {
            let placeholders = (1..=batch.len())
                .map(|n| format!("?{n}"))
                .collect::<Vec<_>>()
                .join(",");
            let mut sql = format!("SELECT token_hash, postings_blob, NULL FROM {} WHERE token_hash IN ({placeholders})", self.inline_table());
            if self.v2 {
                sql.push_str(&format!(" UNION ALL SELECT token_hash, NULL, cardinality FROM blind_bitmap_directory WHERE token_hash IN ({placeholders})"));
            }
            let mut statement = self
                .conn
                .prepare(&sql)
                .map_err(|e| error("prepare cardinalities", e))?;
            let rows = statement
                .query_map(rusqlite::params_from_iter(batch.iter()), |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, Option<Vec<u8>>>(1)?,
                        r.get::<_, Option<i64>>(2)?,
                    ))
                })
                .map_err(|e| error("query cardinalities", e))?;
            for row in rows {
                let (hash, bytes, cardinality) =
                    row.map_err(|e| error("decode cardinality row", e))?;
                let count = match (bytes, cardinality) {
                    (Some(bytes), None) => decode(&bytes)?.len(),
                    (None, Some(count)) if count > 0 => count as u64,
                    _ => return Err("Invalid blind index cardinality representation".into()),
                };
                result.push((hash, count));
            }
        }
        Ok(result)
    }

    fn flat_blob(&self, hash: &str, stats: &mut MutationStats) -> Result<Option<Vec<u8>>, String> {
        let sql = if self.v2 {
            "SELECT postings_blob FROM blind_bitmap_inline WHERE token_hash=?1"
        } else {
            "SELECT postings_blob FROM blind_bitmap_index WHERE token_hash=?1"
        };
        let blob: Option<Vec<u8>> = timed(&mut stats.read, || {
            self.conn
                .prepare_cached(sql)?
                .query_row([hash], |r| r.get(0))
                .optional()
        })
        .map_err(|e| error("read inline posting", e))?;
        stats.bytes_read += blob.as_ref().map_or(0, Vec::len);
        Ok(blob)
    }

    fn directory(&self, hash: &str, stats: &mut MutationStats) -> Result<Option<Metrics>, String> {
        if !self.v2 {
            return Ok(None);
        }
        timed(&mut stats.read, || {
            self.conn.prepare_cached("SELECT payload_bytes, containers, run_containers, cardinality FROM blind_bitmap_directory WHERE token_hash=?1")?
                .query_row([hash], |r| Ok(Metrics { payload: r.get(0)?, containers: r.get(1)?, runs: r.get(2)?, cardinality: r.get(3)? })).optional()
        }).map_err(|e| error("read directory", e))
    }

    fn chunks(&self, hash: &str) -> Result<Option<Posting>, String> {
        if !self.v2 {
            return Ok(None);
        }
        let mut statement = self.conn.prepare_cached("SELECT shard_id, postings_blob FROM blind_bitmap_chunks WHERE token_hash=?1 ORDER BY shard_id")
            .map_err(|e| error("prepare chunks", e))?;
        let mut rows = statement
            .query([hash])
            .map_err(|e| error("read chunks", e))?;
        let mut posting = Posting {
            bitmap: RoaringBitmap::new(),
            bytes: 0,
            fragments: 0,
        };
        while let Some(row) = rows.next().map_err(|e| error("read chunk row", e))? {
            let shard: u32 = row.get(0).map_err(|e| error("decode shard id", e))?;
            let bytes: Vec<u8> = row.get(1).map_err(|e| error("decode chunk blob", e))?;
            let bitmap = decode_chunk(&bytes, shard)?;
            posting.bitmap |= bitmap;
            posting.bytes += bytes.len();
            posting.fragments += 1;
        }
        Ok((posting.fragments > 0).then_some(posting))
    }

    pub fn read(&self, hash: &str, chunked_hint: bool) -> Result<Option<Posting>, String> {
        // Hints avoid an extra lookup for the common path. A token may have
        // been promoted or deleted/recreated after probe. An absent hinted
        // representation is resolved in one SQLite statement snapshot below.
        if chunked_hint {
            if let Some(posting) = self.chunks(hash)? {
                return Ok(Some(posting));
            }
        }
        if let Some(bytes) = self.flat_blob(hash, &mut MutationStats::default())? {
            return Ok(Some(Posting {
                bitmap: decode(&bytes)?,
                bytes: bytes.len(),
                fragments: 1,
            }));
        }
        if !chunked_hint {
            if let Some(posting) = self.chunks(hash)? {
                return Ok(Some(posting));
            }
        }
        if !self.v2 {
            return Ok(None);
        }
        let mut statement = self
            .conn
            .prepare_cached(
                "SELECT 0, NULL, postings_blob FROM blind_bitmap_inline WHERE token_hash=?1
             UNION ALL SELECT 1, c.shard_id, c.postings_blob FROM blind_bitmap_directory d
             LEFT JOIN blind_bitmap_chunks c ON c.token_hash=d.token_hash WHERE d.token_hash=?1",
            )
            .map_err(|e| error("prepare posting fallback", e))?;
        let mut rows = statement
            .query([hash])
            .map_err(|e| error("read posting fallback", e))?;
        let mut posting = Posting {
            bitmap: RoaringBitmap::new(),
            bytes: 0,
            fragments: 0,
        };
        let mut representation = None;
        while let Some(row) = rows.next().map_err(|e| error("read fallback row", e))? {
            let chunked: bool = row.get(0).map_err(|e| error("decode representation", e))?;
            if representation.is_some_and(|previous| previous != chunked) {
                return Err("Conflicting blind posting representations".into());
            }
            representation = Some(chunked);
            let bytes: Option<Vec<u8>> =
                row.get(2).map_err(|e| error("decode fallback blob", e))?;
            let bytes = bytes.ok_or("Blind index directory has no posting fragments")?;
            let bitmap = if chunked {
                decode_chunk(
                    &bytes,
                    row.get(1).map_err(|e| error("decode fallback shard", e))?,
                )?
            } else {
                decode(&bytes)?
            };
            posting.bitmap |= bitmap;
            posting.bytes += bytes.len();
            posting.fragments += 1;
        }
        Ok((posting.fragments > 0).then_some(posting))
    }

    fn write_flat(
        &self,
        hash: &str,
        bitmap: &RoaringBitmap,
        stats: &mut MutationStats,
    ) -> Result<(), String> {
        let bytes = encode(bitmap, stats)?;
        let sql = if self.v2 {
            "INSERT INTO blind_bitmap_inline(token_hash,postings_blob) VALUES(?1,?2) ON CONFLICT(token_hash) DO UPDATE SET postings_blob=excluded.postings_blob"
        } else {
            "INSERT INTO blind_bitmap_index(token_hash,postings_blob) VALUES(?1,?2) ON CONFLICT(token_hash) DO UPDATE SET postings_blob=excluded.postings_blob"
        };
        timed(&mut stats.write, || {
            self.conn
                .prepare_cached(sql)?
                .execute(params![hash, &bytes])
        })
        .map_err(|e| error("write inline posting", e))?;
        stats.bytes_written += bytes.len();
        stats.fragments_written += 1;
        Ok(())
    }

    fn delete_flat(&self, hash: &str, stats: &mut MutationStats) -> Result<(), String> {
        let sql = if self.v2 {
            "DELETE FROM blind_bitmap_inline WHERE token_hash=?1"
        } else {
            "DELETE FROM blind_bitmap_index WHERE token_hash=?1"
        };
        timed(&mut stats.write, || {
            self.conn.prepare_cached(sql)?.execute([hash])
        })
        .map_err(|e| error("delete inline posting", e))?;
        Ok(())
    }

    fn write_directory(
        &self,
        hash: &str,
        metrics: Metrics,
        stats: &mut MutationStats,
    ) -> Result<(), String> {
        if metrics.cardinality <= 0
            || metrics.containers <= 0
            || metrics.runs < 0
            || metrics.runs > metrics.containers
            || metrics.payload < 0
        {
            return Err("Invalid blind index directory metrics".into());
        }
        timed(&mut stats.write, || self.conn.prepare_cached(
            "INSERT INTO blind_bitmap_directory(token_hash,payload_bytes,containers,run_containers,cardinality) VALUES(?1,?2,?3,?4,?5)
             ON CONFLICT(token_hash) DO UPDATE SET payload_bytes=excluded.payload_bytes,containers=excluded.containers,run_containers=excluded.run_containers,cardinality=excluded.cardinality"
        )?.execute(params![hash,metrics.payload,metrics.containers,metrics.runs,metrics.cardinality]))
            .map_err(|e| error("write directory", e))?;
        Ok(())
    }

    fn write_chunk(
        &self,
        hash: &str,
        shard: u32,
        bytes: &[u8],
        stats: &mut MutationStats,
    ) -> Result<(), String> {
        timed(&mut stats.write, || self.conn.prepare_cached(
            "INSERT INTO blind_bitmap_chunks(token_hash,shard_id,postings_blob) VALUES(?1,?2,?3) ON CONFLICT(token_hash,shard_id) DO UPDATE SET postings_blob=excluded.postings_blob"
        )?.execute(params![hash,shard,bytes])).map_err(|e| error("write chunk", e))?;
        stats.bytes_written += bytes.len();
        stats.fragments_written += 1;
        Ok(())
    }

    fn clear_chunks(&self, hash: &str, stats: &mut MutationStats) -> Result<(), String> {
        timed(&mut stats.write, || -> rusqlite::Result<()> {
            self.conn
                .prepare_cached("DELETE FROM blind_bitmap_chunks WHERE token_hash=?1")?
                .execute([hash])?;
            self.conn
                .prepare_cached("DELETE FROM blind_bitmap_directory WHERE token_hash=?1")?
                .execute([hash])?;
            Ok(())
        })
        .map_err(|e| error("clear chunks", e))
    }

    fn partitioned_replace(
        &self,
        hash: &str,
        bitmap: &RoaringBitmap,
        stats: &mut MutationStats,
    ) -> Result<(), String> {
        let mut pieces = Vec::new();
        let mut metrics = Metrics::default();
        for (shard, piece) in split(bitmap) {
            let bytes = encode(&piece, stats)?;
            metrics = metrics.adjust(metrics_for(&bytes, &piece)?, 1)?;
            pieces.push((shard, bytes));
        }
        self.clear_chunks(hash, stats)?;
        self.delete_flat(hash, stats)?;
        self.write_directory(hash, metrics, stats)?;
        for (shard, bytes) in pieces {
            self.write_chunk(hash, shard, &bytes, stats)?;
        }
        Ok(())
    }

    pub fn replace(
        &self,
        hash: &str,
        bitmap: &RoaringBitmap,
        stats: &mut MutationStats,
    ) -> Result<(), String> {
        self.require_transaction()?;
        if bitmap.is_empty() {
            self.delete_flat(hash, stats)?;
            if self.v2 {
                self.clear_chunks(hash, stats)?;
            }
        } else if self.v2 && (self.directory(hash, stats)?.is_some() || should_promote(bitmap)) {
            self.partitioned_replace(hash, bitmap, stats)?;
        } else {
            self.write_flat(hash, bitmap, stats)?;
        }
        Ok(())
    }

    pub fn add(
        &self,
        hash: &str,
        ids: &RoaringBitmap,
        stats: &mut MutationStats,
    ) -> Result<(), String> {
        self.modify(hash, ids, false, stats)
    }

    pub fn remove(
        &self,
        hash: &str,
        ids: &RoaringBitmap,
        stats: &mut MutationStats,
    ) -> Result<(), String> {
        self.modify(hash, ids, true, stats)
    }

    fn modify(
        &self,
        hash: &str,
        ids: &RoaringBitmap,
        remove: bool,
        stats: &mut MutationStats,
    ) -> Result<(), String> {
        self.require_transaction()?;
        if ids.is_empty() {
            return Ok(());
        }
        let Some(mut metrics) = self.directory(hash, stats)? else {
            let old = self.flat_blob(hash, stats)?;
            if old.is_some() {
                stats.existing_tokens += 1;
            }
            if remove && old.is_none() {
                return Ok(());
            }
            let mut bitmap = timed(&mut stats.merge, || {
                old.as_ref()
                    .map_or_else(|| Ok(RoaringBitmap::new()), |b| decode(b))
            })?;
            let before = bitmap.len();
            timed(&mut stats.merge, || {
                if remove {
                    bitmap -= ids;
                } else {
                    bitmap |= ids;
                }
            });
            if bitmap.len() == before {
                return Ok(());
            }
            if bitmap.is_empty() {
                self.delete_flat(hash, stats)?;
            } else if self.v2 && should_promote(&bitmap) {
                self.partitioned_replace(hash, &bitmap, stats)?;
            } else {
                self.write_flat(hash, &bitmap, stats)?;
            }
            return Ok(());
        };
        stats.existing_tokens += 1;
        let mut changed = false;
        for (shard, delta) in split(ids) {
            let old: Option<Vec<u8>> = timed(&mut stats.read, || {
                self.conn.prepare_cached(
                "SELECT postings_blob FROM blind_bitmap_chunks WHERE token_hash=?1 AND shard_id=?2"
            )?.query_row(params![hash,shard], |r| r.get(0)).optional()
            })
            .map_err(|e| error("read writable chunk", e))?;
            if remove && old.is_none() {
                continue;
            }
            stats.bytes_read += old.as_ref().map_or(0, Vec::len);
            let mut bitmap = timed(&mut stats.merge, || {
                old.as_ref()
                    .map_or_else(|| Ok(RoaringBitmap::new()), |b| decode_chunk(b, shard))
            })?;
            let before = bitmap.len();
            let old_metrics = old
                .as_ref()
                .map(|b| metrics_for(b, &bitmap))
                .transpose()?
                .unwrap_or_default();
            timed(&mut stats.merge, || {
                if remove {
                    bitmap -= &delta;
                } else {
                    bitmap |= &delta;
                }
            });
            if bitmap.len() == before {
                continue;
            }
            changed = true;
            metrics = metrics.adjust(old_metrics, -1)?;
            if bitmap.is_empty() {
                timed(&mut stats.write, || {
                    self.conn
                        .prepare_cached(
                            "DELETE FROM blind_bitmap_chunks WHERE token_hash=?1 AND shard_id=?2",
                        )?
                        .execute(params![hash, shard])
                })
                .map_err(|e| error("delete empty chunk", e))?;
            } else {
                let bytes = encode(&bitmap, stats)?;
                metrics = metrics.adjust(metrics_for(&bytes, &bitmap)?, 1)?;
                self.write_chunk(hash, shard, &bytes, stats)?;
            }
        }
        if !changed {
            return Ok(());
        }
        if metrics.cardinality == 0 {
            self.clear_chunks(hash, stats)?;
        } else {
            self.write_directory(hash, metrics, stats)?;
        }
        Ok(())
    }

    pub fn tokens_after(&self, after: &str, limit: i64) -> Result<Vec<String>, String> {
        let mut sql = format!(
            "SELECT token_hash FROM {} WHERE token_hash>?1",
            self.inline_table()
        );
        if self.v2 {
            sql.push_str(
                " UNION ALL SELECT token_hash FROM blind_bitmap_directory WHERE token_hash>?1",
            );
        }
        sql.push_str(" ORDER BY token_hash LIMIT ?2");
        self.conn
            .prepare(&sql)
            .map_err(|e| error("prepare token scan", e))?
            .query_map(params![after, limit], |r| r.get(0))
            .map_err(|e| error("scan tokens", e))?
            .collect::<Result<_, _>>()
            .map_err(|e| error("decode token scan", e))
    }

    pub fn count_after(&self, after: &str) -> Result<i64, String> {
        let mut sql = format!(
            "SELECT (SELECT COUNT(*) FROM {} WHERE token_hash>?1)",
            self.inline_table()
        );
        if self.v2 {
            sql.push_str(" + (SELECT COUNT(*) FROM blind_bitmap_directory WHERE token_hash>?1)");
        }
        self.conn
            .query_row(&sql, [after], |r| r.get(0))
            .map_err(|e| error("count tokens", e))
    }
}

#[cfg(test)]
mod tests;

fn decode(bytes: &[u8]) -> Result<RoaringBitmap, String> {
    RoaringBitmap::deserialize_from(bytes).map_err(|e| error("decode bitmap", e))
}

fn decode_chunk(bytes: &[u8], shard: u32) -> Result<RoaringBitmap, String> {
    let bitmap = decode(bytes)?;
    if bitmap.is_empty()
        || bitmap.min().unwrap() >> SHARD_BITS != shard
        || bitmap.max().unwrap() >> SHARD_BITS != shard
        || bitmap.serialized_size() != bytes.len()
    {
        return Err("Invalid blind index chunk range or encoding".into());
    }
    Ok(bitmap)
}

fn encode(bitmap: &RoaringBitmap, stats: &mut MutationStats) -> Result<Vec<u8>, String> {
    timed(&mut stats.serialize, || {
        let mut bytes = Vec::with_capacity(bitmap.serialized_size());
        bitmap
            .serialize_into(&mut bytes)
            .map_err(|e| error("encode bitmap", e))?;
        Ok(bytes)
    })
}

fn should_promote(bitmap: &RoaringBitmap) -> bool {
    bitmap.serialized_size() >= PROMOTION_BYTES
        && bitmap.min().unwrap() >> SHARD_BITS != bitmap.max().unwrap() >> SHARD_BITS
}

fn split(bitmap: &RoaringBitmap) -> Vec<(u32, RoaringBitmap)> {
    let Some(first) = bitmap.min() else {
        return Vec::new();
    };
    if first >> SHARD_BITS == bitmap.max().unwrap() >> SHARD_BITS {
        return vec![(first >> SHARD_BITS, bitmap.clone())];
    }
    let mut pieces = Vec::new();
    let mut next = Some(first);
    while let Some(id) = next {
        let end = id | SHARD_MASK;
        let mut mask = RoaringBitmap::new();
        mask.insert_range((id & !SHARD_MASK)..=end);
        pieces.push((id >> SHARD_BITS, bitmap & &mask));
        next = if end == u32::MAX {
            None
        } else {
            bitmap.range((Excluded(end), Unbounded)).next()
        };
    }
    pieces
}

fn metrics_for(bytes: &[u8], bitmap: &RoaringBitmap) -> Result<Metrics, String> {
    // Only called after decoding/validating or encoding with Roaring itself.
    let cookie = u32::from_le_bytes(
        bytes
            .get(..4)
            .ok_or("Truncated blind bitmap")?
            .try_into()
            .unwrap(),
    );
    let (containers, runs, header) = if cookie == 12346 {
        let n = u32::from_le_bytes(
            bytes
                .get(4..8)
                .ok_or("Truncated blind bitmap header")?
                .try_into()
                .unwrap(),
        ) as usize;
        (n, 0usize, 8 + 8 * n)
    } else if cookie & 65535 == 12347 {
        let n = (cookie >> 16) as usize + 1;
        let flags = n.div_ceil(8);
        let runs = bytes
            .get(4..4 + flags)
            .ok_or("Truncated blind bitmap flags")?
            .iter()
            .map(|b| b.count_ones() as usize)
            .sum();
        (n, runs, 4 + flags + 4 * n + if n >= 4 { 4 * n } else { 0 })
    } else {
        return Err("Invalid blind bitmap header".into());
    };
    Ok(Metrics {
        payload: bytes
            .len()
            .checked_sub(header)
            .ok_or("Invalid blind bitmap size")? as i64,
        containers: containers as i64,
        runs: runs as i64,
        cardinality: bitmap.len() as i64,
    })
}

impl Metrics {
    fn adjust(self, rhs: Self, sign: i64) -> Result<Self, String> {
        let add = |left: i64, right: i64| {
            left.checked_add(
                right
                    .checked_mul(sign)
                    .ok_or("Blind index metric overflow")?,
            )
            .filter(|v| *v >= 0)
            .ok_or_else(|| "Invalid blind index metric delta".to_string())
        };
        Ok(Self {
            payload: add(self.payload, rhs.payload)?,
            containers: add(self.containers, rhs.containers)?,
            runs: add(self.runs, rhs.runs)?,
            cardinality: add(self.cardinality, rhs.cardinality)?,
        })
    }
}
