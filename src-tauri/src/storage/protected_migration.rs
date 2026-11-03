//! Restartable encryption of legacy derived rows. No protected history is read:
//! only the legacy plaintext fields are sealed with fresh public-key-wrapped keys.
use super::StorageState;
use base64::{engine::general_purpose::STANDARD, Engine};
use carbonpaper_app_bound::payload::payload_key_id;
use rusqlite::{params, Connection, OptionalExtension};

impl StorageState {
    pub(super) fn migrate_protected_content(&self, conn: &Connection) -> Result<(), String> {
        let version: Option<String> = conn
            .query_row(
                "SELECT value FROM app_metadata WHERE key='protected_content_version'",
                [],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        if version.as_deref() == Some("1") {
            return self.import_legacy_anchors(conn);
        }
        self.vacuum_space_check()?;
        conn.execute_batch("PRAGMA secure_delete=ON;")
            .map_err(|e| e.to_string())?;
        self.import_legacy_anchors(conn)?;
        for (table, id, columns, prefix) in [
            (
                "smart_clusters",
                "id",
                vec!["anchor_text", "display_name", "anchor_vector_source_hash"],
                "cluster",
            ),
            (
                "smart_cluster_summaries",
                "smart_cluster_id",
                vec![
                    "title",
                    "summary",
                    "ocr_summary",
                    "key_points_json",
                    "evidence_json",
                    "source_hash",
                ],
                "cluster_summary",
            ),
            (
                "classification_anchors",
                "id",
                vec!["anchors_json"],
                "classification",
            ),
        ] {
            for column in columns {
                let mut cursor = 0i64;
                loop {
                    let mut statement=conn.prepare(&format!("SELECT {id},{column} FROM {table} WHERE {id}>?1 AND {column} IS NOT NULL ORDER BY {id} LIMIT 1024")).map_err(|e|e.to_string())?;
                    let rows = statement
                        .query_map([cursor], |r| {
                            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
                        })
                        .map_err(|e| e.to_string())?
                        .collect::<Result<Vec<_>, _>>()
                        .map_err(|e| e.to_string())?;
                    drop(statement);
                    if rows.is_empty() {
                        break;
                    }
                    let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
                    for (row_id, text) in rows {
                        cursor = row_id;
                        if is_text_envelope(&text) {
                            continue;
                        }
                        let identity = text_identity(prefix, column, row_id);
                        let encrypted = self.protected.text(&identity, &text)?;
                        tx.execute(
                            &format!("UPDATE {table} SET {column}=?1 WHERE {id}=?2"),
                            params![encrypted, row_id],
                        )
                        .map_err(|e| e.to_string())?;
                    }
                    tx.commit().map_err(|e| e.to_string())?;
                }
            }
        }
        for (table, id, column, purpose, prefix) in [(
            "smart_clusters",
            "id",
            "anchor_vector",
            "vectors",
            "cluster_anchor_vector",
        )] {
            let mut cursor = 0i64;
            loop {
                let mut statement=conn.prepare(&format!("SELECT {id},{column} FROM {table} WHERE {id}>?1 AND {column} IS NOT NULL ORDER BY {id} LIMIT 512")).map_err(|e|e.to_string())?;
                let rows = statement
                    .query_map([cursor], |r| {
                        Ok((r.get::<_, i64>(0)?, r.get::<_, Vec<u8>>(1)?))
                    })
                    .map_err(|e| e.to_string())?
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|e| e.to_string())?;
                drop(statement);
                if rows.is_empty() {
                    break;
                }
                let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
                for (row_id, bytes) in rows {
                    cursor = row_id;
                    if payload_key_id(&bytes).is_ok() {
                        continue;
                    }
                    let encrypted =
                        self.protected
                            .seal(purpose, &format!("{prefix}:{row_id}"), &bytes)?;
                    tx.execute(
                        &format!("UPDATE {table} SET {column}=?1 WHERE {id}=?2"),
                        params![encrypted, row_id],
                    )
                    .map_err(|e| e.to_string())?;
                }
                tx.commit().map_err(|e| e.to_string())?;
            }
        }
        let mut cursor = 0i64;
        loop {
            let mut query=conn.prepare("SELECT rowid,index_kind,subject_key,vector_f32 FROM derived_embeddings WHERE rowid>?1 ORDER BY rowid LIMIT 512").map_err(|e|e.to_string())?;
            let rows = query
                .query_map([cursor], |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, Vec<u8>>(3)?,
                    ))
                })
                .map_err(|e| e.to_string())?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| e.to_string())?;
            drop(query);
            if rows.is_empty() {
                break;
            }
            let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
            for (row_id, kind, subject, bytes) in rows {
                cursor = row_id;
                if payload_key_id(&bytes).is_ok() {
                    continue;
                }
                let encrypted =
                    self.protected
                        .seal("vectors", &format!("{kind}:{subject}"), &bytes)?;
                tx.execute(
                    "UPDATE derived_embeddings SET vector_f32=?1 WHERE rowid=?2",
                    params![encrypted, row_id],
                )
                .map_err(|e| e.to_string())?;
            }
            tx.commit().map_err(|e| e.to_string())?;
        }
        let mut cursor = 0i64;
        loop {
            let mut query=conn.prepare("SELECT id,process_name FROM screenshots WHERE id>?1 AND process_name IS NOT NULL ORDER BY id LIMIT 2048").map_err(|e|e.to_string())?;
            let rows = query
                .query_map([cursor], |r| {
                    Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
                })
                .map_err(|e| e.to_string())?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| e.to_string())?;
            drop(query);
            if rows.is_empty() {
                break;
            }
            let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
            // This is a representation change, not a new source revision.
            // DDL and the page update share a transaction so a crash or error
            // restores the old triggers before any capture can resume.
            tx.execute_batch(
                "DROP TRIGGER IF EXISTS app_bound_metadata_update;
                DROP TRIGGER IF EXISTS recap_s_UPDATE;",
            )
            .map_err(|e| e.to_string())?;
            for (id, name) in rows {
                cursor = id;
                if name.starts_with("cpp1:") && name.len() == 102 {
                    let exists: bool = tx
                        .query_row(
                            "SELECT EXISTS(SELECT 1 FROM protected_process_names WHERE token=?1)",
                            [&name],
                            |r| r.get(0),
                        )
                        .map_err(|e| e.to_string())?;
                    if !exists {
                        return Err(
                            "Protected process catalog is missing; recovery is required".into()
                        );
                    }
                    continue;
                }
                let token = self.protected.process_token(&tx, &name)?;
                tx.execute(
                    "UPDATE screenshots SET process_name=?1 WHERE id=?2",
                    params![token, id],
                )
                .map_err(|e| e.to_string())?;
            }
            self.init_processing_stage_schema(&tx)?;
            Self::init_recap_tables(&tx)?;
            tx.commit().map_err(|e| e.to_string())?;
        }
        let has_task_labels: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM pragma_table_info('tasks') WHERE name='label')",
                [],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())?;
        if has_task_labels {
            let mut statement = conn
                .prepare("SELECT id,label FROM tasks WHERE label IS NOT NULL")
                .map_err(|e| e.to_string())?;
            let rows = statement
                .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))
                .map_err(|e| e.to_string())?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| e.to_string())?;
            drop(statement);
            for (id, label) in rows {
                if !is_text_envelope(&label) {
                    let encrypted = self.protected.text(&format!("task_label:{id}"), &label)?;
                    conn.execute(
                        "UPDATE tasks SET label=?1 WHERE id=?2",
                        params![encrypted, id],
                    )
                    .map_err(|e| e.to_string())?;
                }
            }
        }
        // Construction inputs and old sidecars are rebuildable, unlike summaries.
        conn.execute_batch(
            "DELETE FROM ann_build_inputs; DELETE FROM ann_build_checkpoints;
            DELETE FROM derived_ann_generations; DELETE FROM derived_index_generations;",
        )
        .map_err(|e| e.to_string())?;
        let data_dir = self
            .data_dir
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        retire_legacy_sidecars(&data_dir)?;
        // Sealing columns alone leaves plaintext in freed pages and previous WALs.
        // Keep the version marker absent until physical compaction succeeds.
        Self::checkpoint_wal_after_vacuum(conn)?;
        conn.execute_batch("VACUUM;").map_err(|e| {
            format!("Protected-content compaction must finish before capture resumes: {e}")
        })?;
        Self::checkpoint_wal_after_vacuum(conn)?;
        conn.execute(
            "INSERT OR REPLACE INTO app_metadata(key,value) VALUES('protected_content_version','1')",
            [],
        )
        .map_err(|e| e.to_string())?;
        Self::checkpoint_wal_after_vacuum(conn)?;
        Ok(())
    }
}

fn is_text_envelope(text: &str) -> bool {
    text.strip_prefix("cpe1:")
        .and_then(|encoded| STANDARD.decode(encoded).ok())
        .is_some_and(|bytes| payload_key_id(&bytes).is_ok())
}

fn retire_legacy_sidecars(directory: &std::path::Path) -> Result<(), String> {
    let entries = match std::fs::read_dir(directory.join("derived-indexes")) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("Cannot retire legacy vector sidecars: {error}")),
    };
    for entry in entries {
        let entry = entry.map_err(|e| e.to_string())?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let owned = name.trim_start_matches('.');
        if (owned.starts_with("clip_image-") || owned.starts_with("semantic_text-"))
            && [".cpdvec", ".cpdann", ".tmp", ".partial"]
                .iter()
                .any(|suffix| name.ends_with(suffix))
        {
            std::fs::remove_file(entry.path())
                .map_err(|e| format!("Cannot retire legacy vector sidecar {name}: {e}"))?;
        }
    }
    Ok(())
}

fn text_identity(prefix: &str, column: &str, id: i64) -> String {
    match (prefix, column) {
        ("cluster", "anchor_text") => format!("cluster_anchor:{id}"),
        ("cluster", "display_name") => format!("cluster_name:{id}"),
        ("cluster", "anchor_vector_source_hash") => format!("cluster_anchor_hash:{id}"),
        ("classification", _) => "classification_anchors:1".into(),
        _ => format!("{prefix}:{id}:{column}"),
    }
}
