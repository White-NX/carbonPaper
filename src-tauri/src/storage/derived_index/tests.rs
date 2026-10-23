//! Shared fixtures for ledger, lifecycle, ANN and query regressions.

use super::*;
use crate::credential_manager::CredentialManagerState;
use rusqlite::Connection;
use std::sync::Arc;

use super::ann::ANN_TAIL_UPDATES_SQL;
use std::time::Instant;

fn test_storage() -> (tempfile::TempDir, StorageState) {
    let temp = tempfile::tempdir().expect("temp storage directory");
    let credential_state = Arc::new(CredentialManagerState::new(temp.path().to_path_buf()));
    let storage = StorageState::new(temp.path().to_path_buf(), credential_state);
    let connection = Connection::open_in_memory().expect("in-memory database");
    storage.init_tables(&connection).expect("initialize schema");
    *storage.db.lock().unwrap_or_else(|error| error.into_inner()) = Some(connection);
    (temp, storage)
}

fn job(kind: DerivedIndexKind, subject_key: &str) -> DerivedIndexJobSpec {
    DerivedIndexJobSpec {
        index_kind: kind,
        subject_key: subject_key.to_string(),
        model_id: "model-a".to_string(),
        model_revision: "revision-1".to_string(),
        embedding_version: 1,
        source_fingerprint: format!("source-{subject_key}"),
    }
}

fn ensure_active_subject(storage: &StorageState, spec: &DerivedIndexJobSpec) {
    let guard = storage.db.lock().unwrap_or_else(|error| error.into_inner());
    let conn = guard.as_ref().unwrap();
    match spec.index_kind {
        DerivedIndexKind::SemanticText => {
            let id = spec
                .subject_key
                .parse::<i64>()
                .expect("semantic subject key must be a screenshot id");
            conn.execute(
                    "INSERT OR IGNORE INTO screenshots (id, image_path, image_hash) VALUES (?1, ?2, ?3)",
                    params![id, format!("{id}.enc"), format!("semantic-hash-{id}")],
                )
                .unwrap();
        }
        DerivedIndexKind::ClipImage => {
            conn.execute(
                "INSERT OR IGNORE INTO screenshots (image_path, image_hash) VALUES (?1, ?2)",
                params![format!("{}.enc", spec.subject_key), spec.subject_key],
            )
            .unwrap();
        }
    }
}

fn queue_and_claim(storage: &StorageState, spec: &DerivedIndexJobSpec) -> String {
    ensure_active_subject(storage, spec);
    storage.upsert_derived_index_job(spec).unwrap();
    storage.mark_derived_index_job_processing(spec).unwrap()
}

fn claimed_write(
    storage: &StorageState,
    spec: DerivedIndexJobSpec,
    vector: Vec<f32>,
) -> DerivedEmbeddingWrite {
    let lease_token = queue_and_claim(storage, &spec);
    DerivedEmbeddingWrite {
        job: spec,
        lease_token,
        vector,
    }
}

fn commit_vector(
    storage: &StorageState,
    spec: DerivedIndexJobSpec,
    vector: Vec<f32>,
) -> Result<(), String> {
    storage.commit_derived_embedding(&claimed_write(storage, spec, vector))
}

fn current_epoch(storage: &StorageState) -> u64 {
    let guard = storage.db.lock().unwrap_or_else(|error| error.into_inner());
    u64::try_from(
        read_derived_data_epoch(guard.as_ref().unwrap(), DerivedIndexKind::ClipImage).unwrap(),
    )
    .unwrap()
}

fn ann_checkpoint(storage: &StorageState, generation: u64) -> crate::storage::AnnBuildCheckpoint {
    crate::storage::AnnBuildCheckpoint {
        runtime_generation: Some(storage.db_generation()),
        generation,
        dataset_id: storage.processing_dataset_id().unwrap(),
        model_fingerprint: "revision-1".into(),
        covered_epoch: current_epoch(storage),
        scan_upper: String::new(),
        scan_cursor: String::new(),
        expected_rows: 3,
        frozen_rows: 0,
        key_bytes: 0,
        phase: "freeze".into(),
        materialized_rows: 0,
        materialized_key_bytes: 0,
        flat_checksum: None,
        graph_bytes: 0,
        complete: None,
        previous: None,
        copy_offset: 0,
    }
}

fn ann_vector(axis: usize) -> Vec<f32> {
    let mut v = vec![0.0; 512];
    v[axis] = 1.0;
    v
}

/// Give a screenshot a `created_at` the retention queries can reason about.
fn backdate_screenshot(storage: &StorageState, id: i64, modifier: &str) {
    let guard = storage.db.lock().unwrap_or_else(|error| error.into_inner());
    guard
        .as_ref()
        .unwrap()
        .execute(
            "UPDATE screenshots SET created_at = datetime('now', ?2) WHERE id = ?1",
            params![id, modifier],
        )
        .unwrap();
}

fn insert_screenshot_with_ocr(storage: &StorageState, id: i64) {
    let guard = storage.db.lock().unwrap_or_else(|error| error.into_inner());
    let conn = guard.as_ref().unwrap();
    conn.execute(
        "INSERT OR IGNORE INTO screenshots (id, image_path, image_hash) VALUES (?1, ?2, ?3)",
        params![id, format!("{id}.enc"), format!("hash-{id}")],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO ocr_results (screenshot_id, text, text_hash) VALUES (?1, ?2, ?3)",
        params![id, format!("text-{id}"), format!("text-hash-{id}")],
    )
    .unwrap();
}

fn ann_manifest(covered_epoch: u64) -> DerivedAnnGeneration {
    DerivedAnnGeneration {
        index_kind: DerivedIndexKind::ClipImage,
        generation: 1,
        covered_epoch,
        flat_file_name: "clip_image-1.cpdvec".to_string(),
        flat_checksum_sha256: "00".repeat(32),
        ann_file_name: "clip_image-1.cpdann".to_string(),
        ann_checksum_sha256: "11".repeat(32),
        row_count: 1,
        dimensions: 2,
        model_id: "model-a".to_string(),
        model_revision: "revision-1".to_string(),
        embedding_version: 1,
        sidecar_format_version: 4,
        ann_format_version: 1,
        algorithm: "hnsw".to_string(),
        implementation_version: "usearch-2.26.0".to_string(),
        metric: "ip".to_string(),
        quantization: "i8".to_string(),
        connectivity: 16,
        expansion_add: 160,
        expansion_search: 256,
        created_at: "2026-08-13T00:00:00Z".to_string(),
    }
}

fn data_epoch(storage: &StorageState) -> i64 {
    let guard = storage.db.lock().unwrap_or_else(|error| error.into_inner());
    read_derived_data_epoch(guard.as_ref().unwrap(), DerivedIndexKind::SemanticText).unwrap()
}

mod ann;
mod candidates;
mod jobs;
mod lifecycle;
mod queries;
