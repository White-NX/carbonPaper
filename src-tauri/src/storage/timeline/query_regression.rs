use super::*;
use crate::credential_manager::CredentialManagerState;
use rusqlite::StatementStatus;
use std::sync::Arc;

// Frozen shape from 31b13e1. The outer time predicate lets SQLite reverse the
// join, scanning screenshots and restarting the grouped picks coroutine.
const REGRESSED_SQL: &str = "SELECT s.id, s.image_path, s.created_at,
        CAST(strftime('%s', s.created_at) AS INTEGER), s.window_title, s.process_name,
        s.metadata, s.window_title_enc, s.process_name_enc, s.metadata_enc,
        s.content_key_encrypted, s.page_icon_id,
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
fn short_header_ranges_do_not_scan_unrelated_history_on_fresh_or_analyzed_databases() {
    let (_temp, _storage, conn) = fixture(20000);
    for analyzed in [false, true] {
        if analyzed {
            conn.execute_batch("ANALYZE").unwrap();
        }
        let mut timings = Timings::default();
        let rows = read_rows_with_details(
            &conn,
            (1767225600_i64 + 19905 * 60) as f64,
            (1767225600_i64 + 20000 * 60) as f64,
            500,
            false,
            &|| Ok(()),
            &mut timings,
        )
        .unwrap();
        assert_eq!(rows.len(), 96);
        assert_eq!(rows.first().unwrap().id, 19905);
        assert_eq!(rows.last().unwrap().id, 20000);
        assert!(
            timings.sql_vm_steps < 10000,
            "96 rows amid 20000 used {} steps",
            timings.sql_vm_steps
        );
        let plan = conn
            .prepare(&format!(
                "EXPLAIN QUERY PLAN {}",
                timeline_rows_sql_with_details(None, false)
            ))
            .unwrap()
            .query_map(
                params!["2026-01-14 19:45:00", "2026-01-14 21:20:00", 500],
                |row| row.get::<_, String>(3),
            )
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
            .join("; ");
        assert!(plan.contains("idx_screenshots_deleted_created_at (is_deleted=? AND created_at>? AND created_at<?)"), "{plan}");
        assert!(!plan.contains("idx_screenshots_live_image_path"), "{plan}");
        assert!(
            !plan.contains("page_icons"),
            "headers do not join icon ciphertext: {plan}"
        );
    }
}

#[test]
fn detail_id_batches_use_primary_key_seeks_before_and_after_analyze() {
    let (_temp, _storage, conn) = fixture(20000);
    conn.execute("UPDATE screenshots SET is_deleted = 1 WHERE id = 19999", [])
        .unwrap();
    for analyzed in [false, true] {
        if analyzed {
            conn.execute_batch("ANALYZE").unwrap();
        }
        let mut timings = Timings::default();
        let rows = read_rows_by_ids(
            &conn,
            &[1, 10000, 19999, 20000, 1, -1, 29999],
            &|| Ok(()),
            &mut timings,
        )
        .unwrap();
        assert_eq!(
            rows.iter().map(|row| row.id).collect::<Vec<_>>(),
            [1, 10000, 20000]
        );
        assert!(
            timings.sql_vm_steps < 500,
            "an ID batch scanned history: {} steps",
            timings.sql_vm_steps
        );
    }
}

#[test]
fn nearest_snapshot_seeks_stay_bounded_and_skip_deleted_rows() {
    let (_temp, _storage, conn) = fixture(20000);
    conn.execute("UPDATE screenshots SET is_deleted = 1 WHERE id = 19999", [])
        .unwrap();
    for analyzed in [false, true] {
        if analyzed {
            conn.execute_batch("ANALYZE").unwrap();
        }
        for (seconds, expected) in [
            (1767225600.0, 1),
            (1767225600.0 + 120.0, 2),
            (1767225600.0 + 150.0, 2),
            (1767225600.0 + 150.001, 3),
            (1767225600.0 + 19999.0 * 60.0, 19998),
            (1767225600.0 + 19999.0 * 60.0 + 0.001, 20000),
            (1767225600.0 + 30000.0 * 60.0, 20000),
        ] {
            let mut timings = Timings::default();
            let rows =
                read_nearest_rows(&conn, seconds * 1000.0, &|| Ok(()), &mut timings).unwrap();
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].id, expected);
            assert!(
                timings.sql_vm_steps < 250,
                "nearest selection scanned history: {} steps",
                timings.sql_vm_steps
            );
        }
        for sql in [NEAREST_BEFORE_SQL, NEAREST_AFTER_SQL] {
            let plan = conn
                .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
                .unwrap()
                .query_map(["2026-01-14 21:19:00"], |row| row.get::<_, String>(3))
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap()
                .join("; ");
            assert!(
                plan.contains("COVERING INDEX idx_screenshots_deleted_created_at"),
                "{plan}"
            );
            assert!(!plan.contains("TEMP B-TREE"), "{plan}");
        }
    }
}

#[test]
fn nearest_snapshot_handles_empty_history_invalid_times_and_cancellation() {
    let (_temp, _storage, conn) = fixture(1);
    conn.execute("DELETE FROM screenshots", []).unwrap();
    assert!(
        read_nearest_rows(&conn, 1767225600000.0, &|| Ok(()), &mut Timings::default())
            .unwrap()
            .is_empty()
    );
    for time in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, f64::MAX] {
        assert!(read_nearest_rows(&conn, time, &|| Ok(()), &mut Timings::default()).is_err());
    }
    let mut timings = Timings::default();
    let result = read_nearest_rows(
        &conn,
        1767225600000.0,
        &|| Err(CANCELLED.into()),
        &mut timings,
    );
    assert_eq!(result.err().as_deref(), Some(CANCELLED));
    assert_eq!(timings.sql_vm_steps, 0);
}

#[test]
#[ignore = "synthetic encrypted preview file benchmark; excludes CNG, DB, IPC and browser decode"]
fn compare_timeline_hover_preview_images() {
    let (temp, storage, _conn) = fixture(1);
    let pixels = image::RgbImage::from_fn(1920, 1080, |x, y| {
        let grain = (x.wrapping_mul(73856093) ^ y.wrapping_mul(19349663)) as u8;
        if x < 480 && y > 80 {
            image::Rgb([grain, grain.wrapping_add(31), grain.wrapping_add(73)])
        } else if y % 30 < 2 && x > 540 {
            image::Rgb([180, 180, 190])
        } else {
            let shade = 24 + ((x / 120 + y / 120) % 5) as u8;
            image::Rgb([shade, shade, shade + 3])
        }
    });
    let mut encoded = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(pixels)
        .write_to(&mut encoded, image::ImageFormat::Png)
        .unwrap();
    let png = encoded.into_inner();
    let key = [0x42; 32];
    let original = temp.path().join("preview.png.enc");
    std::fs::write(
        &original,
        crate::credential_manager::encrypt_with_master_key(&key, &png).unwrap(),
    )
    .unwrap();
    let thumbnail = StorageState::thumbnail_path_for(&original);
    let cold_started = Instant::now();
    let (decoded, _) =
        crate::storage::image_io::read_encrypted_image_bytes(original.to_str().unwrap(), &key)
            .unwrap();
    storage
        .generate_thumbnail_from_data(&decoded, &original, &key)
        .unwrap();
    let cold_thumbnail = cold_started.elapsed();
    let measure = |path: &std::path::Path| {
        let mut elapsed = Vec::new();
        for _ in 0..40 {
            let started = Instant::now();
            let (data, _) = crate::storage::image_io::read_encrypted_image_as_base64(
                path.to_str().unwrap(),
                &key,
            )
            .unwrap();
            std::hint::black_box(data);
            elapsed.push(started.elapsed());
        }
        elapsed.sort();
        elapsed[elapsed.len() / 2]
    };
    let original_median = measure(&original);
    let thumbnail_median = measure(&thumbnail);
    let (jpeg, _) =
        crate::storage::image_io::read_encrypted_image_bytes(thumbnail.to_str().unwrap(), &key)
            .unwrap();
    if let Ok(directory) = std::env::var("CARBONPAPER_HOVER_BENCH_OUTPUT") {
        let directory = std::path::PathBuf::from(directory);
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join("benchmark-original.png"), &png).unwrap();
        std::fs::write(directory.join("benchmark-thumbnail.jpg"), &jpeg).unwrap();
    }
    eprintln!("synthetic_preview: pixels=1920x1080 original_bytes={} thumbnail_bytes={} warm_original_median={original_median:?} warm_thumbnail_median={thumbnail_median:?} cold_thumbnail_generation={cold_thumbnail:?} samples=40 excludes=CNG,DB,IPC,browser", png.len(), jpeg.len());
}

#[test]
#[ignore = "warm synthetic SQLCipher timeline benchmark; no CNG, image I/O or frontend"]
fn compare_short_timeline_header_queries() {
    let temp = tempfile::tempdir().unwrap();
    let credentials = Arc::new(CredentialManagerState::new(temp.path().into()));
    let storage = StorageState::new(temp.path().into(), credentials);
    let conn = Connection::open(temp.path().join("timeline-benchmark.db")).unwrap();
    crate::storage::connection::configure_sqlcipher_connection(&conn, &[0x42; 32]).unwrap();
    storage.init_tables(&conn).unwrap();
    conn.execute(
        "WITH RECURSIVE ids(id) AS (SELECT 1 UNION ALL SELECT id+1 FROM ids WHERE id < 20000)
         INSERT INTO screenshots(id,image_path,image_hash,created_at,metadata_enc,content_key_encrypted)
         SELECT id, 'image.enc', CAST(id AS TEXT), datetime(1767225600 + id * 60, 'unixepoch'), zeroblob(2048), zeroblob(256) FROM ids", []
    ).unwrap();
    conn.execute_batch("PRAGMA cache_size = -2048").unwrap();
    let full = timeline_rows_sql(None);
    let before = full.replace(" INDEXED BY idx_screenshots_deleted_created_at", "");
    let headers = timeline_rows_sql_with_details(None, false);
    eprintln!("synthetic=true warm_connection=true historical_rows=20000 returned_rows=96 cache_kib=2048 samples=9");
    for (label, sql) in [
        ("before_details", before),
        ("indexed_details", full),
        ("headers", headers),
    ] {
        let mut statement = conn.prepare(&sql).unwrap();
        let mut elapsed = Duration::ZERO;
        let mut steps = 0;
        for sample in 0..11 {
            statement.reset_status(StatementStatus::VmStep);
            let started = Instant::now();
            let rows = statement
                .query_map(
                    params!["2026-01-14 19:45:00", "2026-01-14 21:20:00", 500],
                    RawTimelineRow::from_row,
                )
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap();
            let query = started.elapsed();
            assert_eq!(rows.len(), 96);
            if sample >= 2 {
                elapsed += query;
                steps += statement.get_status(StatementStatus::VmStep);
            }
        }
        eprintln!(
            "{label}: mean_sql={:?} mean_vm_steps={}",
            elapsed / 9,
            steps / 9
        );
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
