//! Opt-in offline benchmark. Only blind-index hashes and bitmap blobs are read
//! from the supplied database. All writes use disposable encrypted index copies.

use super::postings_io::{
    ordered_token_hashes, PostingsCacheBudget, POSTINGS_CACHE_KIB, UPSERT_POSTING_SQL,
};
use roaring::RoaringBitmap;
use rusqlite::{params, Connection, OpenFlags};
use std::collections::HashMap;
use std::time::{Duration, Instant};

const BENCH_KEY: [u8; 32] = [7; 32];
#[derive(Clone, Copy, Debug)]
enum Mode {
    Baseline,
    Optimized,
}

fn db_counter(conn: &Connection, op: i32) -> i32 {
    let mut current = 0;
    let mut highwater = 0;
    // SAFETY: the connection is live and exclusively used on this test thread;
    // sqlite3_db_status only writes to these valid local output pointers.
    let result = unsafe {
        rusqlite::ffi::sqlite3_db_status(conn.handle(), op, &mut current, &mut highwater, 0)
    };
    assert_eq!(result, rusqlite::ffi::SQLITE_OK);
    current
}

fn counters(conn: &Connection) -> [i32; 4] {
    [
        rusqlite::ffi::SQLITE_DBSTATUS_CACHE_HIT,
        rusqlite::ffi::SQLITE_DBSTATUS_CACHE_MISS,
        rusqlite::ffi::SQLITE_DBSTATUS_CACHE_WRITE,
        rusqlite::ffi::SQLITE_DBSTATUS_CACHE_SPILL,
    ]
    .map(|op| db_counter(conn, op))
}

fn open_encrypted(path: &std::path::Path) -> Connection {
    let conn = Connection::open(path).unwrap();
    crate::storage::connection::configure_sqlcipher_connection(&conn, &BENCH_KEY).unwrap();
    conn
}

#[test]
#[ignore = "requires CARBONPAPER_BITMAP_BENCH_DB pointing to an explicitly authorized plaintext database"]
fn compare_postings_io() {
    let source_path =
        std::env::var("CARBONPAPER_BITMAP_BENCH_DB").expect("explicit benchmark source");
    let source =
        Connection::open_with_flags(&source_path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    source.execute_batch("PRAGMA query_only=ON").unwrap();
    let scratch_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target");
    let scratch = tempfile::Builder::new()
        .prefix("bitmap-index-bench-")
        .tempdir_in(scratch_root)
        .unwrap();
    let base_path = scratch.path().join("base.db");
    let mut base = open_encrypted(&base_path);
    base.execute_batch(
        "PRAGMA journal_mode=DELETE; PRAGMA synchronous=OFF; PRAGMA cache_size=-65536;
        CREATE TABLE blind_bitmap_index(token_hash TEXT PRIMARY KEY, postings_blob BLOB NOT NULL);",
    )
    .unwrap();
    let mut max_id = 0;
    let mut posting_count = 0usize;
    let mut posting_bytes = 0usize;
    {
        let tx = base.transaction().unwrap();
        let mut put = tx
            .prepare("INSERT INTO blind_bitmap_index VALUES (?1,?2)")
            .unwrap();
        let mut read = source
            .prepare("SELECT token_hash, postings_blob FROM blind_bitmap_index")
            .unwrap();
        let mut rows = read.query([]).unwrap();
        while let Some(row) = rows.next().unwrap() {
            let hash: String = row.get(0).unwrap();
            let blob: Vec<u8> = row.get(1).unwrap();
            let bitmap = RoaringBitmap::deserialize_from(&blob[..]).expect("valid index bitmap");
            max_id = max_id.max(bitmap.max().unwrap_or(0));
            posting_count += 1;
            posting_bytes += blob.len();
            put.execute(params![hash, blob]).unwrap();
        }
        drop(put);
        tx.commit().unwrap();
    }
    drop(source);
    let new_id = max_id.checked_add(1).expect("synthetic id capacity");
    let windows: Vec<RoaringBitmap> = [0, 1000, 10000]
        .into_iter()
        .map(|offset| {
            let end = max_id.saturating_sub(offset);
            (end.saturating_sub(99)..=end).collect()
        })
        .collect();
    let mut workloads = vec![Vec::<String>::new(); windows.len()];
    {
        let mut read = base
            .prepare("SELECT token_hash, postings_blob FROM blind_bitmap_index")
            .unwrap();
        let mut rows = read.query([]).unwrap();
        while let Some(row) = rows.next().unwrap() {
            let blob: Vec<u8> = row.get(1).unwrap();
            let bitmap = RoaringBitmap::deserialize_from(&blob[..]).unwrap();
            for (hashes, window) in workloads.iter_mut().zip(&windows) {
                if !bitmap.is_disjoint(window) {
                    hashes.push(row.get::<_, String>(0).unwrap());
                }
            }
        }
    }
    for hashes in &mut workloads {
        hashes.sort_unstable();
        // Fixed shuffle: every strategy receives exactly the same hash order.
        let mut seed = 0x1234abcd_u64;
        for i in (1..hashes.len()).rev() {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            hashes.swap(i, (seed as usize) % (i + 1));
        }
        hashes.truncate(450);
        assert!(!hashes.is_empty(), "sample must contain indexed postings");
    }
    let engine = crate::storage::connection::inspect_sqlite_engine(&base).unwrap();
    eprintln!(
        "BENCH {}",
        serde_json::json!({"setup":true,"sqlite":engine.sqlite_version,"cipher":engine.cipher_version,"postings":posting_count,"posting_bytes":posting_bytes,"tokens":workloads.iter().map(Vec::len).collect::<Vec<_>>(),"encrypted_copies":true,"cache_kib":2000,"synchronous":"FULL","wal_autocheckpoint":1000})
    );
    let mut expected: HashMap<String, RoaringBitmap> = HashMap::new();
    for hashes in &workloads {
        for hash in hashes {
            if !expected.contains_key(hash) {
                let blob: Vec<u8> = base
                    .query_row(
                        "SELECT postings_blob FROM blind_bitmap_index WHERE token_hash=?1",
                        [hash],
                        |row| row.get(0),
                    )
                    .unwrap();
                expected.insert(
                    hash.clone(),
                    RoaringBitmap::deserialize_from(&blob[..]).unwrap(),
                );
            }
        }
    }
    const ROUNDS: usize = 6;
    new_id
        .checked_add((ROUNDS * workloads.len() * 100) as u32)
        .expect("synthetic id capacity");
    for round in 0..ROUNDS {
        for (sample, hashes) in workloads.iter().enumerate() {
            for (index, hash) in hashes.iter().enumerate() {
                expected
                    .get_mut(hash)
                    .unwrap()
                    .insert(new_id + (round * 300 + sample * 100 + index % 100) as u32);
            }
        }
    }
    drop(base);

    let modes = [Mode::Baseline, Mode::Optimized];
    let mut connections = Vec::new();
    for mode in modes {
        let path = scratch.path().join(format!("run-{mode:?}.db"));
        std::fs::copy(&base_path, &path).unwrap();
        let conn = open_encrypted(&path);
        conn.execute_batch("PRAGMA journal_mode=WAL").unwrap();
        connections.push(conn);
    }
    let mut results = Vec::new();
    for round in 0..ROUNDS {
        for (sample, hashes) in workloads.iter().enumerate() {
            let mut mode_order = [0, 1];
            if (round + sample) % 2 != 0 {
                mode_order.reverse();
            }
            for mode_index in mode_order {
                let mode = modes[mode_index];
                let conn = &mut connections[mode_index];
                let before = counters(conn);
                let started = Instant::now();
                let cache_budget = if matches!(mode, Mode::Optimized) {
                    Some(PostingsCacheBudget::new(conn).unwrap())
                } else {
                    None
                };
                let tx = conn.unchecked_transaction().unwrap();
                let mut read_time = Duration::ZERO;
                let mut merge_time = Duration::ZERO;
                let mut serialize_time = Duration::ZERO;
                let mut write_time = Duration::ZERO;
                let mut bytes = 0usize;
                let time = Instant::now();
                let mut order: Vec<usize> = (0..hashes.len()).collect();
                if matches!(mode, Mode::Optimized) {
                    let positions: HashMap<&str, usize> = hashes
                        .iter()
                        .enumerate()
                        .map(|(i, h)| (h.as_str(), i))
                        .collect();
                    order = ordered_token_hashes(hashes.iter())
                        .into_iter()
                        .map(|hash| positions[hash])
                        .collect();
                }
                read_time += time.elapsed();
                let mut get = tx
                    .prepare_cached(
                        "SELECT postings_blob FROM blind_bitmap_index WHERE token_hash=?1",
                    )
                    .unwrap();
                let mut put = tx
                    .prepare_cached(if matches!(mode, Mode::Optimized) {
                        UPSERT_POSTING_SQL
                    } else {
                        "INSERT OR REPLACE INTO blind_bitmap_index VALUES (?1,?2)"
                    })
                    .unwrap();
                for &index in &order {
                    let hash = &hashes[index];
                    let time = Instant::now();
                    let blob: Vec<u8> = get.query_row([hash], |row| row.get(0)).unwrap();
                    read_time += time.elapsed();
                    bytes += blob.len();
                    let time = Instant::now();
                    let mut bitmap = RoaringBitmap::deserialize_from(&blob[..]).unwrap();
                    let addition = new_id + (round * 300 + sample * 100 + index % 100) as u32;
                    bitmap.insert(addition);
                    merge_time += time.elapsed();
                    let time = Instant::now();
                    let mut encoded = Vec::new();
                    bitmap.serialize_into(&mut encoded).unwrap();
                    serialize_time += time.elapsed();
                    let time = Instant::now();
                    put.execute(params![hash, encoded]).unwrap();
                    write_time += time.elapsed();
                }
                drop(get);
                drop(put);
                let time = Instant::now();
                tx.commit().unwrap();
                let commit_time = time.elapsed();
                drop(cache_budget);
                let total = started.elapsed();
                let after = counters(conn);
                let result = serde_json::json!({"round":round,"sample":sample,"mode":format!("{mode:?}"),"tokens":hashes.len(),"bytes":bytes,"total_ms":total.as_secs_f64()*1000.,"read_ms":read_time.as_secs_f64()*1000.,"merge_ms":merge_time.as_secs_f64()*1000.,"serialize_ms":serialize_time.as_secs_f64()*1000.,"write_ms":write_time.as_secs_f64()*1000.,"commit_ms":commit_time.as_secs_f64()*1000.,"cache_hits":after[0]-before[0],"cache_misses":after[1]-before[1],"cache_writes":after[2]-before[2],"cache_spills":after[3]-before[3]});
                eprintln!("BENCH {result}");
                results.push(result);
            }
        }
    }
    // Verify complete bitmap equality with an independently computed expected
    // union, after timing so verification does not warm measured connections.
    for (hash, expected_bitmap) in &expected {
        for conn in &connections {
            let blob: Vec<u8> = conn
                .query_row(
                    "SELECT postings_blob FROM blind_bitmap_index WHERE token_hash=?1",
                    [hash],
                    |row| row.get(0),
                )
                .unwrap();
            let bitmap = RoaringBitmap::deserialize_from(&blob[..]).unwrap();
            assert!(
                *expected_bitmap == bitmap,
                "benchmark must preserve the full expected posting"
            );
        }
    }
    if let Ok(report) = std::env::var("CARBONPAPER_BITMAP_BENCH_REPORT") {
        std::fs::write(report, serde_json::to_vec_pretty(&serde_json::json!({"sqlite":engine.sqlite_version,"cipher":engine.cipher_version,"source_postings":posting_count,"source_posting_bytes":posting_bytes,"rounds":ROUNDS,"optimized_cache_kib":POSTINGS_CACHE_KIB,"profile":"debug","encrypted_copies":true,"journal_mode":"wal","synchronous":"FULL","results":results})).unwrap()).unwrap();
    }
}
