//! Exercise the production SQL with the production schema and bundled engine.
//! Timed comparisons are opt-in; normal tests assert results and VM work bounds.

use super::*;
use crate::credential_manager::CredentialManagerState;
use rusqlite::{types::Value, OpenFlags, StatementStatus};
use std::sync::Arc;

fn populate(conn: &Connection, blocks: i64, payload_bytes: i64) {
    let directory = tempfile::tempdir().unwrap();
    let credential = Arc::new(CredentialManagerState::new(directory.path().into()));
    let storage = StorageState::new(directory.path().into(), credential);
    storage.init_tables(conn).unwrap();
    conn.execute(
        "WITH RECURSIVE ids(id) AS (
             SELECT 1 UNION ALL SELECT id + 1 FROM ids WHERE id < ?1
         )
         INSERT INTO screenshots
             (id, image_path, image_hash, created_at, process_name,
              window_title_enc, content_key_encrypted, is_deleted)
         SELECT id, 'synthetic.enc', CAST(id AS TEXT),
                datetime(1767225600 + (id * 97) % 86400, 'unixepoch'),
                'synthetic.exe', zeroblob(?2), zeroblob(?2), id % 29 = 0
         FROM ids",
        params![(blocks + 19) / 20, payload_bytes],
    )
    .unwrap();
    conn.execute(
        "WITH RECURSIVE ids(id) AS (
             SELECT 1 UNION ALL SELECT id + 1 FROM ids WHERE id < ?1
         )
         INSERT INTO ocr_results
             (id, screenshot_id, text_hash, text_enc, text_key_encrypted,
              confidence, box_x1, box_y1, box_x2, box_y2,
              box_x3, box_y3, box_x4, box_y4, created_at, is_deleted)
         SELECT id, (id + 19) / 20, '', zeroblob(?2), zeroblob(?2),
                0.9, 1, 2, 3, 4, 5, 6, 7, 8,
                '2026-01-02 12:00:00', id % 13 = 0
         FROM ids",
        params![blocks, payload_bytes],
    )
    .unwrap();
}

// Freeze the old query shapes rather than deriving them by removing hints
// from the production SQL, so this remains a useful independent reference.
fn previous_rows_sql(columns: &str, count: usize) -> String {
    let placeholders = vec!["?"; count].join(",");
    format!(
        "SELECT {columns}
         FROM ocr_results r JOIN screenshots s ON r.screenshot_id = s.id
         WHERE r.id IN ({placeholders}) AND r.is_deleted = 0 AND s.is_deleted = 0"
    )
}

fn previous_screenshot_ids_sql(count: usize) -> String {
    let placeholders = vec!["?"; count].join(",");
    format!(
        "SELECT DISTINCT screenshot_id FROM ocr_results
         WHERE id IN ({placeholders}) AND is_deleted = 0"
    )
}

const PREVIOUS_RECENT_SQL: &str =
    "SELECT r.id FROM ocr_results r JOIN screenshots s ON r.screenshot_id = s.id
     WHERE r.is_deleted = 0 AND s.is_deleted = 0 ORDER BY r.id DESC LIMIT ?";

#[derive(serde::Serialize)]
struct Measurement {
    elapsed_ms: f64,
    vm_steps: i32,
    sorts: i32,
    rows: usize,
}

fn measure(conn: &Connection, sql: &str, params: &[&dyn ToSql]) -> (Vec<Vec<Value>>, Measurement) {
    let started = Instant::now();
    let mut stmt = conn.prepare(sql).unwrap();
    let columns = stmt.column_count();
    let rows = stmt
        .query_map(params, |row| {
            (0..columns)
                .map(|column| row.get::<_, Value>(column))
                .collect::<rusqlite::Result<Vec<_>>>()
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    let measurement = Measurement {
        elapsed_ms: millis(started.elapsed()),
        vm_steps: stmt.get_status(StatementStatus::VmStep),
        sorts: stmt.get_status(StatementStatus::Sort),
        rows: rows.len(),
    };
    (rows, measurement)
}

fn plan(conn: &Connection, sql: &str, params: &[&dyn ToSql]) -> Vec<String> {
    conn.prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
        .unwrap()
        .query_map(params, |row| row.get(3))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

fn canonical(mut rows: Vec<Vec<Value>>) -> Vec<Vec<Value>> {
    rows.sort_by_key(|row| match row[0] {
        Value::Integer(id) => id,
        _ => panic!("expected an id as the first column"),
    });
    rows
}

fn query_pairs(count: usize) -> [(String, String, &'static str); 3] {
    [
        (
            previous_rows_sql(CANDIDATE_COLUMNS, count),
            search_rows_by_id_sql(CANDIDATE_COLUMNS, count),
            "candidates",
        ),
        (
            previous_rows_sql(SEARCH_RESULT_COLUMNS, count),
            search_rows_by_id_sql(SEARCH_RESULT_COLUMNS, count),
            "page",
        ),
        (
            previous_screenshot_ids_sql(count),
            screenshot_ids_for_ocr_sql(count),
            "screenshots",
        ),
    ]
}

#[test]
fn id_lookups_keep_their_results_and_work_bound_as_history_grows() {
    for blocks in [1_000, 4_000, 16_000] {
        let conn = Connection::open_in_memory().unwrap();
        populate(&conn, blocks, 16);
        for analyzed in [false, true] {
            if analyzed {
                conn.execute_batch("ANALYZE").unwrap();
            }
            for count in [20, 211, SQL_CHUNK] {
                // Mix live/deleted rows and screenshots, duplicates and missing
                // ids. Screenshot timestamps deliberately do not follow ids.
                let mut ids: Vec<i64> = (0..count).map(|i| (i as i64 * 41) % blocks + 1).collect();
                ids[0] = 0;
                ids[1] = blocks + 1;
                ids[2] = ids[3];
                let params: Vec<&dyn ToSql> = ids.iter().map(|id| id as &dyn ToSql).collect();
                for (previous, current, label) in query_pairs(count) {
                    let (expected, _) = measure(&conn, &previous, &params);
                    let (actual, measured) = measure(&conn, &current, &params);
                    assert_eq!(canonical(actual), canonical(expected));
                    let plan = plan(&conn, &current, &params);
                    assert!(
                        plan.iter()
                            .any(|step| step.contains("SEARCH r USING INTEGER PRIMARY KEY")),
                        "{label}: {plan:?}"
                    );
                    assert!(
                        !plan.iter().any(|step| step == "SCAN r"
                            || step == "SCAN s"
                            || step.contains("idx_ocr_deleted_screenshot")),
                        "{label}: {plan:?}"
                    );
                    // VM work, unlike wall time, is stable under CI load. The
                    // bound depends on requested ids, never on archive size.
                    assert!(
                        measured.vm_steps < 5_000 + count as i32 * 64,
                        "{label}: blocks={blocks} analyzed={analyzed} steps={} plan={plan:?}",
                        measured.vm_steps
                    );
                }
            }
        }
    }
}

#[test]
fn single_character_scan_reads_the_live_tail_without_sorting_history() {
    for blocks in [1_000, 4_000, 16_000] {
        let conn = Connection::open_in_memory().unwrap();
        populate(&conn, blocks, 0);
        for analyzed in [false, true] {
            if analyzed {
                conn.execute_batch("ANALYZE").unwrap();
            }
            let window = 64i64;
            let params: [&dyn ToSql; 1] = [&window];
            let (expected, _) = measure(&conn, PREVIOUS_RECENT_SQL, &params);
            let (actual, measured) = measure(&conn, RECENT_CANDIDATE_SQL, &params);
            assert_eq!(actual, expected);
            assert_eq!(actual.len(), window as usize);
            assert_eq!(measured.sorts, 0);
            let plan = plan(&conn, RECENT_CANDIDATE_SQL, &params);
            assert!(
                !plan.iter().any(|step| step.contains("BLOOM FILTER")),
                "{plan:?}"
            );
            assert!(
                measured.vm_steps < window as i32 * 40 + 1_000,
                "blocks={blocks} analyzed={analyzed} steps={} plan={plan:?}",
                measured.vm_steps
            );
            let ids =
                recent_candidate_ids(&conn, None, window as usize, &mut SearchCounts::default())
                    .unwrap();
            let actual_ids: Vec<_> = actual.into_iter().map(|row| row[0].clone()).collect();
            assert_eq!(
                actual_ids,
                ids.into_iter().map(Value::Integer).collect::<Vec<_>>()
            );
        }
    }
}

#[test]
fn newest_ids_preserves_order_across_sparse_dense_and_run_containers() {
    let mut bitmap = RoaringBitmap::new();
    bitmap.insert_range(0..200_000);
    bitmap.extend([1_000_000, u32::MAX - 1, u32::MAX]);
    for optimize in [false, true] {
        if optimize {
            bitmap.optimize();
        }
        assert_eq!(
            newest_ids(&bitmap, 5),
            vec![
                i64::from(u32::MAX),
                i64::from(u32::MAX - 1),
                1_000_000,
                199_999,
                199_998
            ]
        );
        assert_eq!(newest_ids(&bitmap, 0), Vec::<i64>::new());
    }
}

fn compare_pairs(
    conn: &Connection,
    ids: &[i64],
    context: serde_json::Value,
) -> Vec<serde_json::Value> {
    let mut reports = Vec::new();
    for count in [20, SQL_CHUNK] {
        let params: Vec<&dyn ToSql> = ids[..count].iter().map(|id| id as &dyn ToSql).collect();
        for (previous, current, label) in query_pairs(count) {
            let plans = [
                plan(conn, &previous, &params),
                plan(conn, &current, &params),
            ];
            let mut samples: [Vec<Measurement>; 2] = [Vec::new(), Vec::new()];
            for round in 0..5 {
                let mut results = [Vec::new(), Vec::new()];
                // Alternate first reader. Both statements prepare, select and
                // decode the full projection; neither decrypts field payloads.
                for variant in [round % 2, 1 - round % 2] {
                    let sql = if variant == 0 { &previous } else { &current };
                    let (rows, measured) = measure(conn, sql, &params);
                    results[variant] = canonical(rows);
                    samples[variant].push(measured);
                }
                // Do not print payloads if a real snapshot ever differs.
                assert!(results[0] == results[1], "{label} query results differ");
            }
            let report = serde_json::json!({
                "context": context, "query": label, "requested": count,
                "plans": plans, "samples": samples,
            });
            eprintln!("{report}");
            reports.push(report);
        }
    }
    reports
}

fn write_report(name: &str, reports: Vec<serde_json::Value>) {
    if let Ok(directory) = std::env::var("CARBONPAPER_OCR_BENCH_OUT") {
        let path = std::path::Path::new(&directory);
        std::fs::create_dir_all(path).unwrap();
        let report = serde_json::json!({
            "sqlite": rusqlite::version(),
            "debug_assertions": cfg!(debug_assertions),
            "reports": reports,
        });
        std::fs::write(
            path.join(format!("{name}.json")),
            serde_json::to_vec_pretty(&report).unwrap(),
        )
        .unwrap();
    }
}

#[test]
#[ignore = "read-only snapshot comparison; requires CARBONPAPER_OCR_BENCH_DB"]
fn compare_snapshot_queries() {
    let path = std::env::var("CARBONPAPER_OCR_BENCH_DB").expect("set snapshot path");
    let before = std::fs::metadata(&path).unwrap();
    let conn = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    // The database page key is derived from the public key by the production
    // connection policy. This optional path reads only metadata and encrypted
    // payloads; it never opens a private key or unwraps any field encryption.
    let page_encrypted =
        if let Ok(public_key_path) = std::env::var("CARBONPAPER_OCR_BENCH_PUBLIC_KEY") {
            let public_key = std::fs::read(public_key_path).unwrap();
            let key = zeroize::Zeroizing::new(
                crate::credential_manager::derive_db_key_from_public_key(&public_key),
            );
            super::super::connection::configure_sqlcipher_connection(&conn, &key).unwrap();
            true
        } else {
            false
        };
    conn.execute_batch("PRAGMA query_only = ON; PRAGMA cache_size = -2048;")
        .unwrap();
    let _deadline = super::super::connection::QueryDeadline::install(
        &conn,
        Some(Instant::now() + Duration::from_secs(300)),
    )
    .unwrap();
    let (blocks, max_id): (i64, i64) = conn
        .query_row("SELECT count(*), max(id) FROM ocr_results", [], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .unwrap();
    let has_statistics: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='sqlite_stat1')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let mut reports = Vec::new();
    for distribution in ["tail", "spread"] {
        let ids = if distribution == "tail" {
            conn.prepare("SELECT id FROM ocr_results ORDER BY id DESC LIMIT 500")
                .unwrap()
                .query_map([], |row| row.get::<_, i64>(0))
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap()
        } else {
            (0..500)
                .map(|i| {
                    conn.query_row(
                        "SELECT id FROM ocr_results WHERE id >= ? ORDER BY id LIMIT 1",
                        [1 + max_id * i / 500],
                        |row| row.get(0),
                    )
                    .unwrap()
                })
                .collect()
        };
        reports.extend(compare_pairs(
            &conn,
            &ids,
            serde_json::json!({
                "kind": "snapshot", "bytes": before.len(), "blocks": blocks,
            "page_encrypted": page_encrypted,
            "statistics": has_statistics, "distribution": distribution,
            }),
        ));
    }
    drop(_deadline);
    drop(conn);
    let after = std::fs::metadata(path).unwrap();
    assert_eq!(before.len(), after.len());
    assert_eq!(before.modified().unwrap(), after.modified().unwrap());
    write_report("snapshot", reports);
}

#[test]
#[ignore = "synthetic history scaling comparison; creates temporary databases"]
fn compare_synthetic_history_scaling() {
    let mut reports = Vec::new();
    for blocks in [100_000, 1_000_000, 10_000_000] {
        let directory = tempfile::tempdir().unwrap();
        let conn = Connection::open(directory.path().join("synthetic.db")).unwrap();
        // Sparse synthetic payloads keep setup practical. The full production
        // indexes are present; this measures query work, not database GiB.
        conn.execute_batch("PRAGMA cache_size = -16384").unwrap();
        populate(&conn, blocks, 0);
        conn.execute_batch("PRAGMA cache_size = -2048; PRAGMA query_only = ON")
            .unwrap();
        let ids: Vec<i64> = (0..500).map(|i| 1 + (blocks - 1) * i / 500).collect();
        reports.extend(compare_pairs(
            &conn,
            &ids,
            serde_json::json!({
                "kind": "synthetic", "blocks": blocks, "statistics": false,
                "bytes": std::fs::metadata(directory.path().join("synthetic.db")).unwrap().len(),
            }),
        ));
        // Close SQLite before the temporary directory removes the fixture.
        drop(conn);
    }
    write_report("scaling", reports);
}
