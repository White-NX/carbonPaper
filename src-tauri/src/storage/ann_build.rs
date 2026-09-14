//! Durable frozen ANN input and complete-checkpoint cursors. No file operation
//! is performed while an archive transaction or publication lock is held.
use super::{DerivedAnnSnapshotRow, StorageState};
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct AnnCheckpointFile {
    pub name: String,
    pub checksum: String,
    pub rows: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct AnnBuildCheckpoint {
    #[serde(skip)]
    pub runtime_generation: Option<u64>,
    pub generation: u64,
    pub dataset_id: String,
    pub model_fingerprint: String,
    pub covered_epoch: u64,
    pub scan_upper: String,
    pub scan_cursor: String,
    pub expected_rows: u64,
    pub frozen_rows: u64,
    pub key_bytes: u64,
    pub phase: String,
    pub materialized_rows: u64,
    pub materialized_key_bytes: u64,
    pub flat_checksum: Option<String>,
    pub graph_bytes: u64,
    pub complete: Option<AnnCheckpointFile>,
    pub previous: Option<AnnCheckpointFile>,
    pub copy_offset: u64,
}

impl AnnBuildCheckpoint {
    pub(crate) fn flat_name(&self) -> String {
        format!("clip_image-{}.cpdvec", self.generation)
    }
    pub(crate) fn ann_name(&self) -> String {
        format!("clip_image-{}.cpdann", self.generation)
    }
    pub(crate) fn flat_bytes(&self) -> u64 {
        8192 + (self.expected_rows.max(self.frozen_rows) + 1) * 8
            + self.key_bytes
            + self.expected_rows.max(self.frozen_rows) * 512 * 4
    }
    pub(crate) fn small(&self) -> bool {
        self.expected_rows.max(self.frozen_rows) <= crate::background_policy::SMALL_ANN_ROWS
            && self.flat_bytes() <= crate::background_policy::SMALL_ANN_BYTES
            && self.graph_bytes <= crate::background_policy::SMALL_ANN_BYTES
    }
}

fn sql_integer(value: u64) -> Result<i64, String> {
    i64::try_from(value).map_err(|_| "ANN cursor exceeds SQLite range".into())
}

fn current_dataset(conn: &rusqlite::Connection, dataset: &str) -> Result<(), String> {
    let actual: String = conn
        .query_row(
            "SELECT value FROM app_metadata WHERE key='app_bound_dataset_id'",
            [],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    if actual != dataset {
        return Err("background_paused: ANN database changed".into());
    }
    Ok(())
}

fn update_state(conn: &rusqlite::Connection, state: &AnnBuildCheckpoint) -> Result<(), String> {
    current_dataset(conn, &state.dataset_id)?;
    let changes = conn.execute("UPDATE ann_build_checkpoints SET state_json=?2 WHERE index_kind='clip_image' AND generation=?1 AND dataset_id=?3",
        params![sql_integer(state.generation)?, serde_json::to_string(state).map_err(|e| e.to_string())?, state.dataset_id]).map_err(|e| e.to_string())?;
    if changes != 1 {
        return Err("background_paused: ANN build was replaced".into());
    }
    Ok(())
}

impl StorageState {
    pub(crate) fn check_ann_runtime(&self, state: &AnnBuildCheckpoint) -> Result<(), String> {
        if state
            .runtime_generation
            .is_some_and(|generation| generation != self.db_generation())
        {
            return Err("background_paused: ANN database generation changed".into());
        }
        Ok(())
    }
    pub(crate) fn cleanup_ann_inputs(&self, state: &AnnBuildCheckpoint) -> Result<bool, String> {
        let guard = self.get_connection_named("cleanup_ann_inputs")?;
        self.check_ann_runtime(state)?;
        let conn = guard.as_ref().ok_or("Database not initialized")?;
        current_dataset(conn, &state.dataset_id)?;
        let deleted = conn.execute("DELETE FROM ann_build_inputs WHERE rowid IN (SELECT rowid FROM ann_build_inputs LIMIT 256)", []).map_err(|e| e.to_string())?;
        Ok(deleted > 0)
    }
    pub(crate) fn ann_build_checkpoint(&self) -> Result<Option<AnnBuildCheckpoint>, String> {
        let guard = self.get_connection_named("ann_build_checkpoint")?;
        let json: Option<String> = guard
            .as_ref()
            .ok_or("Database not initialized")?
            .query_row(
                "SELECT state_json FROM ann_build_checkpoints WHERE index_kind='clip_image'",
                [],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        json.map(|s| {
            serde_json::from_str(&s).map_err(|e| format!("invalid ANN checkpoint metadata: {e}"))
        })
        .transpose()
    }

    pub(crate) fn begin_ann_build(&self, state: &mut AnnBuildCheckpoint) -> Result<(), String> {
        let guard = self.get_connection_named("begin_ann_build")?;
        self.check_ann_runtime(state)?;
        let conn = guard.as_ref().ok_or("Database not initialized")?;
        let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
        current_dataset(&tx, &state.dataset_id)?;
        // Fixed keyset bound: new keys beyond it belong to the ANN change tail.
        state.scan_upper = tx.query_row("SELECT COALESCE(MAX(subject_key),'') FROM derived_embeddings WHERE index_kind='clip_image'", [], |r| r.get(0)).map_err(|e| e.to_string())?;
        tx.execute("INSERT INTO ann_build_checkpoints(index_kind,generation,dataset_id,state_json) VALUES('clip_image',?1,?2,?3)
            ON CONFLICT(index_kind) DO UPDATE SET generation=excluded.generation,dataset_id=excluded.dataset_id,state_json=excluded.state_json",
            params![sql_integer(state.generation)?, state.dataset_id, serde_json::to_string(state).map_err(|e| e.to_string())?]).map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())
    }

    pub(crate) fn save_ann_build_checkpoint(
        &self,
        state: &AnnBuildCheckpoint,
    ) -> Result<(), String> {
        let guard = self.get_connection_named("save_ann_build_checkpoint")?;
        self.check_ann_runtime(state)?;
        update_state(guard.as_ref().ok_or("Database not initialized")?, state)
    }

    pub(crate) fn freeze_ann_page(
        &self,
        state: &mut AnnBuildCheckpoint,
        limit: u32,
    ) -> Result<(), String> {
        let guard = self.get_connection_named("freeze_ann_page")?;
        self.check_ann_runtime(state)?;
        let conn = guard.as_ref().ok_or("Database not initialized")?;
        let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
        current_dataset(&tx, &state.dataset_id)?;
        let sql = super::derived_index::visible_embedding_page_sql().replace(
            "AND e.subject_key > ?2",
            "AND e.subject_key > ?2 AND e.subject_key <= ?4",
        );
        let page = {
            let mut stmt = tx.prepare(&sql).map_err(|e| e.to_string())?;
            let rows = stmt
                .query_map(
                    params!["clip_image", state.scan_cursor, limit, state.scan_upper],
                    |r| {
                        Ok(DerivedAnnSnapshotRow {
                            subject_key: r.get(0)?,
                            dimensions: r.get(1)?,
                            vector_f32: r.get(2)?,
                        })
                    },
                )
                .map_err(|e| e.to_string())?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
                .map_err(|e| e.to_string())?
        };
        let mut next = state.clone();
        if page.is_empty() {
            next.phase = "materialize".into();
        }
        for row in page {
            if row.dimensions != 512 || row.vector_f32.len() != 2048 {
                return Err("ANN frozen input contract mismatch".into());
            }
            tx.execute("INSERT INTO ann_build_inputs(generation,ordinal,subject_key,vector_f32) VALUES(?1,?2,?3,?4)",
                params![sql_integer(next.generation)?, sql_integer(next.frozen_rows)?, row.subject_key, row.vector_f32]).map_err(|e| e.to_string())?;
            next.frozen_rows += 1;
            next.key_bytes += row.subject_key.len() as u64;
            next.scan_cursor = row.subject_key;
        }
        update_state(&tx, &next)?;
        tx.commit().map_err(|e| e.to_string())?;
        *state = next;
        Ok(())
    }

    pub(crate) fn frozen_ann_page(
        &self,
        state: &AnnBuildCheckpoint,
        after: u64,
        limit: u32,
    ) -> Result<Vec<DerivedAnnSnapshotRow>, String> {
        let guard = self.get_connection_named("frozen_ann_page")?;
        self.check_ann_runtime(state)?;
        let conn = guard.as_ref().ok_or("Database not initialized")?;
        current_dataset(conn, &state.dataset_id)?;
        let mut stmt = conn.prepare("SELECT subject_key,vector_f32 FROM ann_build_inputs WHERE generation=?1 AND ordinal>=?2 ORDER BY ordinal LIMIT ?3").map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(
                params![sql_integer(state.generation)?, sql_integer(after)?, limit],
                |r| {
                    Ok(DerivedAnnSnapshotRow {
                        subject_key: r.get(0)?,
                        dimensions: 512,
                        vector_f32: r.get(1)?,
                    })
                },
            )
            .map_err(|e| e.to_string())?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|e| e.to_string())
    }
}
