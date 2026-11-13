use super::*;

pub(super) const EXPIRE_RECAP_SQL: &str = "UPDATE recap_objects SET state='retired',key_blob=NULL
    WHERE owner=?1 AND state!='retired'
    AND (expires<=?2 OR (state='prepared' AND prepared_until<=?2))";
pub(super) const PRUNE_RECAP_SQL: &str = "DELETE FROM recap_objects
    WHERE owner=?1 AND state='retired' AND created<?2";
const EXACT_SOURCE_REVOCATIONS: i64 = 4096;
const MAX_SOURCE_REVOCATIONS: i64 = 100_000;

impl Ledger {
    pub(super) fn init_recap(conn: &Connection) -> Result<()> {
        db(conn.execute_batch("CREATE TABLE IF NOT EXISTS recap_objects (
            id TEXT PRIMARY KEY,owner TEXT NOT NULL REFERENCES owners(sid),dataset TEXT NOT NULL,
            slot TEXT NOT NULL,binding TEXT NOT NULL,state TEXT NOT NULL,
            key_blob BLOB,digest TEXT,created INTEGER NOT NULL,expires INTEGER NOT NULL,
            prepared_until INTEGER NOT NULL,bytes INTEGER NOT NULL);
            CREATE INDEX IF NOT EXISTS recap_objects_slot ON recap_objects(owner,dataset,slot,state);
            CREATE INDEX IF NOT EXISTS recap_objects_live
                ON recap_objects(owner,state,expires,prepared_until,bytes,created) WHERE state!='retired';
            CREATE INDEX IF NOT EXISTS recap_objects_retired_created
                ON recap_objects(owner,created) WHERE state='retired';
            CREATE TABLE IF NOT EXISTS recap_sources (
                object_id TEXT NOT NULL REFERENCES recap_objects(id) ON DELETE CASCADE,
                screenshot_id INTEGER NOT NULL,PRIMARY KEY(object_id,screenshot_id));
            CREATE INDEX IF NOT EXISTS recap_source_lookup ON recap_sources(screenshot_id,object_id);
            CREATE TABLE IF NOT EXISTS recap_revoked_sources (
                owner TEXT NOT NULL REFERENCES owners(sid),dataset TEXT NOT NULL,screenshot_id INTEGER NOT NULL,
                PRIMARY KEY(owner,dataset,screenshot_id));
            CREATE TABLE IF NOT EXISTS recap_revoked_ranges (
                owner TEXT NOT NULL REFERENCES owners(sid),dataset TEXT NOT NULL,
                first_id INTEGER NOT NULL,last_id INTEGER NOT NULL,PRIMARY KEY(owner,dataset));
            CREATE TABLE IF NOT EXISTS recap_revocation_budgets (
                owner TEXT PRIMARY KEY REFERENCES owners(sid),entries INTEGER NOT NULL,deny_all INTEGER NOT NULL DEFAULT 0);
            INSERT OR IGNORE INTO recap_revocation_budgets(owner,entries)
                SELECT owner,COUNT(*) FROM (SELECT owner FROM recap_revoked_sources UNION ALL SELECT owner FROM recap_revoked_ranges) GROUP BY owner;"))
    }

    pub(super) fn dispatch_recap(
        conn: &Connection,
        who: &Principal,
        request: &Request,
        now: i64,
        enabled: bool,
        limits: Limits,
        dataset: Option<&str>,
        protector: &dyn KeyProtector,
    ) -> Result<Option<Response>> {
        if let Request::RevokeRecapObjects { object_ids } = request {
            if object_ids.len() > 128 || object_ids.iter().any(|id| !valid_id(id)) {
                return Err(BrokerError::InvalidRequest);
            }
            for id in object_ids {
                db(conn.execute("UPDATE recap_objects SET state='retired',key_blob=NULL WHERE id=?1 AND owner=?2",params![id,who.sid]))?;
            }
            return Ok(Some(Response::Ok));
        }
        if matches!(request, Request::RevokeRecap {}) {
            Self::retire_recap(conn, &who.sid, dataset)?;
            let tasks = {
                let mut stmt =
                    db(conn.prepare("SELECT id FROM tasks WHERE owner=?1 AND state!='retired'"))?;
                let rows = db(stmt.query_map([&who.sid], |r| r.get::<_, String>(0)))?;
                db(rows.collect::<rusqlite::Result<Vec<_>>>())?
            };
            for id in tasks {
                let task = Self::task(conn, who, &id, dataset)?;
                if task.binding.consumers & Consumer::Recap.bit() != 0
                    && task.done & Consumer::Recap.bit() == 0
                {
                    let done = task.done | Consumer::Recap.bit();
                    db(conn.execute(
                        "UPDATE tasks SET done=?2,abandoned=abandoned|?3,
                        state=CASE WHEN (?2 & ?4)=?4 THEN 'retired' ELSE state END,
                        key_blob=CASE WHEN (?2 & ?4)=?4 THEN NULL ELSE key_blob END WHERE id=?1",
                        params![id, done, Consumer::Recap.bit(), task.binding.consumers],
                    ))?;
                    db(conn.execute(
                        "DELETE FROM leases WHERE task=?1 AND consumer=?2",
                        params![id, Consumer::Recap.bit()],
                    ))?;
                }
            }
            return Ok(Some(Response::Ok));
        }
        if let Request::RevokeRecapDay { date } = request {
            if date.len() != 10
                || !date.bytes().enumerate().all(|(i, b)| {
                    if i == 4 || i == 7 {
                        b == b'-'
                    } else {
                        b.is_ascii_digit()
                    }
                })
            {
                return Err(BrokerError::InvalidRequest);
            }
            db(conn.execute("UPDATE recap_objects SET state='retired',key_blob=NULL WHERE owner=?1 AND dataset=?2 AND slot LIKE ?3",
                params![who.sid,dataset,format!("day:{date}:%")]))?;
            return Ok(Some(Response::Ok));
        }
        if !matches!(
            request,
            Request::PrepareRecapObject { .. }
                | Request::AddRecapSources { .. }
                | Request::ActivateRecapObject { .. }
                | Request::AcquireRecapObject { .. }
        ) {
            return Ok(None);
        }
        if !enabled {
            return Err(BrokerError::Disabled);
        }
        match request {
            Request::PrepareRecapObject {
                dataset_id,
                slot,
                payload_bytes,
            } => {
                if dataset != Some(dataset_id) {
                    return Err(BrokerError::DatasetMismatch);
                }
                if !valid_id(dataset_id)
                    || slot.is_empty()
                    || slot.len() > 256
                    || !(28..=MAX_INPUT_BYTES + 28).contains(payload_bytes)
                {
                    return Err(BrokerError::InvalidRequest);
                }
                Self::make_room(conn, &who.sid, *payload_bytes, limits.capacity_bytes)?;
                db(conn.execute("UPDATE recap_objects SET state='retired',key_blob=NULL WHERE owner=?1 AND dataset=?2 AND slot=?3 AND state='prepared'",
                    params![who.sid,dataset_id,slot]))?;
                let binding = RecapBinding {
                    object_id: random_id(),
                    dataset_id: dataset_id.clone(),
                    slot: slot.clone(),
                    payload_bytes: *payload_bytes,
                    // Credentials remain authorized until changed/disabled. Evidence
                    // and derived contexts obey the ordinary retention limit.
                    expires_at: if slot == "config" {
                        i64::MAX
                    } else {
                        now + i64::from(limits.retention_days) * DAY_SECS
                    },
                };
                let key = new_key();
                let sealed = protector.protect(&key)?;
                db(conn.execute("INSERT INTO recap_objects VALUES(?1,?2,?3,?4,?5,'prepared',?6,NULL,?7,?8,?9,?10)",
                    params![binding.object_id,who.sid,dataset_id,slot,serde_json::to_string(&binding).map_err(|_|BrokerError::Integrity)?,
                        sealed,now,binding.expires_at,now+PREPARE_TTL_SECS,*payload_bytes as i64]))?;
                Ok(Some(Response::RecapKey(RecapObjectKey { binding, key })))
            }
            Request::AddRecapSources {
                object_id,
                screenshot_ids,
            } => {
                if screenshot_ids.len() > 128 || screenshot_ids.iter().any(|id| *id <= 0) {
                    return Err(BrokerError::InvalidRequest);
                }
                let (_, state, _, _, _) = Self::recap_object(conn, who, object_id, dataset)?;
                if state != "prepared" {
                    return Err(BrokerError::Retired);
                }
                for id in screenshot_ids {
                    let revoked:bool=db(conn.query_row("SELECT
                        EXISTS(SELECT 1 FROM recap_revoked_sources WHERE owner=?1 AND dataset=?2 AND screenshot_id=?3)
                        OR EXISTS(SELECT 1 FROM recap_revoked_ranges WHERE owner=?1 AND dataset=?2 AND ?3 BETWEEN first_id AND last_id)
                        OR EXISTS(SELECT 1 FROM recap_revocation_budgets WHERE owner=?1 AND deny_all=1)",params![who.sid,dataset,id],|r|r.get(0)))?;
                    if revoked {
                        return Err(BrokerError::Retired);
                    }
                    db(conn.execute(
                        "INSERT OR IGNORE INTO recap_sources VALUES(?1,?2)",
                        params![object_id, id],
                    ))?;
                }
                Ok(Some(Response::Ok))
            }
            Request::ActivateRecapObject {
                object_id,
                ciphertext_digest,
            } => {
                if !valid_id(ciphertext_digest) {
                    return Err(BrokerError::InvalidRequest);
                }
                let (binding, state, _, digest, _) =
                    Self::recap_object(conn, who, object_id, dataset)?;
                if state == "active" && digest.as_deref() == Some(ciphertext_digest) {
                    return Ok(Some(Response::Ok));
                }
                if state != "prepared" {
                    return Err(BrokerError::Retired);
                }
                db(conn.execute("UPDATE recap_objects SET state='retired',key_blob=NULL WHERE owner=?1 AND dataset=?2 AND slot=?3 AND id!=?4",
                    params![who.sid,dataset,binding.slot,object_id]))?;
                db(conn.execute(
                    "UPDATE recap_objects SET state='active',digest=?2 WHERE id=?1",
                    params![object_id, ciphertext_digest],
                ))?;
                Ok(Some(Response::Ok))
            }
            Request::AcquireRecapObject {
                object_id,
                ciphertext_digest,
            } => {
                let (binding, state, key, digest, expires) =
                    Self::recap_object(conn, who, object_id, dataset)?;
                if state != "active" || expires <= now {
                    return Err(BrokerError::Retired);
                }
                if digest.as_deref() != Some(ciphertext_digest) {
                    return Err(BrokerError::Integrity);
                }
                let key = protector.unprotect(key.as_deref().ok_or(BrokerError::Retired)?)?;
                Ok(Some(Response::RecapKey(RecapObjectKey { binding, key })))
            }
            _ => unreachable!(),
        }
    }

    fn recap_object(
        conn: &Connection,
        who: &Principal,
        id: &str,
        dataset: Option<&str>,
    ) -> Result<(RecapBinding, String, Option<Vec<u8>>, Option<String>, i64)> {
        if !valid_id(id) {
            return Err(BrokerError::InvalidRequest);
        }
        let row:Option<(String,String,Option<Vec<u8>>,Option<String>,i64)>=db(conn.query_row(
            "SELECT binding,state,key_blob,digest,expires FROM recap_objects WHERE id=?1 AND owner=?2",
            params![id,who.sid],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional())?;
        let (binding, state, key, digest, expires) = row.ok_or(BrokerError::Retired)?;
        let binding: RecapBinding =
            serde_json::from_str(&binding).map_err(|_| BrokerError::Integrity)?;
        if dataset != Some(&binding.dataset_id) {
            return Err(BrokerError::DatasetMismatch);
        }
        Ok((binding, state, key, digest, expires))
    }

    pub(super) fn revoke_recap_source(
        conn: &Connection,
        sid: &str,
        dataset: &str,
        id: i64,
    ) -> Result<()> {
        db(conn.execute(
            "INSERT OR IGNORE INTO recap_revocation_budgets(owner,entries) VALUES(?1,0)",
            [sid],
        ))?;
        let (count, saturated): (i64, bool) = db(conn.query_row(
            "SELECT entries,deny_all FROM recap_revocation_budgets WHERE owner=?1",
            [sid],
            |row| Ok((row.get(0)?, row.get(1)?)),
        ))?;
        if saturated || count >= MAX_SOURCE_REVOCATIONS {
            // Keep deletion available at the authority cap. A permanent owner
            // fence conservatively retires recap grants instead of dropping
            // revocations and letting a restored source acquire another grant.
            db(conn.execute("UPDATE broker_metadata SET value=2 WHERE key='schema'", []))?;
            db(conn.execute(
                "UPDATE recap_revocation_budgets SET deny_all=1,entries=0 WHERE owner=?1",
                [sid],
            ))?;
            db(conn.execute("DELETE FROM recap_revoked_sources WHERE owner=?1", [sid]))?;
            db(conn.execute("DELETE FROM recap_revoked_ranges WHERE owner=?1", [sid]))?;
            Self::retire_recap(conn, sid, None)?;
            return Ok(());
        }
        let covered: bool = db(conn.query_row("SELECT EXISTS(SELECT 1 FROM recap_revoked_ranges WHERE owner=?1 AND dataset=?2 AND ?3 BETWEEN first_id AND last_id)", params![sid,dataset,id], |row| row.get(0)))?;
        if !covered {
            let inserted = db(conn.execute(
                "INSERT OR IGNORE INTO recap_revoked_sources VALUES(?1,?2,?3)",
                params![sid, dataset, id],
            ))?;
            db(conn.execute(
                "UPDATE recap_revocation_budgets SET entries=entries+?2 WHERE owner=?1",
                params![sid, inserted as i64],
            ))?;
            let exact: i64 = db(conn.query_row(
                "SELECT COUNT(*) FROM recap_revoked_sources WHERE owner=?1 AND dataset=?2",
                params![sid, dataset],
                |row| row.get(0),
            ))?;
            if exact >= EXACT_SOURCE_REVOCATIONS {
                // Original services understand exact rows only. Commit the
                // schema fence with compaction so a rollback cannot reopen
                // the ledger and silently ignore the conservative range.
                db(conn.execute("UPDATE broker_metadata SET value=2 WHERE key='schema'", []))?;
                let (first,last): (i64,i64) = db(conn.query_row("SELECT MIN(screenshot_id),MAX(screenshot_id) FROM recap_revoked_sources WHERE owner=?1 AND dataset=?2", params![sid,dataset], |row| Ok((row.get(0)?,row.get(1)?))))?;
                let existed: bool = db(conn.query_row("SELECT EXISTS(SELECT 1 FROM recap_revoked_ranges WHERE owner=?1 AND dataset=?2)", params![sid,dataset], |row|row.get(0)))?;
                db(conn.execute("INSERT INTO recap_revoked_ranges VALUES(?1,?2,?3,?4) ON CONFLICT(owner,dataset) DO UPDATE SET first_id=MIN(first_id,excluded.first_id),last_id=MAX(last_id,excluded.last_id)", params![sid,dataset,first,last]))?;
                let removed = db(conn.execute(
                    "DELETE FROM recap_revoked_sources WHERE owner=?1 AND dataset=?2",
                    params![sid, dataset],
                ))?;
                db(conn.execute(
                    "UPDATE recap_revocation_budgets SET entries=entries-?2+?3 WHERE owner=?1",
                    params![sid, removed as i64, i64::from(!existed)],
                ))?;
                // Compaction may close small gaps too. Retire existing recap
                // contexts in the broader range; other task consumers remain live.
                db(conn.execute("UPDATE recap_objects SET state='retired',key_blob=NULL WHERE owner=?1 AND dataset=?2
                    AND id IN (SELECT object_id FROM recap_sources WHERE screenshot_id BETWEEN
                        (SELECT first_id FROM recap_revoked_ranges WHERE owner=?1 AND dataset=?2) AND
                        (SELECT last_id FROM recap_revoked_ranges WHERE owner=?1 AND dataset=?2))", params![sid,dataset]))?;
            }
        }
        db(conn.execute(
            "UPDATE recap_objects SET state='retired',key_blob=NULL WHERE owner=?1 AND dataset=?2
            AND id IN (SELECT object_id FROM recap_sources WHERE screenshot_id=?3)",
            params![sid, dataset, id],
        ))?;
        Ok(())
    }

    pub(super) fn retire_recap(conn: &Connection, sid: &str, dataset: Option<&str>) -> Result<()> {
        db(conn.execute("UPDATE recap_objects SET state='retired',key_blob=NULL WHERE owner=?1 AND (?2 IS NULL OR dataset=?2)",params![sid,dataset]))?;
        Ok(())
    }

    pub(super) fn reap_recap(conn: &Connection, sid: &str, now: i64) -> Result<()> {
        db(conn.execute(EXPIRE_RECAP_SQL, params![sid, now]))?;
        db(conn.execute(PRUNE_RECAP_SQL, params![sid, now - 60 * DAY_SECS]))?;
        Ok(())
    }
}
