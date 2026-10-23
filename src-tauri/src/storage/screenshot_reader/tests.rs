use super::*;
use crate::credential_manager::CredentialManagerState;
use rusqlite::Connection;
use std::sync::Arc;

const START: f64 = 1_700_000_000.0;
const END: f64 = START + 60.0;

fn storage_fixture() -> (tempfile::TempDir, StorageState) {
    let directory = tempfile::tempdir().unwrap();
    let credentials = Arc::new(CredentialManagerState::new(directory.path().to_path_buf()));
    let storage = StorageState::new(directory.path().to_path_buf(), credentials);
    let connection = Connection::open_in_memory().unwrap();
    storage.init_tables(&connection).unwrap();
    connection
        .execute(
            "INSERT INTO screenshots
                (id, image_path, image_hash, width, height, window_title, process_name,
                 metadata, created_at, source, category, category_confidence)
             VALUES (1, 'screenshots/one.png.enc', 'hash-one', 640, 480, 'First window',
                     'app.exe', '{\"fixture\":true}', ?1, 'browser', 'Work', 0.75)",
            [sqlite_datetime(START)],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO screenshots (id, image_path, image_hash, created_at)
             VALUES (2, 'screenshots/two.png.enc', 'hash-two', ?1)",
            [sqlite_datetime(END)],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO screenshots (id, image_path, image_hash, created_at, is_deleted)
             VALUES (3, 'screenshots/deleted.png.enc', 'hash-deleted', ?1, 1)",
            [sqlite_datetime(START + 30.0)],
        )
        .unwrap();
    *storage.db.lock().unwrap() = Some(connection);
    (directory, storage)
}

#[test]
fn full_record_queries_share_the_projection_and_skip_deleted_rows() {
    let (_directory, storage) = storage_fixture();
    let reader = storage.screenshots();
    let first = reader.by_id(1).unwrap().unwrap();
    assert_eq!(first.width, Some(640));
    assert_eq!(first.height, Some(480));
    assert_eq!(first.window_title.as_deref(), Some("First window"));
    assert_eq!(first.process_name.as_deref(), Some("app.exe"));
    assert_eq!(first.metadata.as_deref(), Some("{\"fixture\":true}"));
    assert_eq!(first.source.as_deref(), Some("browser"));
    assert_eq!(first.category.as_deref(), Some("Work"));
    assert_eq!(first.category_confidence, Some(0.75));
    assert_eq!(first.timestamp, Some(START as i64));
    assert_eq!(first.created_at, "2023-11-14T22:13:20Z");

    for found in [
        reader.by_image_path(&first.image_path).unwrap().unwrap(),
        reader.by_image_path("memory://hash-one").unwrap().unwrap(),
        reader.in_time_range(START, END, Some(1)).unwrap().remove(0),
    ] {
        assert_eq!(
            serde_json::to_value(found).unwrap(),
            serde_json::to_value(&first).unwrap()
        );
    }
    assert!(reader.by_id(3).unwrap().is_none());
    assert!(reader.by_id(99).unwrap().is_none());
    assert!(reader
        .by_image_path("memory://hash-deleted")
        .unwrap()
        .is_none());
    assert!(reader.by_image_path("' OR 1=1 --").unwrap().is_none());
}

#[test]
fn range_queries_keep_inclusive_second_bounds_and_apply_the_limit() {
    let (_directory, storage) = storage_fixture();
    let reader = storage.screenshots();
    let ids =
        |records: Vec<ScreenshotRecord>| records.into_iter().map(|row| row.id).collect::<Vec<_>>();
    assert_eq!(
        ids(reader.in_time_range(START, END, None).unwrap()),
        vec![1, 2]
    );
    assert_eq!(
        ids(reader.in_time_range(START, END, Some(1)).unwrap()),
        vec![1]
    );
    assert!(reader
        .in_time_range(START, END, Some(0))
        .unwrap()
        .is_empty());
    assert_eq!(ids(reader.in_time_range(END, END, None).unwrap()), vec![2]);
    assert!(reader.in_time_range(END, START, None).unwrap().is_empty());
}

#[test]
fn row_decoding_failures_are_reported_instead_of_discarding_records() {
    let (_directory, storage) = storage_fixture();
    storage
        .db
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .execute("UPDATE screenshots SET width = 'invalid' WHERE id = 1", [])
        .unwrap();
    let error = storage
        .screenshots()
        .in_time_range(START, END, None)
        .unwrap_err();
    assert!(
        error.contains("Failed to read screenshot range row"),
        "{error}"
    );
    assert!(storage.screenshots().by_id(1).is_err());
}

#[test]
fn recent_captures_have_one_row_per_screenshot_and_exclude_aborted_records() {
    let (_directory, storage) = storage_fixture();
    let guard = storage.db.lock().unwrap();
    let connection = guard.as_ref().unwrap();
    connection
        .execute_batch(
            "INSERT INTO ocr_results (screenshot_id, text, text_hash, confidence) VALUES
             (1, 'first box', 'hash1', 0.9), (1, 'second box', 'hash2', 0.8);
         INSERT INTO screenshots (id, image_path, image_hash, status, created_at)
             VALUES (4, 'aborted.png', 'aborted', 'aborted', '2023-11-15 00:00:00');",
        )
        .unwrap();
    drop(guard);

    let captures = storage.screenshots().recent(500).unwrap();
    assert_eq!(
        captures
            .iter()
            .map(|row| row.screenshot_id)
            .collect::<Vec<_>>(),
        vec![2, 1]
    );
    assert_eq!(captures[1].created_at, "2023-11-14T22:13:20Z");
    assert_eq!(storage.screenshots().recent(1).unwrap().len(), 1);
}
