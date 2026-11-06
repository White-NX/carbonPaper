//! Encrypted usage intervals. Times are UTC Unix milliseconds, never screenshot counts.
use super::timeline::{SqlCancellation, CANCELLED};
use super::{BackgroundReadError, StorageState};
use crate::credential_manager::decrypt_row_key_with_cng;
use crate::usage::{UsageIdentity, UsageSpan};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use zeroize::Zeroizing;

const PAGE_SIZE: usize = 500;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct UsageCursor {
    pub start_ms: i64,
    pub id: i64,
}

#[derive(Debug, Serialize)]
pub struct UsageTimelineInterval {
    pub id: i64,
    pub run_id: String,
    pub start_ms: i64,
    pub end_ms: i64,
    pub process_name: Option<String>,
    pub process_path: Option<String>,
    pub window_title: Option<String>,
    pub input_active: bool,
    pub media_active: bool,
    pub end_reason: String,
}

#[derive(Debug, Serialize)]
pub struct UsageTimelinePage {
    pub cache_epoch: String,
    pub tracking_started_ms: Option<i64>,
    pub intervals: Vec<UsageTimelineInterval>,
    pub next_cursor: Option<UsageCursor>,
}

struct RawInterval {
    id: i64,
    run_id: String,
    start_ms: i64,
    end_ms: i64,
    identity_enc: Vec<u8>,
    identity_key_enc: Vec<u8>,
    input_active: bool,
    media_active: bool,
    end_reason: String,
}

impl StorageState {
    pub(crate) fn usage_revision(&self) -> u64 {
        self.usage_revision.load(Ordering::Acquire)
    }

    pub(crate) fn begin_usage_tracking(
        &self,
        start_ms: i64,
        generation: u64,
    ) -> Result<(), String> {
        let guard = self.get_connection_named("begin_usage_tracking")?;
        if self.db_generation() != generation {
            return Err("USAGE_DATABASE_CHANGED".into());
        }
        guard
            .as_ref()
            .unwrap()
            .execute(
                "INSERT OR IGNORE INTO usage_tracking(singleton,started_ms) VALUES(1,?1)",
                [start_ms],
            )
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub(crate) fn save_usage_span(
        &self,
        span: &UsageSpan,
        generation: u64,
        revision: u64,
    ) -> Result<(), String> {
        let guard = self.get_connection_named("save_usage_span")?;
        if self.db_generation() != generation || self.usage_revision() != revision {
            return Err("USAGE_DATABASE_CHANGED".into());
        }
        let conn = guard.as_ref().unwrap();
        let exists = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM usage_intervals WHERE run_id=?1 AND sequence=?2)",
                params![span.run_id, span.sequence],
                |row| row.get::<_, bool>(0),
            )
            .map_err(|e| e.to_string())?;
        if exists {
            // Checkpoints never decrypt metadata, overwrite identity, or revive deleted data.
            conn.execute(
                "UPDATE usage_intervals SET end_ms=MAX(end_ms,?3),end_reason=?4
                 WHERE run_id=?1 AND sequence=?2 AND is_deleted=0",
                params![span.run_id, span.sequence, span.end_ms, span.end_reason],
            )
            .map_err(|e| e.to_string())?;
        } else {
            let payload =
                Zeroizing::new(serde_json::to_vec(&span.identity).map_err(|e| e.to_string())?);
            let (encrypted, key) = self.encrypt_payload_with_row_key(&payload)?;
            let process = self
                .protected
                .process_token(conn, &span.identity.process_name)?;
            conn.execute(
                "INSERT INTO usage_intervals(run_id,sequence,start_ms,end_ms,process_name,
                    identity_enc,identity_key_enc,input_active,media_active,end_reason)
                 VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
                params![
                    span.run_id,
                    span.sequence,
                    span.start_ms,
                    span.end_ms,
                    process,
                    encrypted,
                    key,
                    span.input_active,
                    span.media_active,
                    span.end_reason
                ],
            )
            .map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    pub(crate) fn get_usage_timeline(
        &self,
        start_ms: i64,
        end_ms: i64,
        cursor: Option<UsageCursor>,
        cached_epoch: Option<&str>,
        known_ids: &[i64],
        cancelled: &Arc<AtomicBool>,
    ) -> Result<UsageTimelinePage, String> {
        if start_ms < 0 || start_ms > end_ms {
            return Err("Invalid usage timeline range".into());
        }
        let admitted_session = self.credential_state.session_marker();
        let generation = self.db_generation();
        let revision = self.usage_revision();
        let cache_epoch = hex::encode(Sha256::digest(format!(
            "{generation}:{revision}:{admitted_session:?}"
        )));
        let known: HashSet<i64> = if cached_epoch == Some(cache_epoch.as_str()) {
            known_ids.iter().copied().collect()
        } else {
            HashSet::new()
        };
        let check = || {
            if cancelled.load(Ordering::Acquire) {
                Err(CANCELLED.to_owned())
            } else if self.db_generation() != generation || self.usage_revision() != revision {
                Err(CANCELLED.to_owned())
            } else if !self.credential_state.is_session_valid()
                || self.credential_state.silent_read_auth_required()
                || self.credential_state.session_marker() != admitted_session
            {
                Err("AUTH_REQUIRED".to_owned())
            } else {
                Ok(())
            }
        };
        check()?;
        let (tracking_started_ms, mut raw) = {
            let conn = self.open_read_connection_named("get_usage_timeline")?;
            let credentials = self.credential_state.clone();
            let stop = cancelled.clone();
            let _cancellation = SqlCancellation::install(&conn, move || {
                stop.load(Ordering::Acquire)
                    || !credentials.is_session_valid()
                    || credentials.silent_read_auth_required()
                    || credentials.session_marker() != admitted_session
            })?;
            let started = conn
                .query_row(
                    "SELECT started_ms FROM usage_tracking WHERE singleton=1",
                    [],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|e| e.to_string())?;
            let rows = read_intervals(&conn, start_ms, end_ms, cursor.as_ref(), &check);
            check()?;
            (started, rows?)
        };
        let more = raw.len() > PAGE_SIZE;
        raw.truncate(PAGE_SIZE);
        let next_cursor = more.then(|| {
            let last = raw.last().unwrap();
            UsageCursor {
                start_ms: last.start_ms,
                id: last.id,
            }
        });
        let mut intervals = Vec::with_capacity(raw.len());
        for row in raw {
            check()?;
            let (process_name, process_path, window_title) = if known.contains(&row.id) {
                (None, None, None)
            } else {
                let payload = Self::decrypt_payload_with_unwrap(
                    &row.identity_enc,
                    &row.identity_key_enc,
                    &|ciphertext| decrypt_row_key_with_cng(&self.credential_state, ciphertext),
                );
                check()?;
                let payload = Zeroizing::new(payload.map_err(|error| match error {
                    BackgroundReadError::AuthRequired => "AUTH_REQUIRED".to_owned(),
                    BackgroundReadError::Other(message) => message,
                })?);
                let identity: UsageIdentity = serde_json::from_slice(&payload)
                    .map_err(|e| format!("Invalid usage identity: {e}"))?;
                (
                    Some(identity.process_name),
                    Some(identity.process_path),
                    Some(identity.window_title),
                )
            };
            intervals.push(UsageTimelineInterval {
                id: row.id,
                run_id: row.run_id,
                start_ms: row.start_ms,
                end_ms: row.end_ms,
                process_name,
                process_path,
                window_title,
                input_active: row.input_active,
                media_active: row.media_active,
                end_reason: row.end_reason,
            });
        }
        check()?;
        Ok(UsageTimelinePage {
            cache_epoch,
            tracking_started_ms,
            intervals,
            next_cursor,
        })
    }

    pub(super) fn delete_usage_range(
        &self,
        conn: &Connection,
        start_ms: i64,
        end_ms: i64,
        process_tokens: Option<&str>,
    ) -> Result<(), String> {
        if trim_usage_range(conn, start_ms, end_ms, process_tokens)? > 0 {
            self.usage_revision.fetch_add(1, Ordering::AcqRel);
        }
        Ok(())
    }
}

fn read_intervals(
    conn: &Connection,
    start_ms: i64,
    end_ms: i64,
    cursor: Option<&UsageCursor>,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<Vec<RawInterval>, String> {
    let mut stmt = conn
        .prepare(
            "WITH picks AS MATERIALIZED (
            SELECT id FROM usage_intervals INDEXED BY idx_usage_intervals_end
            WHERE is_deleted=0 AND start_ms<?2 AND end_ms>?1
              AND (start_ms,id)>(?3,?4) ORDER BY start_ms,id LIMIT ?5
         ) SELECT u.id,u.run_id,u.start_ms,u.end_ms,u.identity_enc,u.identity_key_enc,
            u.input_active,u.media_active,u.end_reason
         FROM picks CROSS JOIN usage_intervals u ON u.id=picks.id ORDER BY u.start_ms,u.id",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(
            params![
                start_ms,
                end_ms,
                cursor.map_or(i64::MIN, |cursor| cursor.start_ms),
                cursor.map_or(0, |cursor| cursor.id),
                PAGE_SIZE as i64 + 1
            ],
            |row| {
                Ok(RawInterval {
                    id: row.get(0)?,
                    run_id: row.get(1)?,
                    start_ms: row.get(2)?,
                    end_ms: row.get(3)?,
                    identity_enc: row.get(4)?,
                    identity_key_enc: row.get(5)?,
                    input_active: row.get(6)?,
                    media_active: row.get(7)?,
                    end_reason: row.get(8)?,
                })
            },
        )
        .map_err(|e| e.to_string())?;
    let mut result = Vec::new();
    for row in rows {
        check()?;
        result.push(row.map_err(|e| e.to_string())?);
    }
    Ok(result)
}

/// Preserve the portions outside a deleted half-open range. Ciphertext is copied,
/// never decrypted, and a deleted original cannot be revived by a late checkpoint.
fn trim_usage_range(
    conn: &Connection,
    start_ms: i64,
    end_ms: i64,
    process_tokens: Option<&str>,
) -> Result<usize, String> {
    if start_ms >= end_ms {
        return Ok(0);
    }
    let filter = "is_deleted=0 AND start_ms<?2 AND end_ms>?1
        AND (?3 IS NULL OR process_name IN (SELECT value FROM json_each(?3)))";
    let params = params![start_ms, end_ms, process_tokens];
    for (side, start, end) in [
        ("before", "start_ms", "MIN(end_ms,?1)"),
        ("after", "MAX(start_ms,?2)", "end_ms"),
    ] {
        conn.execute(
            &format!(
                "INSERT INTO usage_intervals(run_id,sequence,start_ms,end_ms,process_name,
                identity_enc,identity_key_enc,input_active,media_active,end_reason)
             SELECT run_id || ':deleted:' || id || ':{side}:' || ?1 || ':' || ?2,
                sequence,{start},{end},process_name,identity_enc,identity_key_enc,
                input_active,media_active,'history_deleted'
             FROM usage_intervals WHERE {filter} AND {end}>{start}"
            ),
            params,
        )
        .map_err(|e| e.to_string())?;
    }
    // Retained fragments do not overlap the range and therefore are not selected.
    conn.execute(
        &format!(
            "UPDATE usage_intervals SET is_deleted=1,process_name='',
        identity_enc=x'',identity_key_enc=x'' WHERE {filter}"
        ),
        params,
    )
    .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> Connection {
        let temp = tempfile::tempdir().unwrap();
        let credentials = Arc::new(crate::credential_manager::CredentialManagerState::new(
            temp.path().into(),
        ));
        let storage = StorageState::new(temp.path().into(), credentials);
        let conn = Connection::open_in_memory().unwrap();
        storage.init_tables(&conn).unwrap();
        conn
    }

    fn insert(conn: &Connection, sequence: i64, start: i64, end: i64, process: &str) {
        conn.execute(
            "INSERT INTO usage_intervals(run_id,sequence,start_ms,end_ms,process_name,
            identity_enc,identity_key_enc,input_active,media_active,end_reason)
            VALUES('run',?1,?2,?3,?4,x'01',x'02',1,0,'observing')",
            params![sequence, start, end, process],
        )
        .unwrap();
    }

    #[test]
    fn checkpoints_and_history_fences_do_not_overwrite_identity_or_revive_deleted_usage() {
        let temp = tempfile::tempdir().unwrap();
        let credentials = Arc::new(crate::credential_manager::CredentialManagerState::new(
            temp.path().into(),
        ));
        let storage = StorageState::new(temp.path().into(), credentials);
        let conn = Connection::open_in_memory().unwrap();
        storage.init_tables(&conn).unwrap();
        insert(&conn, 1, 0, 1000, "player");
        *storage.db.lock().unwrap() = Some(conn);
        let span = UsageSpan {
            run_id: "run".into(),
            sequence: 1,
            start_ms: 0,
            end_ms: 2000,
            input_active: true,
            media_active: false,
            end_reason: "idle".into(),
            identity: UsageIdentity {
                process_name: "player.exe".into(),
                process_path: "player.exe".into(),
                window_title: "Film".into(),
                hwnd: 1,
                pid: 1,
            },
        };
        storage.save_usage_span(&span, 0, 0).unwrap();
        {
            let guard = storage
                .get_connection_named("verify_usage_checkpoint")
                .unwrap();
            let conn = guard.as_ref().unwrap();
            assert_eq!(
                conn.query_row(
                    "SELECT end_ms,identity_enc FROM usage_intervals",
                    [],
                    |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?))
                )
                .unwrap(),
                (2000, vec![1])
            );
            storage.delete_usage_range(conn, 0, 3000, None).unwrap();
        }
        assert!(storage.save_usage_span(&span, 0, 0).is_err());
        assert!(storage.save_usage_span(&span, 1, 1).is_err());
        storage.save_usage_span(&span, 0, 1).unwrap();
        let guard = storage
            .get_connection_named("verify_deleted_usage")
            .unwrap();
        assert_eq!(
            guard
                .as_ref()
                .unwrap()
                .query_row(
                    "SELECT is_deleted,length(identity_enc),length(identity_key_enc)
            FROM usage_intervals",
                    [],
                    |row| Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?
                    ))
                )
                .unwrap(),
            (1, 0, 0)
        );
    }

    #[test]
    fn invalid_deletion_bounds_are_rejected_before_either_store_is_touched() {
        let temp = tempfile::tempdir().unwrap();
        let credentials = Arc::new(crate::credential_manager::CredentialManagerState::new(
            temp.path().into(),
        ));
        let storage = StorageState::new(temp.path().into(), credentials);
        for (start, end) in [
            (f64::NAN, f64::INFINITY),
            (-1.0, 1000.0),
            (1000.0, 0.0),
            (0.0, f64::MAX),
        ] {
            assert_eq!(
                storage
                    .delete_screenshots_by_time_range(start, end)
                    .unwrap_err(),
                "Invalid deletion time range"
            );
        }
    }

    #[test]
    fn range_reads_include_spanning_films_and_page_without_sampling_usage() {
        let conn = fixture();
        insert(&conn, 1, 0, 7_200_000, "player");
        for index in 2..=503 {
            insert(&conn, index, 1_000, 2_000, "editor");
        }
        let rows = read_intervals(&conn, 1_500, 1_800, None, &|| Ok(())).unwrap();
        assert_eq!(rows.len(), PAGE_SIZE + 1);
        assert_eq!(rows[0].start_ms, 0);
        let cursor = UsageCursor {
            start_ms: rows[499].start_ms,
            id: rows[499].id,
        };
        let next = read_intervals(&conn, 1_500, 1_800, Some(&cursor), &|| Ok(())).unwrap();
        assert_eq!(next.len(), 3);
        assert!(next.iter().all(|row| row.id > cursor.id));
        assert!(read_intervals(&conn, 0, 3_000, None, &|| Err(CANCELLED.into())).is_err());
    }

    #[test]
    fn deleting_a_range_preserves_both_sides_and_process_scope() {
        let conn = fixture();
        insert(&conn, 1, 0, 10_000, "player");
        insert(&conn, 2, 0, 10_000, "editor");
        trim_usage_range(&conn, 2_000, 8_000, Some(r#"["player"]"#)).unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT process_name,start_ms,end_ms FROM usage_intervals
            WHERE is_deleted=0 ORDER BY process_name,start_ms",
            )
            .unwrap();
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(
            rows,
            vec![
                ("editor".into(), 0, 10_000),
                ("player".into(), 0, 2_000),
                ("player".into(), 8_000, 10_000)
            ]
        );
        assert_eq!(
            conn.query_row(
                "SELECT is_deleted FROM usage_intervals WHERE sequence=1 AND run_id='run'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
            1
        );
    }
}
