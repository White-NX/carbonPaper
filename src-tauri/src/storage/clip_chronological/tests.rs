use super::*;
use crate::storage::{DerivedEmbeddingWrite, DerivedIndexJobSpec};
use rusqlite::params;

fn fixture() -> (tempfile::TempDir, Arc<StorageState>) {
    let (temp, storage) =
        super::super::tests::initialized_read_storage(super::super::mode::DatabaseModePolicy::Wal);
    // Process filtering reads protected metadata under an unlocked UI session.
    storage.credential_state.set_foreground_state(true);
    storage.credential_state.update_auth_time();
    storage
        .db
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .execute_batch("DELETE FROM ocr_results; DELETE FROM screenshots;")
        .unwrap();
    (temp, storage)
}

fn insert(storage: &StorageState, id: i64, time: &str, score: f32, process: &str, category: &str) {
    let hash = format!("chronological-{id}");
    storage
        .db
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .execute(
            "INSERT INTO screenshots(id,image_path,image_hash,created_at,process_name,category)
         VALUES(?1,?2,?3,?4,?5,?6)",
            params![id, format!("{id}.enc"), hash, time, process, category],
        )
        .unwrap();
    let job = DerivedIndexJobSpec {
        index_kind: DerivedIndexKind::ClipImage,
        subject_key: hash,
        model_id: "test-clip".into(),
        model_revision: "revision-1".into(),
        embedding_version: 1,
        source_fingerprint: format!("source-{id}"),
    };
    storage.upsert_derived_index_job(&job).unwrap();
    let lease_token = storage.mark_derived_index_job_processing(&job).unwrap();
    storage
        .commit_derived_embedding(&DerivedEmbeddingWrite {
            job,
            lease_token,
            vector: vec![score, (1.0 - score * score).sqrt()],
        })
        .unwrap();
}

fn request(
    metrics: &VectorQueryMetrics,
    offset: usize,
    limit: usize,
) -> ClipChronologicalRequest<'_> {
    ClipChronologicalRequest {
        query: &[1.0, 0.0],
        offset,
        limit,
        process_names: &[],
        categories: &[],
        start_time: None,
        end_time: None,
        min_similarity: 0.32,
        deadline: Instant::now() + Duration::from_secs(5),
        cancelled: Arc::new(AtomicBool::new(false)),
        metrics,
    }
}

fn ids(hits: &[ClipChronologicalHit]) -> Vec<i64> {
    hits.iter().map(|hit| hit.summary.id).collect()
}

#[test]
fn newest_clip_reads_a_bounded_prefix_and_continues_cached_gallery_pages() {
    let (_temp, storage) = fixture();
    for id in 1..=320 {
        insert(
            &storage,
            id,
            &crate::storage::wire_time::from_unix_seconds(1000 + id),
            if id > 250 { 0.33 } else { 0.95 },
            "browser.exe",
            "work",
        );
    }
    let first_metrics = VectorQueryMetrics::default();
    let first = storage
        .clip_image_chronological(request(&first_metrics, 0, 10))
        .unwrap();
    assert_eq!(ids(&first), (311..=320).rev().collect::<Vec<_>>());
    assert!(first.iter().all(|hit| hit.subject.score < 0.34));
    assert_eq!(first_metrics.snapshot().rows_scanned, CANDIDATE_BATCH_SIZE);

    // Opening the gallery expands the summary's prefix instead of starting
    // another full query. Re-reading a cached prefix needs no vector scan.
    let gallery_metrics = VectorQueryMetrics::default();
    let gallery = storage
        .clip_image_chronological(request(&gallery_metrics, 0, 50))
        .unwrap();
    assert_eq!(ids(&gallery), (271..=320).rev().collect::<Vec<_>>());
    assert_eq!(gallery_metrics.snapshot().rows_scanned, 0);
    let cached_metrics = VectorQueryMetrics::default();
    let cached = storage
        .clip_image_chronological(request(&cached_metrics, 10, 10))
        .unwrap();
    assert_eq!(ids(&cached), (301..=310).rev().collect::<Vec<_>>());
    assert_eq!(cached_metrics.snapshot().rows_scanned, 0);

    let next_metrics = VectorQueryMetrics::default();
    let next = storage
        .clip_image_chronological(request(&next_metrics, 50, 50))
        .unwrap();
    assert_eq!(ids(&next), (221..=270).rev().collect::<Vec<_>>());
    assert_eq!(next_metrics.snapshot().rows_scanned, CANDIDATE_BATCH_SIZE);
}

#[test]
fn chronological_clip_filters_before_paging_and_keeps_unknown_times_at_the_end() {
    let (_temp, storage) = fixture();
    for (id, time, score, process, category) in [
        (1, "1970-01-01 00:01:40", 0.95, "editor.exe", "work"),
        (2, "1970-01-01 00:03:20", 0.33, "editor.exe", "work"),
        (3, "1970-01-01 00:04:10", 0.90, "other.exe", "work"),
        (4, "1970-01-01 00:04:00", 0.90, "editor.exe", "other"),
        (5, "invalid timestamp", 0.80, "editor.exe", "work"),
        (6, "1970-01-01 00:03:50", 0.20, "editor.exe", "work"),
        (7, "1970-01-01 00:03:40", 0.90, "editor.exe", "work"),
        (8, "1970-01-01 00:03:20", 0.36, "editor.exe", "work"),
    ] {
        insert(&storage, id, time, score, process, category);
    }
    storage.db.lock().unwrap().as_ref().unwrap().execute(
        "UPDATE derived_index_jobs SET model_revision='stale' WHERE subject_key='chronological-7'", [],
    ).unwrap();
    let processes = ["editor.exe".into()];
    let categories = ["work".into()];
    let metrics = VectorQueryMetrics::default();
    let mut filtered = request(&metrics, 0, 2);
    filtered.process_names = &processes;
    filtered.categories = &categories;
    filtered.start_time = Some(150.0);
    filtered.end_time = Some(260.0);
    let page = storage.clip_image_chronological(filtered).unwrap();
    assert_eq!(ids(&page), vec![2, 8]);
    assert_eq!(page[0].summary.timestamp, Some(200));
    let mut remaining = request(&metrics, 2, 2);
    remaining.process_names = &processes;
    remaining.categories = &categories;
    remaining.start_time = Some(150.0);
    remaining.end_time = Some(260.0);
    let tail = storage.clip_image_chronological(remaining).unwrap();
    assert_eq!(ids(&tail), vec![5]);
    assert_eq!(tail[0].summary.timestamp, None);
}

#[test]
fn chronological_clip_cache_expires_on_writes_and_releases_maintenance_readers() {
    let (_temp, storage) = fixture();
    for id in 1..=20 {
        insert(
            &storage,
            id,
            &crate::storage::wire_time::from_unix_seconds(100 + id),
            0.8,
            "editor.exe",
            "work",
        );
    }
    let metrics = VectorQueryMetrics::default();
    storage
        .clip_image_chronological(request(&metrics, 0, 10))
        .unwrap();
    storage
        .db
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .execute("UPDATE screenshots SET is_deleted=1 WHERE id=20", [])
        .unwrap();
    let after_write = storage
        .clip_image_chronological(request(&metrics, 0, 10))
        .unwrap();
    assert_eq!(ids(&after_write), (10..=19).rev().collect::<Vec<_>>());
    assert!(storage.clip_chronological_cache.lock().unwrap().is_some());
    let maintenance = storage
        .try_database_maintenance("chronological_clip_test")
        .unwrap();
    assert!(storage.clip_chronological_cache.lock().unwrap().is_none());
    drop(maintenance);
    storage
        .clip_image_chronological(request(&metrics, 0, 10))
        .unwrap();
    assert!(storage.evict_clip_chronological_cache_if_idle(Duration::ZERO));
    let mut cancelled = request(&metrics, 0, 10);
    cancelled.cancelled = Arc::new(AtomicBool::new(true));
    assert_eq!(
        storage.clip_image_chronological(cancelled).err().unwrap(),
        "query_cancelled"
    );
}

#[test]
fn chronological_clip_uses_indexed_cursor_seeks_without_archive_sorts() {
    let (_temp, storage) = fixture();
    let guard = storage.db.lock().unwrap();
    let conn = guard.as_ref().unwrap();
    for analyzed in [false, true] {
        if analyzed {
            conn.execute_batch("ANALYZE").unwrap();
        }
        for (time, inverse_id, same_time) in [
            (i64::MAX, 0, false),
            (100, -10, false),
            (100, -10, true),
            (0, -10, true),
        ] {
            let plan: Vec<String> = conn
                .prepare(&format!(
                    "EXPLAIN QUERY PLAN {}",
                    candidate_sql(0, same_time)
                ))
                .unwrap()
                .query_map(
                    params![
                        "clip_image",
                        time,
                        inverse_id,
                        Option::<f64>::None,
                        Option::<f64>::None,
                        128
                    ],
                    |row| row.get(3),
                )
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap();
            assert!(
                plan.iter().any(|detail| detail
                    .contains("SEARCH s USING INDEX idx_screenshots_clip_chronological")),
                "{plan:?}"
            );
            assert!(
                plan.iter()
                    .all(|detail| !detail.contains("TEMP B-TREE") && !detail.starts_with("SCAN ")),
                "{plan:?}"
            );
        }
    }
}

#[test]
fn chronological_index_upgrades_legacy_unknown_times_without_rejecting_rows() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch("CREATE TABLE screenshots(id INTEGER PRIMARY KEY,created_at TEXT,is_deleted INTEGER DEFAULT 0);
        INSERT INTO screenshots(id,created_at) VALUES(1,'now'),(2,'NOW'),(3,'invalid timestamp'),(4,NULL),(5,'1970-01-01 00:01:40');").unwrap();
    init_index(&conn).unwrap();
    let times: Vec<(i64, i64)> = conn
        .prepare(&format!(
            "SELECT id,{CAPTURE_TIME_SQL} FROM screenshots ORDER BY id"
        ))
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(times, vec![(1, 0), (2, 0), (3, 0), (4, 0), (5, 100)]);
}
