use super::*;
use crate::credential_manager::CredentialManagerState;
use rusqlite::StatementStatus;
use std::sync::Arc;

// Frozen shape from 31b13e1. The outer time predicate lets SQLite reverse the
// join, scanning screenshots and restarting the grouped picks coroutine.
const REGRESSED_SQL: &str = "SELECT s.id, s.image_path, s.created_at,
        CAST(strftime('%s', s.created_at) AS INTEGER), s.window_title, s.process_name,
        s.metadata, s.window_title_enc, s.process_name_enc, s.metadata_enc,
        s.content_key_encrypted, s.page_icon_enc, s.page_icon_id,
        pi.icon_enc, pi.icon_key_encrypted, s.category FROM screenshots s
    JOIN (SELECT MIN(id) AS picked_id FROM screenshots
          WHERE is_deleted = 0 AND created_at BETWEEN ?1 AND ?2
          GROUP BY CAST(strftime('%s', created_at) AS INTEGER) / 21600) picks
      ON picks.picked_id = s.id
    LEFT JOIN page_icons pi ON pi.id = s.page_icon_id
    WHERE s.is_deleted = 0 AND s.created_at BETWEEN ?1 AND ?2
    ORDER BY s.created_at ASC, s.id ASC LIMIT ?3";

fn fixture(rows: i64) -> (tempfile::TempDir, StorageState, Connection) {
    let temp = tempfile::tempdir().unwrap();
    let credentials = Arc::new(CredentialManagerState::new(temp.path().into()));
    let storage = StorageState::new(temp.path().into(), credentials);
    let conn = Connection::open_in_memory().unwrap();
    storage.init_tables(&conn).unwrap();
    conn.execute("WITH RECURSIVE ids(id) AS (SELECT 1 UNION ALL SELECT id+1 FROM ids WHERE id < ?1)
        INSERT INTO screenshots(id,image_path,image_hash,created_at,metadata_enc)
        SELECT id, 'test.enc', CAST(id AS TEXT), datetime(1767225600 + id * 60, 'unixepoch'), zeroblob(256) FROM ids", [rows]).unwrap();
    (temp, storage, conn)
}

#[test]
#[ignore = "offline query-plan comparison; synthetic rows only"]
fn compare_regressed_query_plan() {
    for count in [1000, 4000] {
        let (_temp, _storage, conn) = fixture(count);
        let corrected = REGRESSED_SQL.replace(
            "WHERE s.is_deleted = 0 AND s.created_at BETWEEN ?1 AND ?2",
            "",
        );
        let production = timeline_rows_sql(Some(21600));
        for (label, sql) in [
            ("regressed", REGRESSED_SQL),
            ("without_outer_range", corrected.as_str()),
            ("fixed_production", production.as_str()),
        ] {
            let plan = conn
                .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
                .unwrap()
                .query_map(params!["2026-01-01", "2026-04-01", 500], |row| {
                    row.get::<_, String>(3)
                })
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap();
            let mut stmt = conn.prepare(sql).unwrap();
            let started = Instant::now();
            let results = stmt
                .query_map(params!["2026-01-01", "2026-04-01", 500], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap();
            eprintln!(
                "{label}: input={count} output={} elapsed={:?} steps={} plan={plan:?}",
                results.len(),
                started.elapsed(),
                stmt.get_status(StatementStatus::VmStep)
            );
        }
    }
}

#[test]
fn sampled_query_uses_primary_key_hydration_with_linear_work() {
    for count in [1000, 4000, 16000] {
        let (_temp, _storage, conn) = fixture(count);
        // IDs and capture time need not agree after import; keep MIN(id)
        // semantics, UTC bucket boundaries, sorting and soft-deletion intact.
        conn.execute_batch(
            "UPDATE screenshots SET created_at = '2026-01-05 12:00:00' WHERE id = 1;
            UPDATE screenshots SET is_deleted = 1 WHERE id = 360;",
        )
        .unwrap();
        for analyzed in [false, true] {
            if analyzed {
                conn.execute_batch("ANALYZE").unwrap();
            }
            for bucket in [None, Some(21600)] {
                let sql = timeline_rows_sql(bucket);
                let plan = conn
                    .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
                    .unwrap()
                    .query_map(params!["2026-01-01", "2026-04-01", 500], |row| {
                        row.get::<_, String>(3)
                    })
                    .unwrap()
                    .collect::<Result<Vec<_>, _>>()
                    .unwrap();
                let mut stmt = conn.prepare(&sql).unwrap();
                let actual = stmt
                    .query_map(params!["2026-01-01", "2026-04-01", 500], |row| {
                        row.get::<_, i64>(0)
                    })
                    .unwrap()
                    .collect::<Result<Vec<_>, _>>()
                    .unwrap();
                let steps = stmt.get_status(StatementStatus::VmStep);
                assert!(
                    plan.iter().any(|step| step == "MATERIALIZE picks"),
                    "{plan:?}"
                );
                assert!(
                    plan.iter()
                        .any(|step| step.contains("SEARCH s USING INTEGER PRIMARY KEY")),
                    "{plan:?}"
                );
                assert!(
                    !plan.iter().any(|step| step.contains("CO-ROUTINE")),
                    "{plan:?}"
                );
                assert!(
                    steps < 100 * count as i32 + 50_000,
                    "count={count} analyzed={analyzed} bucket={bucket:?} steps={steps}: {plan:?}"
                );
                let expected_sql = if let Some(bucket) = bucket {
                    format!(
                        "SELECT id FROM screenshots NOT INDEXED WHERE id IN (
                        SELECT MIN(id) FROM screenshots WHERE is_deleted = 0
                        AND created_at BETWEEN ?1 AND ?2
                        GROUP BY CAST(strftime('%s', created_at) AS INTEGER) / {bucket})
                        ORDER BY created_at, id LIMIT ?3"
                    )
                } else {
                    "SELECT id FROM screenshots WHERE is_deleted = 0 AND created_at BETWEEN ?1 AND ?2
                     ORDER BY created_at, id LIMIT ?3".into()
                };
                let expected = conn
                    .prepare(&expected_sql)
                    .unwrap()
                    .query_map(params!["2026-01-01", "2026-04-01", 500], |row| {
                        row.get::<_, i64>(0)
                    })
                    .unwrap()
                    .collect::<Result<Vec<_>, _>>()
                    .unwrap();
                assert_eq!(actual, expected);
            }
        }
    }
}

#[test]
fn cancellation_interrupts_sql_before_first_row_and_removes_handler() {
    let (_temp, _storage, conn) = fixture(4000);
    let cancelled = Arc::new(AtomicBool::new(false));
    let stop = Arc::clone(&cancelled);
    let sql = timeline_rows_sql(Some(21600));
    let mut stmt = conn.prepare(&sql).unwrap();
    // Simulate a UI cancellation delivered while sqlite3_step is evaluating
    // the first result. The old per-result loop cannot observe this signal.
    let cancellation = SqlCancellation::install(&conn, move || {
        stop.store(true, Ordering::Release);
        stop.load(Ordering::Acquire)
    })
    .unwrap();
    let mut rows = stmt
        .query(params!["2026-01-01", "2026-04-01", 500])
        .unwrap();
    let error = rows.next().unwrap_err();
    assert_eq!(
        error.sqlite_error_code(),
        Some(rusqlite::ErrorCode::OperationInterrupted)
    );
    drop(rows);
    assert!(cancelled.load(Ordering::Acquire));
    assert!(stmt.get_status(StatementStatus::VmStep) < 5000);
    drop(cancellation);
    let mut timings = Timings::default();
    let rows = read_rows(
        &conn,
        1767225600.0,
        1775001600.0,
        500,
        &|| Ok(()),
        &mut timings,
    )
    .unwrap();
    assert!(
        !rows.is_empty(),
        "a cancelled query must not poison the connection"
    );
}

#[test]
#[ignore = "read-only snapshot benchmark; requires CARBONPAPER_TIMELINE_BENCH_DB"]
fn compare_snapshot_queries() {
    let path = std::env::var("CARBONPAPER_TIMELINE_BENCH_DB").expect("set snapshot path");
    let conn =
        Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    conn.execute_batch("PRAGMA query_only = ON").unwrap();
    eprintln!(
        "engine={} cipher={}",
        rusqlite::version(),
        conn.query_row("PRAGMA cipher_version", [], |r| r.get::<_, String>(0))
            .unwrap()
    );
    for days in [7, 30, 90] {
        let (start, end, span): (String, String, f64) = conn.query_row(
            "SELECT datetime(max(created_at), ?1), max(created_at), ?2 FROM screenshots WHERE is_deleted = 0",
            params![format!("-{days} days"), days as f64 * 86400.0], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?))).unwrap();
        let bucket = StorageState::snap_bucket_seconds(span / 500.0);
        let old = REGRESSED_SQL.replace("/ 21600", &format!("/ {bucket}"));
        let new = timeline_rows_sql(Some(bucket));
        let reference = old.replace(
            "WHERE s.is_deleted = 0 AND s.created_at BETWEEN ?1 AND ?2",
            "",
        );
        let expected = conn
            .prepare(&reference)
            .unwrap()
            .query_map(params![&start, &end, 500], |row| row.get::<_, i64>(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        for (label, sql) in [("regressed", old), ("fixed", new)] {
            let started = Instant::now();
            let deadline = started + Duration::from_secs(2);
            let _cancel =
                SqlCancellation::install(&conn, move || Instant::now() >= deadline).unwrap();
            let mut stmt = conn.prepare(&sql).unwrap();
            let results = stmt
                .query_map(params![&start, &end, 500], |row| row.get::<_, i64>(0))
                .unwrap()
                .collect::<Result<Vec<_>, _>>();
            let elapsed = started.elapsed();
            eprintln!(
                "{label}: days={days} output={:?} elapsed={elapsed:?} steps={} interrupted={}",
                results.as_ref().ok().map(Vec::len),
                stmt.get_status(StatementStatus::VmStep),
                results.is_err()
            );
            if label == "fixed" {
                assert_eq!(results.unwrap(), expected);
            }
        }
    }
}
