use super::*;

fn legacy(conn: &Connection) {
    conn.execute_batch(
        "PRAGMA foreign_keys=ON;
        CREATE TABLE app_metadata(key TEXT PRIMARY KEY,value TEXT);
        CREATE TABLE blind_bitmap_index(token_hash TEXT PRIMARY KEY,postings_blob BLOB NOT NULL);
        CREATE TABLE marker(id INTEGER PRIMARY KEY,value TEXT);
        INSERT INTO marker VALUES(1,'before');",
    )
    .unwrap();
}

fn fixture() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    legacy(&conn);
    ensure_schema(&conn).unwrap();
    conn
}

fn large() -> RoaringBitmap {
    (0..70_000).map(|n| n * 37).collect()
}

fn write(conn: &Connection, hash: &str, bitmap: &RoaringBitmap) {
    let tx = conn.unchecked_transaction().unwrap();
    BlindIndex::open(&tx)
        .unwrap()
        .replace(hash, bitmap, &mut MutationStats::default())
        .unwrap();
    tx.commit().unwrap();
}

fn assert_posting(conn: &Connection, hash: &str, expected: &RoaringBitmap, chunked: bool) {
    let store = BlindIndex::open(conn).unwrap();
    let result = store.read(hash, chunked).unwrap();
    if expected.is_empty() {
        assert!(result.is_none());
        assert!(store.probe(&[hash.into()]).unwrap().postings.is_empty());
    } else {
        assert!(result.unwrap().bitmap == *expected);
        let probes = store.probe(&[hash.into()]).unwrap().postings;
        assert_eq!(probes.len(), 1);
        assert_eq!(probes[0].bytes, expected.serialized_size());
        assert_eq!(probes[0].chunked, chunked);
        if chunked {
            assert_eq!(probes[0].cardinality, Some(expected.len()));
        }
    }
}

#[test]
fn upgrade_is_idempotent_and_rejects_legacy_access() {
    let conn = Connection::open_in_memory().unwrap();
    legacy(&conn);
    let original = large();
    let encoded = encode(&original, &mut MutationStats::default()).unwrap();
    conn.execute(
        "INSERT INTO blind_bitmap_index VALUES(?1,?2)",
        params!["historical", encoded],
    )
    .unwrap();
    ensure_schema(&conn).unwrap();
    ensure_schema(&conn).unwrap();
    assert_posting(&conn, "historical", &original, false);
    conn.execute_batch("CREATE TABLE IF NOT EXISTS blind_bitmap_index(token_hash TEXT PRIMARY KEY,postings_blob BLOB NOT NULL)").unwrap();
    for sql in [
        "SELECT postings_blob FROM blind_bitmap_index WHERE token_hash='historical'",
        "SELECT token_hash,length(postings_blob) FROM blind_bitmap_index",
        "INSERT OR REPLACE INTO blind_bitmap_index VALUES('historical',X'00')",
        "UPDATE blind_bitmap_index SET postings_blob=X'00'",
        "DELETE FROM blind_bitmap_index",
    ] {
        assert!(
            conn.prepare(sql).is_err(),
            "legacy access must fail closed: {sql}"
        );
    }
    // Ordinary schema evolution and maintenance must still work with the guard view.
    conn.execute_batch("ALTER TABLE marker ADD COLUMN extra TEXT; VACUUM;")
        .unwrap();
    let integrity: String = conn
        .query_row("PRAGMA integrity_check", [], |r| r.get(0))
        .unwrap();
    assert_eq!(integrity, "ok");
    assert_posting(&conn, "historical", &original, false);
}

#[test]
fn future_version_is_rejected_without_schema_mutation() {
    let conn = Connection::open_in_memory().unwrap();
    legacy(&conn);
    conn.execute("INSERT INTO app_metadata VALUES(?1,'99')", [FORMAT_KEY])
        .unwrap();
    assert!(check_supported_format(&conn).is_err());
    assert!(ensure_schema(&conn).is_err());
    assert!(matches!(object(&conn,"blind_bitmap_index").unwrap(),Some((kind,_)) if kind=="table"));
    assert!(object(&conn, "blind_bitmap_inline").unwrap().is_none());
}

#[test]
fn promotion_boundaries_duplicates_and_cross_shard_removal_preserve_sets() {
    let conn = fixture();
    let mut expected = large();
    // Seed a cold large inline posting, just like an upgraded database.
    conn.execute(
        "INSERT INTO blind_bitmap_inline VALUES(?1,?2)",
        params![
            "token",
            encode(&expected, &mut MutationStats::default()).unwrap()
        ],
    )
    .unwrap();
    let additions: RoaringBitmap = [SHARD_MASK, SHARD_MASK + 1, 3 << SHARD_BITS, u32::MAX]
        .into_iter()
        .collect();
    {
        let tx = conn.unchecked_transaction().unwrap();
        let store = BlindIndex::open(&tx).unwrap();
        store
            .add("token", &additions, &mut MutationStats::default())
            .unwrap();
        let changes = tx.total_changes();
        store
            .add("token", &additions, &mut MutationStats::default())
            .unwrap();
        assert_eq!(
            tx.total_changes(),
            changes,
            "duplicate addition must not write"
        );
        tx.commit().unwrap();
    }
    expected |= &additions;
    assert_posting(&conn, "token", &expected, true);
    // A stale inline hint must resolve to the promoted posting.
    assert!(
        BlindIndex::open(&conn)
            .unwrap()
            .read("token", false)
            .unwrap()
            .unwrap()
            .bitmap
            == expected
    );
    let removals: RoaringBitmap = (0..=SHARD_MASK).chain([u32::MAX]).collect();
    {
        let tx = conn.unchecked_transaction().unwrap();
        let store = BlindIndex::open(&tx).unwrap();
        store
            .remove("token", &removals, &mut MutationStats::default())
            .unwrap();
        let changes = tx.total_changes();
        store
            .remove("token", &removals, &mut MutationStats::default())
            .unwrap();
        assert_eq!(
            tx.total_changes(),
            changes,
            "duplicate removal must not write"
        );
        tx.commit().unwrap();
    }
    expected -= &removals;
    assert_posting(&conn, "token", &expected, true);
    write(&conn, "token", &RoaringBitmap::new());
    assert_posting(&conn, "token", &RoaringBitmap::new(), false);
    let recreated: RoaringBitmap = [7, 9].into_iter().collect();
    write(&conn, "token", &recreated);
    assert_posting(&conn, "token", &recreated, false);
    // A stale chunked hint must resolve to a newly recreated inline posting.
    assert!(
        BlindIndex::open(&conn)
            .unwrap()
            .read("token", true)
            .unwrap()
            .unwrap()
            .bitmap
            == recreated
    );
}

#[test]
fn schema_errors_roll_back_and_namespace_conflicts_are_rejected() {
    let conn = Connection::open_in_memory().unwrap();
    legacy(&conn);
    write(&conn, "legacy", &large());
    conn.execute_batch(
        "CREATE TRIGGER blind_inline_excludes_chunks AFTER INSERT ON marker BEGIN SELECT 1; END",
    )
    .unwrap();
    assert!(ensure_schema(&conn).is_err());
    assert!(object(&conn, "blind_bitmap_inline").unwrap().is_none());
    assert!(object(&conn, "blind_bitmap_directory").unwrap().is_none());
    assert_posting(&conn, "legacy", &large(), false);
    conn.execute_batch("DROP TRIGGER blind_inline_excludes_chunks")
        .unwrap();
    ensure_schema(&conn).unwrap();
    write(&conn, "chunked", &large());
    assert!(conn
        .execute(
            "INSERT INTO blind_bitmap_inline VALUES('chunked',?1)",
            [encode(&large(), &mut MutationStats::default()).unwrap()]
        )
        .is_err());
    assert!(conn
        .execute_batch("INSERT INTO blind_bitmap_directory VALUES('legacy',2,1,0,1)")
        .is_err());
    assert_posting(&conn, "legacy", &large(), false);
    assert_posting(&conn, "chunked", &large(), true);
}

#[test]
fn run_container_metadata_matches_canonical_serialization() {
    let conn = fixture();
    write(&conn, "runs", &large());
    let mut runs = RoaringBitmap::new();
    runs.insert_range(0..70_000);
    runs.insert_range((2 << SHARD_BITS)..(2 << SHARD_BITS) + 70_000);
    runs.optimize();
    write(&conn, "runs", &runs);
    assert_posting(&conn, "runs", &runs, true);
    let tx = conn.unchecked_transaction().unwrap();
    let additions: RoaringBitmap = [100_000, (2 << SHARD_BITS) + 100_000].into_iter().collect();
    BlindIndex::open(&tx)
        .unwrap()
        .add("runs", &additions, &mut MutationStats::default())
        .unwrap();
    tx.commit().unwrap();
    runs |= additions;
    assert_posting(&conn, "runs", &runs, true);
}

#[test]
fn corrupt_posting_rolls_back_markers_and_prior_promotion() {
    let conn = fixture();
    let original = large();
    conn.execute(
        "INSERT INTO blind_bitmap_inline VALUES('cold',?1)",
        [encode(&original, &mut MutationStats::default()).unwrap()],
    )
    .unwrap();
    conn.execute_batch("INSERT INTO blind_bitmap_inline VALUES('corrupt',X'0102')")
        .unwrap();
    let result = (|| -> Result<(), String> {
        let tx = conn.unchecked_transaction().unwrap();
        tx.execute("UPDATE marker SET value='after' WHERE id=1", [])
            .unwrap();
        let store = BlindIndex::open(&tx)?;
        let ids: RoaringBitmap = [u32::MAX].into_iter().collect();
        store.add("cold", &ids, &mut MutationStats::default())?;
        store.add("corrupt", &ids, &mut MutationStats::default())?;
        tx.commit().map_err(|e| e.to_string())
    })();
    assert!(result.is_err());
    assert_posting(&conn, "cold", &original, false);
    let marker: String = conn
        .query_row("SELECT value FROM marker WHERE id=1", [], |r| r.get(0))
        .unwrap();
    assert_eq!(marker, "before");
}

#[test]
fn chunk_range_corruption_is_rejected_and_probe_batches_are_bounded() {
    let conn = fixture();
    write(&conn, "chunked", &large());
    conn.execute(
        "UPDATE blind_bitmap_chunks SET postings_blob=?1 WHERE token_hash='chunked' AND shard_id=0",
        [encode(
            &[u32::MAX].into_iter().collect(),
            &mut MutationStats::default(),
        )
        .unwrap()],
    )
    .unwrap();
    assert!(BlindIndex::open(&conn)
        .unwrap()
        .read("chunked", true)
        .is_err());
    write(&conn, "chunked", &RoaringBitmap::new());
    let tx = conn.unchecked_transaction().unwrap();
    let store = BlindIndex::open(&tx).unwrap();
    let keys: Vec<String> = (0..1001).map(|n| format!("synthetic-{n:04}")).collect();
    for hash in &keys {
        store
            .add(
                hash,
                &[1, 2].into_iter().collect(),
                &mut MutationStats::default(),
            )
            .unwrap();
    }
    tx.commit().unwrap();
    let store = BlindIndex::open(&conn).unwrap();
    let probes = store.probe(&keys).unwrap();
    assert_eq!(probes.postings.len(), keys.len());
    assert_eq!(probes.statements, 3);
    let frequencies = store.cardinalities(&keys).unwrap();
    assert_eq!(frequencies.len(), keys.len());
    assert!(frequencies.iter().all(|(_, count)| *count == 2));
    assert_eq!(store.count_after("").unwrap(), 1001);
    assert_eq!(
        store.tokens_after("synthetic-0998", 500).unwrap(),
        vec!["synthetic-0999", "synthetic-1000"]
    );
}

#[test]
fn encrypted_snapshot_reopens_with_equivalent_postings() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("source.db");
    let copy = temp.path().join("copy.db");
    let key = [7u8; 32];
    let original = large();
    {
        let conn = Connection::open(&path).unwrap();
        crate::storage::connection::configure_sqlcipher_connection(&conn, &key).unwrap();
        legacy(&conn);
        ensure_schema(&conn).unwrap();
        write(&conn, "large", &original);
        conn.execute("VACUUM INTO ?1", [copy.to_str().unwrap()])
            .unwrap();
    }
    let restored = Connection::open(copy).unwrap();
    crate::storage::connection::configure_sqlcipher_connection(&restored, &key).unwrap();
    ensure_schema(&restored).unwrap();
    assert_posting(&restored, "large", &original, true);
    assert!(restored
        .prepare("SELECT * FROM blind_bitmap_index")
        .is_err());
}

#[test]
fn crash_recovery_preserves_format_and_posting_transaction_boundaries() {
    for operation in ["schema", "posting"] {
        for committed in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let path = temp.path().join("crash.db");
            {
                let conn = Connection::open(&path).unwrap();
                legacy(&conn);
                conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL")
                    .unwrap();
                if operation == "posting" {
                    ensure_schema(&conn).unwrap();
                    conn.execute(
                        "INSERT INTO blind_bitmap_inline VALUES('cold',?1)",
                        [encode(&large(), &mut MutationStats::default()).unwrap()],
                    )
                    .unwrap();
                }
            }
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--ignored",
                    "--exact",
                    "storage::blind_index::tests::crash_child",
                ])
                .env("CARBONPAPER_BLIND_CRASH_DB", &path)
                .env("CARBONPAPER_BLIND_CRASH_OP", operation)
                .env(
                    "CARBONPAPER_BLIND_CRASH_COMMIT",
                    if committed { "1" } else { "0" },
                )
                .output()
                .unwrap();
            assert_eq!(
                output.status.code(),
                Some(86),
                "child: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            let conn = Connection::open(&path).unwrap();
            if operation == "schema" {
                let kind = object(&conn, "blind_bitmap_index").unwrap().unwrap().0;
                assert_eq!(kind, if committed { "view" } else { "table" });
                ensure_schema(&conn).unwrap();
            } else {
                let mut expected = large();
                if committed {
                    expected.insert(u32::MAX);
                }
                assert_posting(&conn, "cold", &expected, committed);
                let marker: String = conn
                    .query_row("SELECT value FROM marker WHERE id=1", [], |r| r.get(0))
                    .unwrap();
                assert_eq!(marker, if committed { "after" } else { "before" });
            }
            let integrity: String = conn
                .query_row("PRAGMA integrity_check", [], |r| r.get(0))
                .unwrap();
            assert_eq!(integrity, "ok");
        }
    }
}

#[test]
#[ignore = "only invoked by the crash-recovery parent with a synthetic database"]
fn crash_child() {
    let path = std::env::var("CARBONPAPER_BLIND_CRASH_DB").expect("parent-owned fixture");
    let conn = Connection::open(path).unwrap();
    conn.execute_batch("PRAGMA journal_mode=WAL;PRAGMA synchronous=FULL")
        .unwrap();
    let tx = conn.unchecked_transaction().unwrap();
    if std::env::var("CARBONPAPER_BLIND_CRASH_OP").unwrap() == "schema" {
        ensure_schema(&tx).unwrap();
    } else {
        tx.execute("UPDATE marker SET value='after' WHERE id=1", [])
            .unwrap();
        BlindIndex::open(&tx)
            .unwrap()
            .add(
                "cold",
                &[u32::MAX].into_iter().collect(),
                &mut MutationStats::default(),
            )
            .unwrap();
    }
    if std::env::var("CARBONPAPER_BLIND_CRASH_COMMIT").unwrap() == "1" {
        tx.commit().unwrap();
    }
    std::process::exit(86);
}
