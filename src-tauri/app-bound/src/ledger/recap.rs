use super::*;

impl Ledger {
    pub(super) fn init_recap(conn: &Connection) -> Result<()> {
        db(conn.execute_batch("CREATE TABLE IF NOT EXISTS recap_objects (
            id TEXT PRIMARY KEY,owner TEXT NOT NULL REFERENCES owners(sid),dataset TEXT NOT NULL,
            slot TEXT NOT NULL,binding TEXT NOT NULL,state TEXT NOT NULL,
            key_blob BLOB,digest TEXT,created INTEGER NOT NULL,expires INTEGER NOT NULL,
            prepared_until INTEGER NOT NULL,bytes INTEGER NOT NULL);
            CREATE INDEX IF NOT EXISTS recap_objects_slot ON recap_objects(owner,dataset,slot,state);
            CREATE TABLE IF NOT EXISTS recap_sources (
                object_id TEXT NOT NULL REFERENCES recap_objects(id) ON DELETE CASCADE,
                screenshot_id INTEGER NOT NULL,PRIMARY KEY(object_id,screenshot_id));
            CREATE INDEX IF NOT EXISTS recap_source_lookup ON recap_sources(screenshot_id,object_id);
            CREATE TABLE IF NOT EXISTS recap_revoked_sources (
                owner TEXT NOT NULL REFERENCES owners(sid),dataset TEXT NOT NULL,screenshot_id INTEGER NOT NULL,
                PRIMARY KEY(owner,dataset,screenshot_id));"))
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
                    let revoked:bool=db(conn.query_row("SELECT EXISTS(SELECT 1 FROM recap_revoked_sources WHERE owner=?1 AND dataset=?2 AND screenshot_id=?3)",params![who.sid,dataset,id],|r|r.get(0)))?;
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
            "INSERT OR IGNORE INTO recap_revoked_sources VALUES(?1,?2,?3)",
            params![sid, dataset, id],
        ))?;
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
        db(conn.execute("UPDATE recap_objects SET state='retired',key_blob=NULL WHERE owner=?1 AND state!='retired'
            AND (expires<=?2 OR (state='prepared' AND prepared_until<=?2))",params![sid,now]))?;
        db(conn.execute(
            "DELETE FROM recap_objects WHERE owner=?1 AND state='retired' AND created<?2",
            params![sid, now - 60 * DAY_SECS],
        ))?;
        Ok(())
    }
}
