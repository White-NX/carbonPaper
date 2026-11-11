//! Bounded, restartable storage rewrites. Readers accept both old and compact
//! rows throughout the migration; a batch and its cursor commit together.
use super::StorageState;
use rusqlite::{params, Connection, OptionalExtension};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter};

mod progress;
pub(super) use progress::ProgressState;
use progress::Snapshot;
pub use progress::StorageCompactionProgress;

#[cfg(test)]
mod tests;

const OCR_CURSOR: &str = "storage_ocr_marker_cursor_v1";
const OCR_DONE: &str = "storage_ocr_marker_done_v1";
const OCR_QUEUE_VERSION: &str = "storage_ocr_queue_version";
const PROGRESS_EVENT: &str = "storage-compaction-progress";
const PROGRESS_INTERVAL: Duration = Duration::from_secs(1);
const LOG_INTERVAL: Duration = Duration::from_secs(10);

/// Keep the legacy column name so old rows remain readable without rebuilding
/// the multi-gigabyte OCR table. Only emptiness is part of the reader contract.
pub(super) const OCR_INDEXED: &str = "1";

/// A representation-only update must not invalidate source revisions or
/// expensive derived caches. Only the named notification triggers are parked;
/// referential, queue and format guards continue running. DDL, data and trigger
/// restoration commit atomically, including after a crash or failed checkpoint.
pub(super) fn representation_transaction<T>(
    conn: &Connection,
    triggers: &[&str],
    operation: impl FnOnce(&Connection) -> Result<T, String>,
) -> Result<T, String> {
    let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
    let mut definitions = Vec::new();
    for name in triggers {
        let definition: Option<String> = tx
            .query_row(
                "SELECT sql FROM sqlite_master WHERE type='trigger' AND name=?1",
                [name],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        if let Some(sql) = definition {
            tx.execute_batch(&format!("DROP TRIGGER {name};"))
                .map_err(|e| e.to_string())?;
            definitions.push(sql);
        }
    }
    let result = operation(&tx)?;
    for definition in definitions {
        tx.execute_batch(&definition).map_err(|e| e.to_string())?;
    }
    tx.commit().map_err(|e| e.to_string())?;
    Ok(result)
}

pub(super) fn initialize(conn: &Connection) -> Result<(), String> {
    let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS ocr_index_pending (
            ocr_id INTEGER PRIMARY KEY REFERENCES ocr_results(id) ON DELETE CASCADE
        );
        CREATE TRIGGER IF NOT EXISTS ocr_index_pending_insert AFTER INSERT ON ocr_results
        WHEN NEW.text_hash='' AND NEW.is_deleted=0 BEGIN
            INSERT OR IGNORE INTO ocr_index_pending VALUES(NEW.id);
        END;
        CREATE TRIGGER IF NOT EXISTS ocr_index_pending_update
        AFTER UPDATE OF id,text_hash,is_deleted ON ocr_results
        WHEN OLD.id!=NEW.id OR
            (OLD.text_hash='' AND OLD.is_deleted=0)!=(NEW.text_hash='' AND NEW.is_deleted=0)
        BEGIN
            DELETE FROM ocr_index_pending WHERE ocr_id=OLD.id;
            INSERT OR IGNORE INTO ocr_index_pending
                SELECT NEW.id WHERE NEW.text_hash='' AND NEW.is_deleted=0;
        END;
        CREATE TRIGGER IF NOT EXISTS ocr_index_pending_delete AFTER DELETE ON ocr_results BEGIN
            DELETE FROM ocr_index_pending WHERE ocr_id=OLD.id;
        END;",
    )
    .map_err(|e| e.to_string())?;
    let version: Option<String> = tx
        .query_row(
            "SELECT value FROM app_metadata WHERE key=?1",
            [OCR_QUEUE_VERSION],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    match version.as_deref() {
        None => {
            // The legacy hash index locates the usually tiny backlog without
            // scanning every OCR body. Fresh databases have no OCR rows yet.
            tx.execute(
                "INSERT OR IGNORE INTO ocr_index_pending
                    SELECT id FROM ocr_results WHERE text_hash='' AND is_deleted=0",
                [],
            )
            .map_err(|e| e.to_string())?;
            tx.execute(
                "INSERT INTO app_metadata(key,value) VALUES(?1,'1')",
                [OCR_QUEUE_VERSION],
            )
            .map_err(|e| e.to_string())?;
        }
        Some("1") => {}
        Some(_) => return Err("Unsupported OCR index queue format".into()),
    }
    tx.execute_batch("DROP INDEX IF EXISTS idx_text_hash;")
        .map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())
}

/// No OCR decryption or search-index rebuild is needed. Every existing
/// nonempty value already means indexed, including during HMAC migration.
fn compact_ocr_markers(conn: &Connection, limit: usize) -> Result<usize, String> {
    let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
    let done: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM app_metadata WHERE key=?1 AND value='1')",
            [OCR_DONE],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    if done {
        return Ok(0);
    }
    let cursor: i64 = tx
        .query_row(
            "SELECT CAST(value AS INTEGER) FROM app_metadata WHERE key=?1",
            [OCR_CURSOR],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?
        .unwrap_or(0);
    let ids = {
        let mut stmt = tx
            .prepare("SELECT id FROM ocr_results WHERE id>?1 ORDER BY id LIMIT ?2")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(params![cursor, limit.clamp(1, 2048) as i64], |r| {
                r.get::<_, i64>(0)
            })
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?
    };
    if let Some(last) = ids.last() {
        tx.execute(
            "UPDATE ocr_results SET text_hash=?3
                WHERE id>?1 AND id<=?2 AND length(text_hash)>1",
            params![cursor, last, OCR_INDEXED],
        )
        .map_err(|e| e.to_string())?;
        tx.execute(
            "INSERT INTO app_metadata(key,value) VALUES(?1,?2)
                ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            params![OCR_CURSOR, last.to_string()],
        )
        .map_err(|e| e.to_string())?;
    } else {
        tx.execute(
            "INSERT INTO app_metadata(key,value) VALUES(?1,'1')
                ON CONFLICT(key) DO UPDATE SET value='1'",
            [OCR_DONE],
        )
        .map_err(|e| e.to_string())?;
        tx.execute("DELETE FROM app_metadata WHERE key=?1", [OCR_CURSOR])
            .map_err(|e| e.to_string())?;
    }
    tx.commit().map_err(|e| e.to_string())?;
    Ok(ids.len())
}

/// Counts visits rather than successful rewrites: unchanged, preserved and
/// racing rows can be visited again. Vector work also includes block GC.
#[derive(Debug, Default, Clone, Copy)]
struct BatchWork {
    marker_rows_scanned: u64,
    vector_work_units: u64,
    icon_rows_scanned: u64,
    ocr_key_rows_scanned: u64,
    marker_time: Duration,
    vector_time: Duration,
    icon_time: Duration,
    ocr_key_time: Duration,
}

impl BatchWork {
    fn total(&self) -> u64 {
        self.marker_rows_scanned
            + self.vector_work_units
            + self.icon_rows_scanned
            + self.ocr_key_rows_scanned
    }

    fn add(&mut self, batch: Self) {
        self.marker_rows_scanned += batch.marker_rows_scanned;
        self.vector_work_units += batch.vector_work_units;
        self.icon_rows_scanned += batch.icon_rows_scanned;
        self.ocr_key_rows_scanned += batch.ocr_key_rows_scanned;
        self.marker_time += batch.marker_time;
        self.vector_time += batch.vector_time;
        self.icon_time += batch.icon_time;
        self.ocr_key_time += batch.ocr_key_time;
    }
}

struct Diagnostics {
    generation: u64,
    snapshot: Snapshot,
    started: Instant,
    last_progress: Instant,
    last_report: Instant,
    work: BatchWork,
    batches: u64,
    batch_time: Duration,
    throttle_time: Duration,
    active: bool,
}

impl Diagnostics {
    fn new(generation: u64, snapshot: Snapshot) -> Self {
        Self {
            generation,
            snapshot,
            started: Instant::now(),
            last_progress: Instant::now(),
            last_report: Instant::now(),
            work: BatchWork::default(),
            batches: 0,
            batch_time: Duration::ZERO,
            throttle_time: Duration::ZERO,
            active: snapshot.pending(),
        }
    }

    fn report(&mut self, message: &'static str) {
        tracing::info!(
            generation = self.generation,
            batches = self.batches,
            elapsed_ms = self.started.elapsed().as_millis() as u64,
            batch_time_ms = self.batch_time.as_millis() as u64,
            throttle_time_ms = self.throttle_time.as_millis() as u64,
            work = ?self.work,
            estimated_percent = ?self.snapshot.estimated_percent(),
            checkpoints = ?self.snapshot.checkpoints,
            max_ids = ?self.snapshot.max_ids,
            pending = self.snapshot.pending(),
            free_pages = self.snapshot.free_pages,
            "[DB:COMPACT] {message}"
        );
        self.last_report = Instant::now();
    }
}

impl StorageState {
    pub(crate) fn start_storage_compaction(self: &Arc<Self>, app: AppHandle) {
        if self.storage_compaction_started.swap(true, Ordering::AcqRel) {
            return;
        }
        let storage = Arc::clone(self);
        std::thread::spawn(move || {
            let mut diagnostics: Option<Diagnostics> = None;
            while !crate::IS_QUITTING.load(Ordering::Relaxed)
                && !crate::IS_UPDATING.load(Ordering::Relaxed)
            {
                if !storage.is_initialized()
                    || storage.is_database_maintenance_pending()
                    || crate::maintenance::is_active()
                {
                    let progress = storage.storage_compaction_progress();
                    if progress.phase != "idle" {
                        storage.publish_compaction_progress(
                            &app,
                            storage.db_generation(),
                            "waiting_for_maintenance",
                            progress.percent,
                        );
                    }
                    std::thread::sleep(Duration::from_secs(1));
                    continue;
                }

                if diagnostics
                    .as_ref()
                    .is_none_or(|state| state.generation != storage.db_generation())
                {
                    match storage.read_compaction_snapshot() {
                        Ok((generation, snapshot)) => {
                            let mut state = Diagnostics::new(generation, snapshot);
                            state.report("Loaded restartable scan checkpoints");
                            diagnostics = Some(state);
                        }
                        Err(error) => {
                            storage.publish_compaction_progress(
                                &app,
                                storage.db_generation(),
                                "retry_wait",
                                None,
                            );
                            tracing::warn!("[DB:COMPACT] Checkpoint read deferred: {error}");
                            std::thread::sleep(Duration::from_secs(30));
                            continue;
                        }
                    }
                }
                let state = diagnostics.as_mut().expect("diagnostics initialized");
                let generation = state.generation;
                // Continue reporting activity while queue/GC work is making
                // progress. Only a zero-work batch establishes an auth wait.
                if state.snapshot.pending() && !storage.storage_compaction_progress().running {
                    storage.publish_compaction_progress(
                        &app,
                        generation,
                        state.snapshot.phase(storage.silent_read_wait_reason()),
                        state.snapshot.estimated_percent(),
                    );
                }

                let waiting = Instant::now();
                let Some(rewrite_permit) = storage
                    .rewrite_budget
                    .acquire(&storage.foreground_db_activity)
                else {
                    break;
                };
                state.throttle_time += waiting.elapsed();
                // Maintenance or a database swap may have begun during the
                // cooldown. Recheck before entering any rewrite transaction.
                if storage.db_generation() != generation
                    || !storage.is_initialized()
                    || storage.is_database_maintenance_pending()
                    || crate::maintenance::is_active()
                {
                    continue;
                }
                let started = Instant::now();
                let mut work = BatchWork::default();
                let mut result = storage.compact_storage_batch(&mut work);
                // Release the shared slot after every database guard has left
                // scope. The next worker pays the cooldown without DB locks.
                drop(rewrite_permit);
                // A backup restore or directory migration invalidates both
                // counters and IDs. Never publish a result for the old file.
                if storage.db_generation() != generation {
                    continue;
                }
                state.work.add(work);
                state.batches += 1;
                state.batch_time += started.elapsed();
                if work.total() > 0 {
                    state.active = true;
                }
                if work.total() == 0
                    || result.is_err()
                    || state.last_progress.elapsed() >= PROGRESS_INTERVAL
                {
                    match storage.read_compaction_snapshot() {
                        Ok((current, snapshot)) if current == generation => {
                            state.snapshot = snapshot;
                            state.last_progress = Instant::now();
                        }
                        Ok(_) => continue,
                        Err(error) if result.is_ok() => {
                            result = Err(format!("checkpoint read: {error}"));
                        }
                        Err(_) => {}
                    }
                }
                if let Err(error) = result {
                    storage.publish_compaction_progress(
                        &app,
                        generation,
                        "retry_wait",
                        state.snapshot.estimated_percent(),
                    );
                    // Earlier stages may have committed before this failure;
                    // their visits remain included in the diagnostic totals.
                    tracing::warn!("[DB:COMPACT] Background rewrite deferred: {error}");
                    state.report("Partial batch preserved; retrying in 30 seconds");
                    std::thread::sleep(Duration::from_secs(30));
                    continue;
                }
                if work.total() == 0 {
                    if state.snapshot.pending() {
                        storage.publish_compaction_progress(
                            &app,
                            generation,
                            storage
                                .silent_read_wait_reason()
                                .unwrap_or("waiting_for_verification"),
                            state.snapshot.estimated_percent(),
                        );
                    } else {
                        if state.active {
                            state.report("Background scan completed");
                            state.active = false;
                        }
                        storage.publish_compaction_progress(
                            &app,
                            generation,
                            "idle",
                            state.snapshot.estimated_percent(),
                        );
                    }
                    if state.active && state.last_report.elapsed() >= LOG_INTERVAL {
                        state.report("Background scan waiting");
                    }
                    std::thread::sleep(Duration::from_secs(5));
                } else {
                    storage.publish_compaction_progress(
                        &app,
                        generation,
                        "running",
                        state
                            .snapshot
                            .pending()
                            .then(|| state.snapshot.estimated_percent())
                            .flatten(),
                    );
                    if state.last_report.elapsed() >= LOG_INTERVAL {
                        state.report("Background scan progress");
                    }
                }
            }
            if let Some(mut state) = diagnostics {
                state.report("Background rewrite stopped");
                storage.publish_compaction_progress(&app, state.generation, "idle", None);
            }
        });
    }

    fn read_compaction_snapshot(&self) -> Result<(u64, Snapshot), String> {
        let guard = self.get_connection_named("storage_compaction_checkpoint")?;
        let conn = guard.as_ref().ok_or("Database not initialized")?;
        Ok((self.db_generation(), Snapshot::read(conn)?))
    }

    fn publish_compaction_progress(
        &self,
        app: &AppHandle,
        generation: u64,
        phase: &'static str,
        percent: Option<f64>,
    ) {
        let mut state = self
            .storage_compaction_progress
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if self.db_generation() != generation {
            return;
        }
        let previous_phase = state.snapshot().phase;
        if let Some(progress) = state.update(generation, phase, percent) {
            if previous_phase != phase {
                tracing::info!(
                    generation,
                    phase,
                    estimated_percent = ?percent,
                    "[DB:COMPACT] Background rewrite state changed"
                );
            }
            // Emit after releasing the progress lock. UI polling reads only
            // this cache and never waits for the rewrite's database lock.
            drop(state);
            let _ = app.emit(PROGRESS_EVENT, progress);
        }
    }

    fn compact_storage_batch(&self, work: &mut BatchWork) -> Result<(), String> {
        {
            let guard = self.get_connection_named("storage_compaction")?;
            let conn = guard.as_ref().ok_or("Database not initialized")?;
            let started = Instant::now();
            let result = compact_ocr_markers(conn, 256);
            work.marker_time = started.elapsed();
            work.marker_rows_scanned =
                result.map_err(|error| format!("OCR markers: {error}"))? as u64;
            let started = Instant::now();
            let result = super::vector_blocks::compact(conn);
            work.vector_time = started.elapsed();
            work.vector_work_units =
                result.map_err(|error| format!("vector blocks: {error}"))? as u64;
        }
        let started = Instant::now();
        let result = self.compact_process_icons_batch();
        work.icon_time = started.elapsed();
        work.icon_rows_scanned = result.map_err(|error| format!("process icons: {error}"))? as u64;
        let started = Instant::now();
        let result = self.compact_ocr_keys_batch();
        work.ocr_key_time = started.elapsed();
        work.ocr_key_rows_scanned = result.map_err(|error| format!("OCR keys: {error}"))? as u64;
        Ok(())
    }
}
