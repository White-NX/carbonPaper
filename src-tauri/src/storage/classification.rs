//! Native anchors, durable user feedback, and fenced classification commits.
//! The legacy anchor file is read once and retained. Feedback stores references
//! to encrypted archive inputs, never copies of screenshot text.

use super::StorageState;
use crate::classification::scoring::{import_anchors, validate_anchors, Anchors};
use rusqlite::{params, Connection, OptionalExtension};

pub(crate) struct AnchorSnapshot {
    pub generation: u64,
    pub revision: i64,
    pub anchors: Anchors,
}

#[derive(Clone, Debug)]
pub(crate) struct ClassificationLease {
    pub generation: u64,
    pub screenshot_id: i64,
    pub source_revision: i64,
    pub user_revision: i64,
    pub token: String,
}

#[derive(Clone, Debug)]
pub(crate) struct ClassificationFeedback {
    pub id: i64,
    pub screenshot_id: i64,
    pub category: String,
    pub old_category: Option<String>,
    pub source_revision: i64,
    pub attempts: i64,
}

fn anchors_on_conn(conn: &Connection, generation: u64) -> Result<Option<AnchorSnapshot>, String> {
    let row: Option<(i64, i64, String)> = conn
        .query_row(
            "SELECT schema_version,revision,anchors_json FROM classification_anchors WHERE id=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    row.map(|(version, revision, json)| {
        if version != 1 {
            return Err("unsupported classification anchor schema".into());
        }
        let anchors: Anchors =
            serde_json::from_str(&json).map_err(|e| format!("invalid stored anchors: {e}"))?;
        validate_anchors(&anchors)?;
        Ok(AnchorSnapshot {
            generation,
            revision,
            anchors,
        })
    })
    .transpose()
}

fn write_anchors(conn: &Connection, revision: i64, anchors: &Anchors) -> Result<i64, String> {
    validate_anchors(anchors)?;
    let next = revision.checked_add(1).ok_or("anchor revision overflow")?;
    let json = serde_json::to_string(anchors).map_err(|e| e.to_string())?;
    let changed=conn.execute("UPDATE classification_anchors SET anchors_json=?1,revision=?2 WHERE id=1 AND revision=?3 AND schema_version=1",params![json,next,revision]).map_err(|e|e.to_string())?;
    if changed != 1 {
        return Err("classification anchors changed; retry the operation".into());
    }
    Ok(next)
}

fn revisions(conn: &Connection, id: i64) -> Result<Option<(i64, i64)>, String> {
    conn.query_row(
        "SELECT COALESCE(r.revision,0),COALESCE(u.revision,0) FROM screenshots s
        LEFT JOIN screenshot_processing_revisions r ON r.screenshot_id=s.id
        LEFT JOIN classification_user_revisions u ON u.screenshot_id=s.id
        WHERE s.id=?1 AND s.is_deleted=0",
        [id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .optional()
    .map_err(|e| e.to_string())
}

impl StorageState {
    pub(super) fn init_classification_schema(&self, conn: &Connection) -> Result<(), String> {
        conn.execute_batch("CREATE TABLE IF NOT EXISTS classification_anchors(
                id INTEGER PRIMARY KEY CHECK(id=1),schema_version INTEGER NOT NULL,
                revision INTEGER NOT NULL,anchors_json TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS classification_user_revisions(
                screenshot_id INTEGER PRIMARY KEY REFERENCES screenshots(id) ON DELETE CASCADE,
                revision INTEGER NOT NULL DEFAULT 0);
            CREATE TABLE IF NOT EXISTS classification_feedback(
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                screenshot_id INTEGER NOT NULL REFERENCES screenshots(id) ON DELETE CASCADE,
                category TEXT NOT NULL,old_category TEXT,source_revision INTEGER NOT NULL,
                attempts INTEGER NOT NULL DEFAULT 0,next_retry_at INTEGER NOT NULL DEFAULT 0,last_error TEXT);
            CREATE INDEX IF NOT EXISTS idx_classification_feedback_due ON classification_feedback(next_retry_at,id);").map_err(|e|e.to_string())?;
        Ok(())
    }

    pub(crate) fn load_classification_anchors(&self) -> Result<AnchorSnapshot, String> {
        let _activity = self.foreground_db_read();
        let generation = self.db_generation();
        {
            let guard = self.get_connection_named("load_classification_anchors")?;
            if let Some(snapshot) = anchors_on_conn(
                guard.as_ref().ok_or("Database not initialized")?,
                generation,
            )? {
                return Ok(snapshot);
            }
        }
        let path = self
            .data_dir
            .lock()
            .map_err(|_| "data directory lock poisoned")?
            .join("anchors.json");
        let contents = match std::fs::metadata(&path) {
            Ok(meta) => {
                if meta.len() > 16 * 1024 * 1024 {
                    return Err("legacy anchors.json exceeds the import limit".into());
                }
                Some(
                    std::fs::read_to_string(&path)
                        .map_err(|e| format!("cannot read legacy anchors: {e}"))?,
                )
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(format!("cannot inspect legacy anchors: {error}")),
        };
        let anchors = import_anchors(contents.as_deref())?;
        let json = serde_json::to_string(&anchors).map_err(|e| e.to_string())?;
        let guard = self.get_connection_named("import_classification_anchors")?;
        let conn = guard.as_ref().ok_or("Database not initialized")?;
        if self.db_generation() != generation {
            return Err("database changed during anchor import".into());
        }
        conn.execute("INSERT OR IGNORE INTO classification_anchors(id,schema_version,revision,anchors_json) VALUES(1,1,1,?1)",[json]).map_err(|e|e.to_string())?;
        anchors_on_conn(conn, generation)?.ok_or_else(|| "anchor import did not commit".into())
    }

    pub(crate) fn save_classification_anchors(
        &self,
        generation: u64,
        revision: i64,
        anchors: &Anchors,
    ) -> Result<i64, String> {
        let guard = self.get_connection_named("save_classification_anchors")?;
        if self.db_generation() != generation {
            return Err("database changed".into());
        }
        write_anchors(
            guard.as_ref().ok_or("Database not initialized")?,
            revision,
            anchors,
        )
    }

    /// A user correction and its learning intent commit together. Loading BGE
    /// can fail or be deferred without losing the correction or its old category.
    pub(crate) fn update_category_with_feedback(
        &self,
        id: i64,
        category: &str,
    ) -> Result<bool, String> {
        if category.trim().is_empty() || category.len() > 256 {
            return Err("invalid category".into());
        }
        let mut guard = self.get_connection_named("update_category_with_feedback")?;
        let conn = guard.as_mut().ok_or("Database not initialized")?;
        let tx = conn.transaction().map_err(|e| e.to_string())?;
        let Some((source, _)) = revisions(&tx, id)? else {
            return Ok(false);
        };
        let old: Option<String> = tx
            .query_row("SELECT category FROM screenshots WHERE id=?1", [id], |r| {
                r.get(0)
            })
            .map_err(|e| e.to_string())?;
        tx.execute(
            "UPDATE screenshots SET category=?2,category_confidence=1.0 WHERE id=?1",
            params![id, category],
        )
        .map_err(|e| e.to_string())?;
        tx.execute(
            "INSERT INTO classification_user_revisions(screenshot_id,revision) VALUES(?1,1)
            ON CONFLICT(screenshot_id) DO UPDATE SET revision=revision+1",
            [id],
        )
        .map_err(|e| e.to_string())?;
        tx.execute("INSERT INTO classification_feedback(screenshot_id,category,old_category,source_revision) VALUES(?1,?2,?3,?4)",params![id,category,old,source]).map_err(|e|e.to_string())?;
        tx.commit().map_err(|e| e.to_string())?;
        Ok(true)
    }

    pub(crate) fn next_classification_feedback(
        &self,
    ) -> Result<Option<ClassificationFeedback>, String> {
        let guard = self.get_connection_named("next_classification_feedback")?;
        let conn = guard.as_ref().ok_or("Database not initialized")?;
        conn.execute("DELETE FROM classification_feedback WHERE NOT EXISTS(
            SELECT 1 FROM screenshots s LEFT JOIN screenshot_processing_revisions r ON r.screenshot_id=s.id
            WHERE s.id=classification_feedback.screenshot_id AND s.is_deleted=0
            AND COALESCE(r.revision,0)=classification_feedback.source_revision)",[]).map_err(|e|e.to_string())?;
        // Preserve feedback ordering even while an earlier correction backs off.
        conn.query_row("SELECT id,screenshot_id,category,old_category,source_revision,attempts
            FROM classification_feedback WHERE id=(SELECT MIN(id) FROM classification_feedback WHERE attempts<5)
            AND next_retry_at<=?1",[chrono::Utc::now().timestamp()],|r|Ok(ClassificationFeedback{
                id:r.get(0)?,screenshot_id:r.get(1)?,category:r.get(2)?,old_category:r.get(3)?,source_revision:r.get(4)?,attempts:r.get(5)?,
            })).optional().map_err(|e|e.to_string())
    }

    pub(crate) fn commit_classification_feedback(
        &self,
        generation: u64,
        revision: i64,
        anchors: &Anchors,
        feedback: &ClassificationFeedback,
    ) -> Result<i64, String> {
        let mut guard = self.get_connection_named("commit_classification_feedback")?;
        let conn = guard.as_mut().ok_or("Database not initialized")?;
        if self.db_generation() != generation {
            return Err("database changed".into());
        }
        let tx = conn.transaction().map_err(|e| e.to_string())?;
        if revisions(&tx, feedback.screenshot_id)?.map(|r| r.0) != Some(feedback.source_revision) {
            return Err("feedback source changed".into());
        }
        let exists:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM classification_feedback WHERE id=?1 AND screenshot_id=?2)",params![feedback.id,feedback.screenshot_id],|r|r.get(0)).map_err(|e|e.to_string())?;
        if !exists {
            return Err("feedback already completed".into());
        }
        let next = write_anchors(&tx, revision, anchors)?;
        tx.execute(
            "DELETE FROM classification_feedback WHERE id=?1",
            [feedback.id],
        )
        .map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())?;
        Ok(next)
    }

    pub(crate) fn defer_classification_feedback(
        &self,
        generation: u64,
        feedback: &ClassificationFeedback,
        error: &str,
        deferred: bool,
    ) -> Result<(), String> {
        let guard = self.get_connection_named("defer_classification_feedback")?;
        if self.db_generation() != generation {
            return Err("database changed".into());
        }
        let attempts = feedback.attempts + i64::from(!deferred);
        let delay = 30 * (1i64 << attempts.clamp(0, 5));
        guard.as_ref().ok_or("Database not initialized")?.execute("UPDATE classification_feedback SET attempts=?2,next_retry_at=?3,last_error=?4 WHERE id=?1",
            params![feedback.id,attempts,chrono::Utc::now().timestamp()+delay,error.chars().take(500).collect::<String>()]).map_err(|e|e.to_string())?;
        Ok(())
    }

    pub(crate) fn classification_user_revision(&self, id: i64) -> Result<i64, String> {
        let guard = self.get_connection_named("classification_user_revision")?;
        revisions(guard.as_ref().ok_or("Database not initialized")?, id)?
            .map(|r| r.1)
            .ok_or_else(|| "screenshot is no longer active".into())
    }

    pub(crate) fn claim_native_classification(
        &self,
        generation: u64,
        id: i64,
    ) -> Result<Option<ClassificationLease>, String> {
        let guard = self.get_connection_named("claim_native_classification")?;
        let conn = guard.as_ref().ok_or("Database not initialized")?;
        if self.db_generation() != generation {
            return Err("database changed".into());
        }
        let Some((source_revision, user_revision)) = revisions(conn, id)? else {
            return Ok(None);
        };
        let token = carbonpaper_app_bound::protocol::random_id();
        let changed = conn
            .execute(
                "UPDATE screenshot_ocr_status SET postprocess_status='queued',postprocess_lease=?2,
            postprocess_error=NULL,updated_at=CURRENT_TIMESTAMP WHERE screenshot_id=?1
            AND postprocess_status IN ('pending','waiting_for_auth') AND postprocess_attempts<5
            AND (postprocess_next_retry_at IS NULL OR postprocess_next_retry_at<=CURRENT_TIMESTAMP)",
                params![id, token],
            )
            .map_err(|e| e.to_string())?;
        Ok((changed == 1).then_some(ClassificationLease {
            generation,
            screenshot_id: id,
            source_revision,
            user_revision,
            token,
        }))
    }

    pub(crate) fn start_native_classification(
        &self,
        lease: &ClassificationLease,
    ) -> Result<bool, String> {
        let guard = self.get_connection_named("start_native_classification")?;
        let conn = guard.as_ref().ok_or("Database not initialized")?;
        if self.db_generation() != lease.generation {
            return Ok(false);
        }
        if revisions(conn, lease.screenshot_id)?.map(|r| r.0) != Some(lease.source_revision) {
            return Ok(false);
        }
        conn.execute("UPDATE screenshot_ocr_status SET postprocess_status='processing' WHERE screenshot_id=?1
            AND postprocess_lease=?2 AND postprocess_status='queued'",params![lease.screenshot_id,lease.token]).map(|n|n==1).map_err(|e|e.to_string())
    }

    pub(crate) fn commit_native_classification(
        &self,
        lease: &ClassificationLease,
        category: Option<&str>,
        score: Option<f64>,
    ) -> Result<bool, String> {
        if category.is_some_and(|c| c.len() > 256)
            || score.is_some_and(|s| !s.is_finite() || s < 0.0)
        {
            return Err("invalid classification result".into());
        }
        let mut guard = self.get_connection_named("commit_native_classification")?;
        let conn = guard.as_mut().ok_or("Database not initialized")?;
        if self.db_generation() != lease.generation {
            return Err("database changed".into());
        }
        let tx = conn.transaction().map_err(|e| e.to_string())?;
        let Some((source, user)) = revisions(&tx, lease.screenshot_id)? else {
            return Ok(false);
        };
        if source != lease.source_revision {
            return Err("classification source changed".into());
        }
        let owned: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM screenshot_ocr_status WHERE screenshot_id=?1
            AND postprocess_lease=?2 AND postprocess_status IN ('queued','processing'))",
                params![lease.screenshot_id, lease.token],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())?;
        if !owned {
            return Ok(false);
        }
        if user == 0 && user == lease.user_revision {
            if let Some(category) = category {
                tx.execute("UPDATE screenshots SET category=?2,category_confidence=?3 WHERE id=?1 AND is_deleted=0",params![lease.screenshot_id,category,score]).map_err(|e|e.to_string())?;
            }
        }
        tx.execute("UPDATE screenshot_ocr_status SET postprocess_status='completed',postprocess_lease=NULL,
            postprocess_error=NULL,postprocess_attempts=0,postprocess_next_retry_at=NULL WHERE screenshot_id=?1",[lease.screenshot_id]).map_err(|e|e.to_string())?;
        tx.commit().map_err(|e| e.to_string())?;
        Ok(true)
    }

    pub(crate) fn defer_native_classification(
        &self,
        lease: &ClassificationLease,
        error: &str,
        deferred: bool,
    ) -> Result<(), String> {
        let guard = self.get_connection_named("defer_native_classification")?;
        let conn = guard.as_ref().ok_or("Database not initialized")?;
        if self.db_generation() != lease.generation {
            return Ok(());
        }
        let attempts:Option<i64>=conn.query_row("SELECT postprocess_attempts FROM screenshot_ocr_status WHERE screenshot_id=?1 AND postprocess_lease=?2",
            params![lease.screenshot_id,lease.token],|r|r.get(0)).optional().map_err(|e|e.to_string())?;
        let Some(attempts) = attempts else {
            return Ok(());
        };
        let (status, delay, attempts) = if deferred {
            ("pending", Some(30), attempts)
        } else {
            super::screenshot::ocr_postprocess_retry_decision(attempts)
        };
        conn.execute("UPDATE screenshot_ocr_status SET postprocess_status=?3,postprocess_lease=NULL,postprocess_error=?4,
            postprocess_attempts=?5,postprocess_next_retry_at=CASE WHEN ?6 IS NULL THEN NULL ELSE datetime('now',?6) END
            WHERE screenshot_id=?1 AND postprocess_lease=?2",params![lease.screenshot_id,lease.token,status,
            error.chars().take(500).collect::<String>(),attempts,delay.map(|d|format!("+{d} seconds"))]).map_err(|e|e.to_string())?;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
