use super::*;
use crate::credential_manager::CredentialManagerState;
use rusqlite::{params, StatementStatus};
use std::sync::Arc;

fn fixture() -> (tempfile::TempDir, StorageState, Connection) {
    let directory = tempfile::tempdir().unwrap();
    let credentials = Arc::new(CredentialManagerState::new(directory.path().to_path_buf()));
    let storage = StorageState::new(directory.path().to_path_buf(), credentials);
    let conn = Connection::open_in_memory().unwrap();
    storage.init_tables(&conn).unwrap();
    (directory, storage, conn)
}

fn insert_history(conn: &Connection, count: i64) {
    conn.execute(
        "WITH RECURSIVE ids(id) AS (
            SELECT 1 UNION ALL SELECT id + 1 FROM ids WHERE id < ?1
         )
         INSERT INTO screenshots
            (id, image_path, image_hash, status, content_key_encrypted, metadata_enc)
         SELECT id, 'screenshots/' || id || '.png.enc', 'hash-' || id,
                'committed', zeroblob(256), zeroblob(512) FROM ids",
        [count],
    )
    .unwrap();
}

#[test]
fn lookup_reads_opaque_image_keys_without_hydrating_metadata() {
    let (_directory, _storage, conn) = fixture();
    let path = "screenshots/截图's.png.enc";
    let key = vec![1u8, 2, 3, 4];
    conn.execute(
        "INSERT INTO screenshots
            (id, image_path, image_hash, content_key_encrypted, width, metadata_enc)
         VALUES (1, ?1, 'hash-one', ?2, 'invalid-width', X'01')",
        params![path, key],
    )
    .unwrap();
    // A deleted record may retain the same path. Only the live record's key is usable.
    conn.execute(
        "INSERT INTO screenshots
            (id, image_path, image_hash, content_key_encrypted, is_deleted)
         VALUES (2, ?1, 'hash-deleted', X'FF', 1)",
        [path],
    )
    .unwrap();
    for lookup in [
        ImageLookup::Id(1),
        ImageLookup::from_path(path),
        ImageLookup::from_path("memory://hash-one"),
    ] {
        let location = lookup.query(&conn).unwrap();
        assert_eq!(location.path, path);
        assert_eq!(location.key_enc.as_ref(), Some(&key));
    }
    assert!(ImageLookup::Id(2).query(&conn).is_err());
    assert!(ImageLookup::from_path("memory://hash-deleted")
        .query(&conn)
        .is_err());
    assert!(ImageLookup::from_path("' OR 1=1 --").query(&conn).is_err());
}

#[test]
fn batch_locations_use_primary_keys_skip_deleted_rows_and_deduplicate() {
    let (_directory, _storage, conn) = fixture();
    insert_history(&conn, 20000);
    conn.execute_batch(
        "UPDATE screenshots SET is_deleted = 1 WHERE id = 19999;
         UPDATE screenshots SET width = 'invalid-width', metadata_enc = X'01' WHERE id = 20000;",
    )
    .unwrap();
    for analyzed in [false, true] {
        if analyzed {
            conn.execute_batch("ANALYZE").unwrap();
        }
        let steps = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = Arc::clone(&steps);
        conn.progress_handler(
            1,
            Some(move || {
                counted.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                false
            }),
        )
        .unwrap();
        let locations = image_locations_by_ids(&conn, &[20000, 1, 1, 19999, 20001, -1]).unwrap();
        conn.progress_handler(0, None::<fn() -> bool>).unwrap();
        assert_eq!(locations.len(), 2);
        assert_eq!(locations[&20000].path, "screenshots/20000.png.enc");
        assert_eq!(locations[&20000].key_enc, Some(vec![0; 256]));
        let executed = steps.load(std::sync::atomic::Ordering::Relaxed);
        assert!(
            executed < 300,
            "a small ID batch scanned history: {executed} steps"
        );
    }
    let locations = image_locations_by_ids(&conn, &(1..=1001).collect::<Vec<_>>()).unwrap();
    assert_eq!(
        locations.len(),
        1001,
        "all SQL parameter chunks must be read"
    );
    assert!(image_locations_by_ids(&conn, &[]).unwrap().is_empty());
}

#[test]
fn full_images_and_batch_locations_do_not_wait_for_the_primary_mutex() {
    for policy in [
        super::super::mode::DatabaseModePolicy::Wal,
        super::super::mode::DatabaseModePolicy::Delete,
    ] {
        let directory = tempfile::tempdir().unwrap();
        let credentials = Arc::new(CredentialManagerState::new(directory.path().into()));
        crate::credential_manager::save_public_key_to_file(&credentials, b"image-read-test-key")
            .unwrap();
        let storage = Arc::new(StorageState::new_with_mode_policy(
            directory.path().into(),
            credentials,
            policy,
        ));
        storage.initialize().unwrap();
        {
            let guard = storage.get_connection_named("seed_image_read").unwrap();
            guard.as_ref().unwrap().execute(
                "INSERT INTO screenshots(id,image_path,image_hash) VALUES (1, 'image.enc', 'one')", []
            ).unwrap();
        }
        let primary = storage.get_connection_named("held_image_metadata").unwrap();
        let reader_storage = Arc::clone(&storage);
        let (finished_tx, finished_rx) = std::sync::mpsc::channel();
        let reader = std::thread::spawn(move || {
            // No row key: reach the unwrap boundary without opening a native
            // CNG key. This still exercises real independent SQLCipher reads.
            let image = reader_storage.read_image_by_id(1);
            let batch = reader_storage.batch_read_thumbnails_by_ids(&[1, 1, 99], false);
            finished_tx.send((image, batch)).unwrap();
        });
        let finished = finished_rx.recv_timeout(Duration::from_secs(5));
        drop(primary);
        reader.join().unwrap();
        let (image, batch) =
            finished.expect("image lookups must finish before the primary mutex is released");
        assert_eq!(image.unwrap_err(), "Failed to unwrap image row key");
        assert_eq!(batch.len(), 2);
        assert_eq!(
            batch["1"].as_ref().unwrap_err(),
            "Failed to unwrap image row key"
        );
        assert_eq!(batch["99"].as_ref().unwrap_err(), "Screenshot not found");
        assert!(storage
            .try_database_maintenance("after_image_read")
            .is_some());
    }
}

#[test]
fn lookup_reports_missing_rows_and_database_errors_and_keeps_auth_required() {
    let (_directory, storage, conn) = fixture();
    conn.execute(
        "INSERT INTO screenshots (id, image_path, image_hash)
         VALUES (1, 'missing-key.png.enc', 'missing-key')",
        [],
    )
    .unwrap();
    assert!(ImageLookup::Id(1).query(&conn).unwrap().key_enc.is_none());
    assert_eq!(
        ImageLookup::Id(99).query(&conn).unwrap_err(),
        "Image not found"
    );
    assert_eq!(
        ImageLookup::from_path("memory://missing")
            .query(&conn)
            .unwrap_err(),
        "No screenshot found for hash: missing"
    );
    *storage.db.lock().unwrap() = Some(conn);
    assert_eq!(
        storage.read_image_by_id(1).unwrap_err(),
        "Failed to unwrap image row key"
    );
    assert_eq!(storage.read_image_by_id(99).unwrap_err(), "Image not found");
    assert!(matches!(
        storage.read_image_bytes_silent("missing-key.png.enc"),
        Err(BackgroundReadError::AuthRequired)
    ));
    let guard = storage.db.lock().unwrap();
    let conn = guard.as_ref().unwrap();
    conn.execute_batch("DROP TABLE screenshots").unwrap();
    let error = ImageLookup::Id(1).query(conn).unwrap_err();
    assert!(error.contains("image key"), "{error}");
    assert!(error.contains("no such table"), "{error}");
}

#[test]
fn cached_only_thumbnail_misses_skip_cng_and_original_generation() {
    let (directory, storage, _conn) = fixture();
    let original = directory.path().join("one.png.enc");
    std::fs::write(&original, b"an original that must not be decoded").unwrap();
    let thumb_path = StorageState::thumbnail_path_for(&original);
    let mut timings = RowKeyDecryptTimings::default();
    assert_eq!(
        storage
            .read_thumbnail_at(&original, None, &mut timings, true)
            .unwrap_err(),
        "Thumbnail not cached"
    );
    assert_eq!(timings.calls, 0);
    assert!(!thumb_path.exists());
    assert_eq!(
        std::fs::read(&original).unwrap(),
        b"an original that must not be decoded"
    );
    // Ordinary reads still reach the key boundary and retain lazy generation.
    assert_eq!(
        storage
            .read_thumbnail_at(&original, None, &mut timings, false)
            .unwrap_err(),
        "Failed to unwrap image row key"
    );
    std::fs::create_dir_all(thumb_path.parent().unwrap()).unwrap();
    std::fs::write(&thumb_path, b"encrypted thumbnail").unwrap();
    // Even a disk hit must unwrap the screenshot key before reading its contents.
    assert_eq!(
        storage
            .read_thumbnail_at(&original, None, &mut timings, true)
            .unwrap_err(),
        "Failed to unwrap image row key"
    );
    assert_eq!(std::fs::read(&thumb_path).unwrap(), b"encrypted thumbnail");
}

#[test]
fn cached_only_thumbnail_reads_preserve_encryption_and_skip_missing_or_corrupt_cache_repair() {
    let (directory, storage, _conn) = fixture();
    let original = directory.path().join("one.png");
    image::RgbImage::new(16, 9).save(&original).unwrap();
    let thumb_path = StorageState::thumbnail_path_for(&original);
    let key = [7; 32];
    assert_eq!(
        storage
            .read_thumbnail_with_key(&original, &thumb_path, &key, true)
            .unwrap_err(),
        "Thumbnail not cached"
    );
    assert!(!thumb_path.exists());
    std::fs::create_dir_all(thumb_path.parent().unwrap()).unwrap();
    let encrypted = encrypt_with_master_key(&key, b"cached JPEG payload").unwrap();
    std::fs::write(&thumb_path, &encrypted).unwrap();
    let (encoded, mime) = storage
        .read_thumbnail_with_key(&original, &thumb_path, &key, true)
        .unwrap();
    assert_eq!(mime, "image/jpeg");
    assert_eq!(
        base64::Engine::decode(&base64::engine::general_purpose::STANDARD, encoded).unwrap(),
        b"cached JPEG payload"
    );
    std::fs::write(&thumb_path, b"corrupt cached ciphertext").unwrap();
    assert!(storage
        .read_thumbnail_with_key(&original, &thumb_path, &key, true)
        .is_err());
    assert_eq!(
        std::fs::read(&thumb_path).unwrap(),
        b"corrupt cached ciphertext"
    );
    // Ordinary card reads continue to repair the cache and generate JPEGs lazily.
    let (encoded, _) = storage
        .read_thumbnail_with_key(&original, &thumb_path, &key, false)
        .unwrap();
    let jpeg = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, encoded).unwrap();
    assert!(image::load_from_memory(&jpeg).is_ok());
    assert!(storage.try_read_cached_thumbnail(&thumb_path, &key).is_ok());
}

#[test]
fn full_image_path_resolution_releases_db_mutex_and_retains_maintenance_permit() {
    let (_directory, storage, conn) = fixture();
    conn.execute(
        "INSERT INTO screenshots (id, image_path, image_hash, content_key_encrypted)
         VALUES (1, 'screenshots/one.png.enc', 'one', X'0102')",
        [],
    )
    .unwrap();
    let (query_started_tx, query_started_rx) = std::sync::mpsc::channel();
    let mut notified = false;
    conn.progress_handler(
        1,
        Some(move || {
            if !notified {
                query_started_tx.send(()).unwrap();
                notified = true;
            }
            false
        }),
    )
    .unwrap();
    *storage.db.lock().unwrap() = Some(conn);
    let directory_guard = storage.data_dir.lock().unwrap();
    std::thread::scope(|scope| {
        let reader = scope.spawn(|| storage.read_image_by_id(1));
        let started = query_started_rx.recv_timeout(Duration::from_secs(5));
        let (acquired_tx, acquired_rx) = std::sync::mpsc::channel();
        let competing_storage = &storage;
        let competing_query = scope.spawn(move || {
            let _guard = competing_storage
                .get_connection_named("test_image_mutex_release")
                .unwrap();
            acquired_tx.send(()).unwrap();
        });
        let acquired = acquired_rx.recv_timeout(Duration::from_secs(5));
        let maintenance_blocked = storage
            .try_database_maintenance("test_image_location")
            .is_none();
        // Release the reader before asserting, including on a timeout, so a
        // failed regression never leaves scoped threads blocked during cleanup.
        drop(directory_guard);
        competing_query.join().unwrap();
        assert_eq!(
            reader.join().unwrap().unwrap_err(),
            "Failed to unwrap image row key"
        );
        assert!(started.is_ok(), "image query did not start: {started:?}");
        assert!(
            acquired.is_ok(),
            "path resolution retained the DB mutex: {acquired:?}"
        );
        assert!(
            maintenance_blocked,
            "the file location lost its maintenance permit"
        );
    });
}

fn assert_bounded_lookup(conn: &Connection, lookup: ImageLookup<'_>, found: bool) {
    let plan = conn
        .prepare(&format!("EXPLAIN QUERY PLAN {}", lookup.sql()))
        .unwrap()
        .query_map([lookup.parameter()], |row| row.get::<_, String>(3))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
        .join("; ");
    match lookup {
        ImageLookup::Id(_) => assert!(plan.contains("USING INTEGER PRIMARY KEY"), "{plan}"),
        ImageLookup::Path(_) => {
            assert!(plan.contains("idx_screenshots_live_image_path"), "{plan}")
        }
        ImageLookup::Hash(_) => assert!(plan.contains("image_hash=?"), "{plan}"),
    }
    let mut statement = conn.prepare(lookup.sql()).unwrap();
    let result = statement
        .query_row([lookup.parameter()], |_| Ok(()))
        .optional()
        .unwrap();
    assert_eq!(result.is_some(), found);
    assert_eq!(statement.get_status(StatementStatus::FullscanStep), 0);
    let steps = statement.get_status(StatementStatus::VmStep);
    assert!(steps < 80, "image lookup used {steps} VM steps: {plan}");
}

#[test]
fn image_lookups_stay_bounded_on_fresh_and_upgraded_databases() {
    let (_directory, storage, conn) = fixture();
    insert_history(&conn, 10000);
    // Re-running initialization must install the index on an existing database
    // without changing records, and must remain safe on subsequent startups.
    for upgraded in [false, true] {
        if upgraded {
            conn.execute_batch("DROP INDEX idx_screenshots_live_image_path")
                .unwrap();
            storage.init_tables(&conn).unwrap();
            storage.init_tables(&conn).unwrap();
        }
        for analyzed in [false, true] {
            if analyzed {
                conn.execute_batch("ANALYZE").unwrap();
            }
            for (lookup, found) in [
                (ImageLookup::Id(10000), true),
                (ImageLookup::Id(10001), false),
                (ImageLookup::Path("screenshots/1.png.enc"), true),
                (ImageLookup::Path("screenshots/10000.png.enc"), true),
                (ImageLookup::Path("screenshots/missing.png.enc"), false),
                (ImageLookup::Hash("hash-10000"), true),
                (ImageLookup::Hash("hash-missing"), false),
            ] {
                assert_bounded_lookup(&conn, lookup, found);
            }
        }
    }
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM screenshots", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        10000
    );
    let location = ImageLookup::Id(10000).query(&conn).unwrap();
    assert_eq!(location.path, "screenshots/10000.png.enc");
    assert_eq!(location.key_enc.unwrap(), vec![0; 256]);
}

#[test]
fn path_index_tracks_pending_renames_and_soft_deletion() {
    let (_directory, _storage, conn) = fixture();
    conn.execute_batch(
        "INSERT INTO screenshots
            (id, image_path, image_hash, status, content_key_encrypted)
         VALUES (1, 'screenshots/one.png.enc.pending', 'one', 'pending', X'0102');
         UPDATE screenshots SET image_path = 'screenshots/one.png.enc', status = 'committed'
            WHERE id = 1;",
    )
    .unwrap();
    assert!(ImageLookup::Path("screenshots/one.png.enc.pending")
        .query(&conn)
        .is_err());
    assert_eq!(
        ImageLookup::Path("screenshots/one.png.enc")
            .query(&conn)
            .unwrap()
            .key_enc,
        Some(vec![1, 2])
    );
    conn.execute("UPDATE screenshots SET is_deleted = 1 WHERE id = 1", [])
        .unwrap();
    for lookup in [
        ImageLookup::Id(1),
        ImageLookup::Path("screenshots/one.png.enc"),
    ] {
        assert!(lookup.query(&conn).is_err());
    }
    conn.execute("UPDATE screenshots SET is_deleted = 0 WHERE id = 1", [])
        .unwrap();
    assert!(ImageLookup::Path("screenshots/one.png.enc")
        .query(&conn)
        .is_ok());
}

#[test]
fn measured_image_read_preserves_encryption_and_pending_file_compatibility() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("one.png.enc.pending");
    let key = [7; 32];
    let plaintext = b"encrypted image fixture";
    let mut encrypted = encrypt_with_master_key(&key, plaintext).unwrap();
    std::fs::write(&path, &encrypted).unwrap();
    let mut timings = ImageFileReadTimings::default();
    let (data, mime) =
        read_encrypted_image_bytes_measured(path.to_str().unwrap(), &key, &mut timings).unwrap();
    assert_eq!(data, plaintext);
    assert_eq!(mime, "image/png");
    let (encoded, _) = read_encrypted_image_as_base64(path.to_str().unwrap(), &key).unwrap();
    assert_eq!(
        base64::Engine::decode(&base64::engine::general_purpose::STANDARD, encoded).unwrap(),
        plaintext
    );
    *encrypted.last_mut().unwrap() ^= 1;
    std::fs::write(&path, encrypted).unwrap();
    assert!(read_encrypted_image_bytes(path.to_str().unwrap(), &key).is_err());
}

#[test]
#[ignore = "offline SQLCipher query benchmark; synthetic rows, no CNG or image file reads"]
fn compare_image_lookup_queries() {
    let (directory, storage, memory) = fixture();
    drop(memory);
    let conn = Connection::open(directory.path().join("image-lookup.db")).unwrap();
    crate::storage::connection::configure_sqlcipher_connection(&conn, &[0x31; 32]).unwrap();
    storage.init_tables(&conn).unwrap();
    insert_history(&conn, 20000);
    conn.execute_batch("PRAGMA cache_size = -2048; DROP INDEX idx_screenshots_live_image_path")
        .unwrap();
    eprintln!(
        "engine={} cipher={} rows=20000 cache_kib=2048 profile=debug synthetic=true",
        rusqlite::version(),
        conn.query_row("PRAGMA cipher_version", [], |row| row.get::<_, String>(0))
            .unwrap()
    );
    for (label, lookup) in [
        (
            "before_path",
            ImageLookup::Path("screenshots/20000.png.enc"),
        ),
        (
            "indexed_path",
            ImageLookup::Path("screenshots/20000.png.enc"),
        ),
        ("direct_id", ImageLookup::Id(20000)),
    ] {
        if label == "indexed_path" {
            storage.init_tables(&conn).unwrap();
        }
        let mut statement = conn.prepare(lookup.sql()).unwrap();
        // One warm-up, then three completed queries against the same connection.
        statement
            .query_row([lookup.parameter()], |_| Ok(()))
            .unwrap();
        statement.reset_status(StatementStatus::VmStep);
        let started = Instant::now();
        for _ in 0..3 {
            statement
                .query_row([lookup.parameter()], |row| {
                    Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, String>(1)?))
                })
                .unwrap();
        }
        eprintln!(
            "{label}: mean_query={:?} mean_vm_steps={}",
            started.elapsed() / 3,
            statement.get_status(StatementStatus::VmStep) / 3
        );
    }
}

#[test]
#[ignore = "warm synthetic SQLCipher thumbnail batch benchmark; no CNG or image files"]
fn compare_thumbnail_batch_lookup_queries() {
    let (directory, storage, memory) = fixture();
    drop(memory);
    let conn = Connection::open(directory.path().join("thumbnail-batch-benchmark.db")).unwrap();
    crate::storage::connection::configure_sqlcipher_connection(&conn, &[0x43; 32]).unwrap();
    storage.init_tables(&conn).unwrap();
    insert_history(&conn, 20000);
    conn.execute_batch("PRAGMA cache_size = -2048").unwrap();
    let before = "SELECT id, content_key_encrypted, image_path FROM screenshots
        WHERE is_deleted = 0 AND id IN (?1, ?2, ?3, ?4, ?5, ?6)";
    let after = before.replace("FROM screenshots", "FROM screenshots NOT INDEXED");
    let ids = [19995, 19996, 19997, 19998, 19999, 20000];
    eprintln!("synthetic=true warm_connection=true historical_rows=20000 batch_ids=6 cache_kib=2048 samples=9");
    for (label, sql) in [
        ("before_id_batch", before),
        ("primary_key_batch", after.as_str()),
    ] {
        let mut statement = conn.prepare(sql).unwrap();
        let mut elapsed = Duration::ZERO;
        let mut steps = 0;
        for sample in 0..11 {
            statement.reset_status(StatementStatus::VmStep);
            let started = Instant::now();
            let rows = statement
                .query_map(rusqlite::params_from_iter(&ids), |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, Vec<u8>>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                })
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap();
            let query = started.elapsed();
            assert_eq!(rows.len(), 6);
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
#[ignore = "warm synthetic AES/Base64 benchmark; no CNG, file I/O, IPC or browser decoding"]
fn compare_image_crypto_hot_path() {
    use aes_gcm::{aead::Aead, aead::KeyInit, Aes256Gcm, Nonce};
    use base64::Engine;
    use std::hint::black_box;

    // The former desktop implementation, instantiated in the unoptimized
    // application crate. Keep it here only as a reproducible benchmark baseline.
    #[inline(never)]
    fn legacy_decrypt(key: &[u8], encrypted: &[u8]) -> Vec<u8> {
        Aes256Gcm::new_from_slice(key)
            .unwrap()
            .decrypt(Nonce::from_slice(&encrypted[..12]), &encrypted[12..])
            .unwrap()
    }

    let key = [7; 32];
    eprintln!(
        "synthetic=true warm_memory=true samples=9 debug_assertions={}",
        cfg!(debug_assertions)
    );
    for size in [16 * 1024, 256 * 1024, 1024 * 1024, 4 * 1024 * 1024] {
        let plaintext = vec![0x5a; size];
        let encrypted = encrypt_with_master_key(&key, &plaintext).unwrap();
        for _ in 0..2 {
            black_box(legacy_decrypt(&key, &encrypted));
            black_box(decrypt_with_master_key(&key, &encrypted).unwrap());
        }
        let mut before = Duration::ZERO;
        let mut after = Duration::ZERO;
        let mut base64 = Duration::ZERO;
        for _ in 0..9 {
            let started = Instant::now();
            let old = legacy_decrypt(black_box(&key), black_box(&encrypted));
            before += started.elapsed();
            let started = Instant::now();
            let current = decrypt_with_master_key(black_box(&key), black_box(&encrypted)).unwrap();
            after += started.elapsed();
            assert_eq!(old, plaintext);
            assert_eq!(current, plaintext);
            let started = Instant::now();
            black_box(base64::engine::general_purpose::STANDARD.encode(black_box(&current)));
            base64 += started.elapsed();
        }
        eprintln!(
            "bytes={size} legacy_aes_mean={:?} storage_aes_mean={:?} base64_mean={:?}",
            before / 9,
            after / 9,
            base64 / 9
        );
    }
}
