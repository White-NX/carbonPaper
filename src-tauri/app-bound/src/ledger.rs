//! Service-owned authority. User-writable queue rows are never authorization.
use crate::{
    crypto::{new_key, KeyProtector},
    protocol::*,
};
use rusqlite::{params, Connection, OptionalExtension};
use std::path::Path;

#[derive(Debug, Clone)]
pub struct Principal {
    pub sid: String,
    pub runtime_id: String,
    /// Kernel PID plus process creation time, not a request-supplied PID.
    pub process_identity: String,
}

pub struct Ledger {
    conn: Connection,
}

struct StoredTask {
    binding: TaskBinding,
    state: String,
    done: u8,
    abandoned: u8,
    key: Option<Vec<u8>>,
    digest: Option<String>,
    expires: i64,
}

fn db<T>(value: rusqlite::Result<T>) -> Result<T> {
    value.map_err(|_| BrokerError::Storage)
}

impl Ledger {
    pub fn open(path: &Path) -> Result<Self> {
        Self::from_connection(db(Connection::open(path))?)
    }

    fn from_connection(conn: Connection) -> Result<Self> {
        db(conn.busy_timeout(std::time::Duration::from_secs(3)))?;
        db(conn.execute_batch("PRAGMA foreign_keys=ON; PRAGMA journal_mode=DELETE;
            PRAGMA synchronous=FULL; PRAGMA secure_delete=ON;
            CREATE TABLE IF NOT EXISTS broker_metadata (key TEXT PRIMARY KEY, value INTEGER NOT NULL);
            INSERT OR IGNORE INTO broker_metadata VALUES ('schema', 1);
            CREATE TABLE IF NOT EXISTS owners (
                sid TEXT PRIMARY KEY, runtime_id TEXT NOT NULL, enabled INTEGER NOT NULL,
                retention_days INTEGER NOT NULL, capacity_bytes INTEGER NOT NULL,
                dataset_id TEXT, clock INTEGER NOT NULL DEFAULT 0
            );
            CREATE TABLE IF NOT EXISTS tasks (
                id TEXT PRIMARY KEY, owner TEXT NOT NULL REFERENCES owners(sid),
                dataset TEXT NOT NULL, screenshot_id INTEGER NOT NULL, binding TEXT NOT NULL,
                state TEXT NOT NULL CHECK(state IN ('prepared','active','retired')),
                done INTEGER NOT NULL DEFAULT 0, abandoned INTEGER NOT NULL DEFAULT 0, key_blob BLOB, digest TEXT,
                created INTEGER NOT NULL, expires INTEGER NOT NULL, prepared_until INTEGER NOT NULL,
                bytes INTEGER NOT NULL, UNIQUE(owner, dataset, screenshot_id)
            );
            CREATE INDEX IF NOT EXISTS broker_tasks_owner ON tasks(owner, state, created);
            CREATE TABLE IF NOT EXISTS leases (
                task TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
                consumer INTEGER NOT NULL, id TEXT NOT NULL, process TEXT NOT NULL,
                deadline INTEGER NOT NULL, completed INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY(task, consumer)
            );"))?;
        let schema: i64 = db(conn.query_row(
            "SELECT value FROM broker_metadata WHERE key='schema'",
            [],
            |r| r.get(0),
        ))?;
        if schema != 1 {
            return Err(BrokerError::VersionMismatch);
        }
        Ok(Self { conn })
    }

    /// Installer-only: caller SID is obtained from the unelevated requesting
    /// process. The installer validates the complete signed runtime first.
    pub fn register_owner(&mut self, sid: &str, runtime_id: &str, enable: bool) -> Result<()> {
        let limits = Limits::default();
        let tx = db(self.conn.transaction())?;
        db(tx.execute(
            "INSERT INTO owners(sid,runtime_id,enabled,retention_days,capacity_bytes)
            VALUES(?1,?2,?3,?4,?5) ON CONFLICT(sid) DO UPDATE SET runtime_id=excluded.runtime_id,
            enabled=CASE WHEN ?3=1 THEN 1 ELSE owners.enabled END",
            params![
                sid,
                runtime_id,
                enable,
                limits.retention_days,
                limits.capacity_bytes as i64
            ],
        ))?;
        // Updating the registered image invalidates leases of the old image.
        db(tx.execute("DELETE FROM leases WHERE task IN (SELECT id FROM tasks WHERE owner=?1) AND completed=0", [sid]))?;
        db(tx.commit())
    }

    pub fn unregister_owner(&mut self, sid: &str) -> Result<()> {
        let tx = db(self.conn.transaction())?;
        db(tx.execute("DELETE FROM tasks WHERE owner=?1", [sid]))?;
        db(tx.execute("DELETE FROM owners WHERE sid=?1", [sid]))?;
        db(tx.commit())
    }

    pub fn registered_owners(&self) -> Result<u64> {
        db(self.conn.query_row("SELECT COUNT(*) FROM owners", [], |r| {
            r.get::<_, i64>(0).map(|v| v as u64)
        }))
    }

    pub fn registered_runtime(&self, sid: &str) -> Result<Option<String>> {
        db(self
            .conn
            .query_row("SELECT runtime_id FROM owners WHERE sid=?1", [sid], |r| {
                r.get(0)
            })
            .optional())
    }

    pub fn owner_registration(&self, sid: &str) -> Result<Option<(String, bool)>> {
        db(self
            .conn
            .query_row(
                "SELECT runtime_id,enabled FROM owners WHERE sid=?1",
                [sid],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional())
    }

    pub fn restore_registration(
        &mut self,
        sid: &str,
        previous: Option<(String, bool)>,
    ) -> Result<()> {
        if let Some((runtime, enabled)) = previous {
            db(self.conn.execute(
                "UPDATE owners SET runtime_id=?2,enabled=?3 WHERE sid=?1",
                params![sid, runtime, enabled],
            ))?;
        } else {
            self.unregister_owner(sid)?;
        }
        Ok(())
    }

    pub fn handle(
        &mut self,
        principal: &Principal,
        request: Request,
        wall_time: i64,
        protector: &dyn KeyProtector,
    ) -> Result<Response> {
        let tx = db(self.conn.transaction())?;
        let owner: Option<(String, bool, u32, u64, Option<String>, i64)> = db(tx.query_row(
            "SELECT runtime_id,enabled,retention_days,capacity_bytes,dataset_id,clock FROM owners WHERE sid=?1",
            [&principal.sid], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get::<_,i64>(3)? as u64,r.get(4)?,r.get(5)?))).optional())?;
        let Some((runtime_id, enabled, days, capacity_bytes, dataset, clock)) = owner else {
            return Err(BrokerError::AccessDenied);
        };
        if runtime_id != principal.runtime_id {
            return Err(BrokerError::AccessDenied);
        }
        let now = wall_time.max(clock);
        db(tx.execute(
            "UPDATE owners SET clock=?2 WHERE sid=?1",
            params![principal.sid, now],
        ))?;
        let limits = Limits {
            retention_days: days,
            capacity_bytes,
        };
        Self::reap(&tx, &principal.sid, now, limits)?;
        db(tx.execute_batch("SAVEPOINT broker_command"))?;
        let result = Self::dispatch(
            &tx, principal, request, now, enabled, limits, dataset, protector,
        );
        // Expiry and clock advancement survive rejected requests as well. A
        // failed DPAPI operation cannot publish a partial task or lease.
        match result {
            Ok(response) => {
                db(tx.execute_batch("RELEASE broker_command"))?;
                db(tx.commit())?;
                Ok(response)
            }
            Err(error) => {
                db(tx.execute_batch("ROLLBACK TO broker_command; RELEASE broker_command"))?;
                db(tx.commit())?;
                Err(error)
            }
        }
    }

    fn dispatch(
        conn: &Connection,
        who: &Principal,
        request: Request,
        now: i64,
        enabled: bool,
        limits: Limits,
        dataset: Option<String>,
        protector: &dyn KeyProtector,
    ) -> Result<Response> {
        match request {
            Request::Status {} => return Ok(Response::Status(Self::status(conn, who)?)),
            Request::InspectTask { task_id } => {
                let task = Self::task(conn, who, &task_id, dataset.as_deref())?;
                return Ok(Response::TaskState(TaskState {
                    task: task.binding,
                    active: task.state == "active",
                    retired: task.state == "retired",
                    finished_consumers: task.done & !task.abandoned,
                    abandoned_consumers: task.abandoned,
                    expires_at: task.expires,
                }));
            }
            Request::SetPolicy { enabled, limits } => {
                limits.validate()?;
                db(conn.execute(
                    "UPDATE owners SET enabled=?2,retention_days=?3,capacity_bytes=?4 WHERE sid=?1",
                    params![
                        who.sid,
                        enabled,
                        limits.retention_days,
                        limits.capacity_bytes as i64
                    ],
                ))?;
                if !enabled {
                    Self::retire_where(conn, &who.sid, None)?;
                }
                // Never extend an existing grant when a user raises the limit.
                db(conn.execute("UPDATE tasks SET expires=MIN(expires,created+?2) WHERE owner=?1 AND state!='retired'",
                    params![who.sid, i64::from(limits.retention_days)*DAY_SECS]))?;
                Self::reap(conn, &who.sid, now, limits)?;
                return Ok(Response::Status(Self::status(conn, who)?));
            }
            Request::RetireDataset { dataset_id } => {
                if !valid_id(&dataset_id) {
                    return Err(BrokerError::InvalidRequest);
                }
                Self::retire_where(conn, &who.sid, Some(&dataset_id))?;
                db(conn.execute(
                    "UPDATE owners SET dataset_id=NULL WHERE sid=?1 AND dataset_id=?2",
                    params![who.sid, dataset_id],
                ))?;
                return Ok(Response::Ok);
            }
            Request::RevokeTask { task_id } => {
                if !valid_id(&task_id) {
                    return Err(BrokerError::InvalidRequest);
                }
                db(conn.execute(
                    "UPDATE tasks SET state='retired',key_blob=NULL WHERE id=?1 AND owner=?2",
                    params![task_id, who.sid],
                ))?;
                db(conn.execute("DELETE FROM leases WHERE task IN (SELECT id FROM tasks WHERE id=?1 AND owner=?2)", params![task_id,who.sid]))?;
                return Ok(Response::Ok);
            }
            Request::RevokeTasks { task_ids } => {
                if task_ids.len() > 128 || task_ids.iter().any(|id| !valid_id(id)) {
                    return Err(BrokerError::InvalidRequest);
                }
                for id in task_ids {
                    db(conn.execute(
                        "UPDATE tasks SET state='retired',key_blob=NULL WHERE id=?1 AND owner=?2",
                        params![id, who.sid],
                    ))?;
                    db(conn.execute("DELETE FROM leases WHERE task IN (SELECT id FROM tasks WHERE id=?1 AND owner=?2)",params![id,who.sid]))?;
                }
                return Ok(Response::Ok);
            }
            Request::RevokeScreenshots {
                dataset_id,
                screenshot_ids,
            } => {
                if !valid_id(&dataset_id)
                    || screenshot_ids.len() > 128
                    || screenshot_ids.iter().any(|id| *id <= 0)
                {
                    return Err(BrokerError::InvalidRequest);
                }
                // Deletion is scoped by the protected ledger, never by a
                // possibly rolled-back or missing user-writable queue row.
                for id in screenshot_ids {
                    db(conn.execute("UPDATE tasks SET state='retired',key_blob=NULL WHERE owner=?1 AND dataset=?2 AND screenshot_id=?3",
                        params![who.sid,dataset_id,id]))?;
                    db(conn.execute("DELETE FROM leases WHERE task IN (SELECT id FROM tasks WHERE owner=?1 AND dataset=?2 AND screenshot_id=?3)",
                        params![who.sid,dataset_id,id]))?;
                }
                return Ok(Response::Ok);
            }
            Request::AbandonConsumer { task_id, consumer } => {
                let task = Self::task(conn, who, &task_id, dataset.as_deref())?;
                if task.binding.consumers & consumer.bit() == 0 {
                    return Err(BrokerError::AccessDenied);
                }
                if task.done & consumer.bit() != 0 {
                    return Ok(Response::Ok);
                }
                let done = task.done | consumer.bit();
                db(conn.execute("UPDATE tasks SET done=?2,abandoned=(abandoned | ?4),state=CASE WHEN (?2 & ?3)=?3 THEN 'retired' ELSE state END,
                    key_blob=CASE WHEN (?2 & ?3)=?3 THEN NULL ELSE key_blob END WHERE id=?1",params![task_id,done,task.binding.consumers,consumer.bit()]))?;
                db(conn.execute(
                    "DELETE FROM leases WHERE task=?1 AND consumer=?2",
                    params![task_id, consumer.bit()],
                ))?;
                return Ok(Response::Ok);
            }
            Request::AttachDataset { dataset_id } => {
                if !valid_id(&dataset_id) {
                    return Err(BrokerError::InvalidRequest);
                }
                if dataset.as_ref().is_some_and(|d| d != &dataset_id) {
                    return Err(BrokerError::DatasetMismatch);
                }
                db(conn.execute(
                    "UPDATE owners SET dataset_id=?2 WHERE sid=?1",
                    params![who.sid, dataset_id],
                ))?;
                return Ok(Response::Ok);
            }
            _ => {}
        }
        if !enabled {
            return Err(BrokerError::Disabled);
        }
        match request {
            Request::PrepareTask {
                dataset_id,
                screenshot_id,
                consumers,
                payload_bytes,
            } => {
                if !valid_id(&dataset_id)
                    || screenshot_id <= 0
                    || consumers == 0
                    || consumers & !7 != 0
                    || !(28..=MAX_INPUT_BYTES + 28).contains(&payload_bytes)
                {
                    return Err(BrokerError::InvalidRequest);
                }
                if dataset.as_deref() != Some(&dataset_id) {
                    return Err(BrokerError::DatasetMismatch);
                }
                let already: bool = db(conn.query_row("SELECT EXISTS(SELECT 1 FROM tasks WHERE owner=?1 AND dataset=?2 AND screenshot_id=?3)",
                    params![who.sid,dataset_id,screenshot_id], |r| r.get(0)))?;
                if already {
                    return Err(BrokerError::Retired);
                }
                let key = new_key();
                // Protect before changing state so an OS failure cannot orphan
                // an unprotected record or consume another task's capacity.
                let sealed = protector.protect(&key)?;
                Self::make_room(conn, &who.sid, payload_bytes, limits.capacity_bytes)?;
                let task = TaskBinding {
                    task_id: random_id(),
                    dataset_id,
                    screenshot_id,
                    input_version: INPUT_VERSION,
                    consumers,
                    payload_bytes,
                    expires_at: now.saturating_add(i64::from(limits.retention_days) * DAY_SECS),
                };
                let binding =
                    serde_json::to_string(&task).map_err(|_| BrokerError::InvalidRequest)?;
                db(conn.execute("INSERT INTO tasks(id,owner,dataset,screenshot_id,binding,state,key_blob,created,expires,prepared_until,bytes)
                    VALUES(?1,?2,?3,?4,?5,'prepared',?6,?7,?8,?9,?10)", params![task.task_id,who.sid,task.dataset_id,
                    screenshot_id,binding,sealed,now,task.expires_at,now.saturating_add(PREPARE_TTL_SECS),payload_bytes as i64]))?;
                Ok(Response::Prepared(PreparedTask { task, key }))
            }
            Request::ActivateTask {
                task_id,
                ciphertext_digest,
            } => {
                if !valid_id(&ciphertext_digest) {
                    return Err(BrokerError::InvalidRequest);
                }
                let task = Self::task(conn, who, &task_id, dataset.as_deref())?;
                if task.state == "retired" {
                    return Err(BrokerError::Retired);
                }
                if task
                    .digest
                    .as_ref()
                    .is_some_and(|v| v != &ciphertext_digest)
                {
                    return Err(BrokerError::Integrity);
                }
                db(conn.execute(
                    "UPDATE tasks SET state='active',digest=?2 WHERE id=?1",
                    params![task_id, ciphertext_digest],
                ))?;
                Ok(Response::Ok)
            }
            Request::AcquireTask {
                task_id,
                consumer,
                ciphertext_digest,
            } => {
                let task = Self::task(conn, who, &task_id, dataset.as_deref())?;
                if task.state != "active" || task.done & consumer.bit() != 0 {
                    return Err(BrokerError::Retired);
                }
                if task.binding.consumers & consumer.bit() == 0 {
                    return Err(BrokerError::AccessDenied);
                }
                if task.digest.as_deref() != Some(&ciphertext_digest) {
                    return Err(BrokerError::Integrity);
                }
                let deadline: Option<i64> = db(conn
                    .query_row(
                        "SELECT deadline FROM leases WHERE task=?1 AND consumer=?2 AND completed=0",
                        params![task_id, consumer.bit()],
                        |r| r.get(0),
                    )
                    .optional())?;
                if deadline.is_some_and(|v| v > now) {
                    return Err(BrokerError::Busy);
                }
                let key = protector.unprotect(task.key.as_deref().ok_or(BrokerError::Retired)?)?;
                if key.0.len() != 32 {
                    return Err(BrokerError::Integrity);
                }
                let deadline = task.expires.min(now.saturating_add(LEASE_TTL_SECS));
                let lease_id = random_id();
                db(conn.execute("INSERT INTO leases(task,consumer,id,process,deadline,completed) VALUES(?1,?2,?3,?4,?5,0)
                    ON CONFLICT(task,consumer) DO UPDATE SET id=excluded.id,process=excluded.process,deadline=excluded.deadline,completed=0",
                    params![task_id,consumer.bit(),lease_id,who.process_identity,deadline]))?;
                Ok(Response::Lease(TaskLease {
                    task: task.binding,
                    consumer,
                    lease_id,
                    deadline,
                    key,
                }))
            }
            Request::RenewLease {
                task_id,
                consumer,
                lease_id,
            } => {
                let task = Self::task(conn, who, &task_id, dataset.as_deref())?;
                Self::check_lease(conn, who, &task_id, consumer, &lease_id, now, false)?;
                if task.state != "active" {
                    return Err(BrokerError::Retired);
                }
                let deadline = task.expires.min(now.saturating_add(LEASE_TTL_SECS));
                db(conn.execute(
                    "UPDATE leases SET deadline=?3 WHERE task=?1 AND consumer=?2",
                    params![task_id, consumer.bit(), deadline],
                ))?;
                Ok(Response::Deadline(deadline))
            }
            Request::ReleaseLease {
                task_id,
                consumer,
                lease_id,
            } => {
                Self::task(conn, who, &task_id, dataset.as_deref())?;
                Self::check_lease(conn, who, &task_id, consumer, &lease_id, now, false)?;
                db(conn.execute(
                    "DELETE FROM leases WHERE task=?1 AND consumer=?2",
                    params![task_id, consumer.bit()],
                ))?;
                Ok(Response::Ok)
            }
            Request::FinishConsumer {
                task_id,
                consumer,
                lease_id,
            } => {
                let task = Self::task(conn, who, &task_id, dataset.as_deref())?;
                // Only an exact, previously completed receipt is idempotent.
                // A new process may resend the durable receipt without a key.
                if Self::check_lease(conn, who, &task_id, consumer, &lease_id, now, true)? {
                    return Ok(Response::Ok);
                }
                if task.state != "active" {
                    return Err(BrokerError::Retired);
                }
                let done = task.done | consumer.bit();
                db(conn.execute(
                    "UPDATE leases SET completed=1 WHERE task=?1 AND consumer=?2",
                    params![task_id, consumer.bit()],
                ))?;
                if done & task.binding.consumers == task.binding.consumers {
                    db(conn.execute(
                        "UPDATE tasks SET done=?2,state='retired',key_blob=NULL WHERE id=?1",
                        params![task_id, done],
                    ))?;
                } else {
                    db(conn.execute(
                        "UPDATE tasks SET done=?2 WHERE id=?1",
                        params![task_id, done],
                    ))?;
                }
                Ok(Response::Ok)
            }
            _ => Err(BrokerError::InvalidRequest),
        }
    }

    fn check_lease(
        conn: &Connection,
        who: &Principal,
        id: &str,
        consumer: Consumer,
        lease_id: &str,
        now: i64,
        allow_completed: bool,
    ) -> Result<bool> {
        if !valid_id(lease_id) {
            return Err(BrokerError::InvalidRequest);
        }
        let lease: Option<(String, String, i64, bool)> = db(conn
            .query_row(
                "SELECT id,process,deadline,completed FROM leases WHERE task=?1 AND consumer=?2",
                params![id, consumer.bit()],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional())?;
        let Some((stored, process, deadline, completed)) = lease else {
            return Err(BrokerError::LeaseExpired);
        };
        if stored != lease_id {
            return Err(BrokerError::LeaseExpired);
        }
        if allow_completed && completed {
            return Ok(true);
        }
        if completed || process != who.process_identity || deadline <= now {
            return Err(BrokerError::LeaseExpired);
        }
        Ok(false)
    }

    fn task(
        conn: &Connection,
        who: &Principal,
        id: &str,
        dataset: Option<&str>,
    ) -> Result<StoredTask> {
        if !valid_id(id) {
            return Err(BrokerError::InvalidRequest);
        }
        let row: Option<(String,String,u8,u8,Option<Vec<u8>>,Option<String>,i64)> = db(conn.query_row(
            "SELECT binding,state,done,abandoned,key_blob,digest,expires FROM tasks WHERE id=?1 AND owner=?2", params![id,who.sid],
            |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?))).optional())?;
        let Some((binding, state, done, abandoned, key, digest, expires)) = row else {
            return Err(BrokerError::Retired);
        };
        let binding: TaskBinding =
            serde_json::from_str(&binding).map_err(|_| BrokerError::Integrity)?;
        if dataset != Some(&binding.dataset_id) {
            return Err(BrokerError::DatasetMismatch);
        }
        Ok(StoredTask {
            binding,
            state,
            done,
            abandoned,
            key,
            digest,
            expires,
        })
    }

    fn status(conn: &Connection, who: &Principal) -> Result<BrokerStatus> {
        let (enabled, days, bytes, dataset): (bool, u32, u64, Option<String>) = db(conn
            .query_row(
                "SELECT enabled,retention_days,capacity_bytes,dataset_id FROM owners WHERE sid=?1",
                [&who.sid],
                |r| Ok((r.get(0)?, r.get(1)?, r.get::<_, i64>(2)? as u64, r.get(3)?)),
            ))?;
        let (count, used) = Self::usage(conn, &who.sid)?;
        Ok(BrokerStatus {
            enabled,
            limits: Limits {
                retention_days: days,
                capacity_bytes: bytes,
            },
            dataset_id: dataset,
            active_tasks: count,
            active_bytes: used,
            runtime_id: who.runtime_id.clone(),
        })
    }

    fn usage(conn: &Connection, sid: &str) -> Result<(u64, u64)> {
        db(conn.query_row(
            "SELECT COUNT(*),COALESCE(SUM(bytes),0) FROM tasks WHERE owner=?1 AND state!='retired'",
            [sid],
            |r| Ok((r.get::<_, i64>(0)? as u64, r.get::<_, i64>(1)? as u64)),
        ))
    }

    fn retire_where(conn: &Connection, sid: &str, dataset: Option<&str>) -> Result<()> {
        db(conn.execute("UPDATE tasks SET state='retired',key_blob=NULL WHERE owner=?1 AND (?2 IS NULL OR dataset=?2)", params![sid,dataset]))?;
        db(conn.execute("DELETE FROM leases WHERE task IN (SELECT id FROM tasks WHERE owner=?1 AND (?2 IS NULL OR dataset=?2))", params![sid,dataset]))?;
        Ok(())
    }

    fn reap(conn: &Connection, sid: &str, now: i64, limits: Limits) -> Result<()> {
        db(conn.execute(
            "UPDATE tasks SET state='retired',key_blob=NULL WHERE owner=?1 AND state!='retired'
            AND (expires<=?2 OR (state='prepared' AND prepared_until<=?2))",
            params![sid, now],
        ))?;
        Self::make_room(conn, sid, 0, limits.capacity_bytes)?;
        // Unknown IDs fail closed too, so old tombstones can be reclaimed once
        // no valid input grant could survive. Keys are never reconstructed.
        db(conn.execute(
            "DELETE FROM tasks WHERE owner=?1 AND state='retired' AND expires<?2",
            params![sid, now - 60 * DAY_SECS],
        ))?;
        Ok(())
    }

    pub fn maintenance(&mut self, wall_time: i64) -> Result<()> {
        let owners = {
            let mut stmt = db(self
                .conn
                .prepare("SELECT sid,retention_days,capacity_bytes,clock FROM owners"))?;
            let rows = db(stmt.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, u32>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, i64>(3)?,
                ))
            }))?;
            db(rows.collect::<rusqlite::Result<Vec<_>>>())?
        };
        let tx = db(self.conn.transaction())?;
        for (sid, days, bytes, clock) in owners {
            db(tx.execute(
                "UPDATE owners SET clock=?2 WHERE sid=?1",
                params![sid, wall_time.max(clock)],
            ))?;
            Self::reap(
                &tx,
                &sid,
                wall_time.max(clock),
                Limits {
                    retention_days: days,
                    capacity_bytes: bytes as u64,
                },
            )?;
        }
        db(tx.commit())
    }

    fn make_room(conn: &Connection, sid: &str, incoming: u64, capacity: u64) -> Result<()> {
        if incoming > capacity {
            return Err(BrokerError::LimitExceeded);
        }
        loop {
            let (count, bytes) = Self::usage(conn, sid)?;
            if bytes.saturating_add(incoming) <= capacity
                && (incoming == 0 || count < MAX_ACTIVE_TASKS)
            {
                return Ok(());
            }
            let id: Option<String> = db(conn.query_row("SELECT id FROM tasks WHERE owner=?1 AND state!='retired' ORDER BY created,id LIMIT 1", [sid], |r| r.get(0)).optional())?;
            let Some(id) = id else {
                return Err(BrokerError::LimitExceeded);
            };
            db(conn.execute(
                "UPDATE tasks SET state='retired',key_blob=NULL WHERE id=?1",
                [&id],
            ))?;
            db(conn.execute("DELETE FROM leases WHERE task=?1", [&id]))?;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct TestProtection;
    impl KeyProtector for TestProtection {
        fn protect(&self, key: &TaskKey) -> Result<Vec<u8>> {
            Ok(key.0.iter().map(|b| b ^ 0xa5).collect())
        }
        fn unprotect(&self, blob: &[u8]) -> Result<TaskKey> {
            Ok(TaskKey(blob.iter().map(|b| b ^ 0xa5).collect()))
        }
    }
    fn fixture() -> (Ledger, Principal, String) {
        let mut ledger = Ledger::from_connection(Connection::open_in_memory().unwrap()).unwrap();
        let who = Principal {
            sid: "user-a".into(),
            runtime_id: "release-a".into(),
            process_identity: "10:100".into(),
        };
        ledger
            .register_owner(&who.sid, &who.runtime_id, true)
            .unwrap();
        let dataset = random_id();
        ledger
            .handle(
                &who,
                Request::AttachDataset {
                    dataset_id: dataset.clone(),
                },
                100,
                &TestProtection,
            )
            .unwrap();
        (ledger, who, dataset)
    }
    fn prepare(
        ledger: &mut Ledger,
        who: &Principal,
        dataset: &str,
        id: i64,
        mask: u8,
    ) -> PreparedTask {
        let Response::Prepared(prepared) = ledger
            .handle(
                who,
                Request::PrepareTask {
                    dataset_id: dataset.into(),
                    screenshot_id: id,
                    consumers: mask,
                    payload_bytes: 40,
                },
                100,
                &TestProtection,
            )
            .unwrap()
        else {
            panic!()
        };
        ledger
            .handle(
                who,
                Request::ActivateTask {
                    task_id: prepared.task.task_id.clone(),
                    ciphertext_digest: "a".repeat(64),
                },
                101,
                &TestProtection,
            )
            .unwrap();
        prepared
    }
    fn acquire(
        ledger: &mut Ledger,
        who: &Principal,
        id: &str,
        consumer: Consumer,
        time: i64,
    ) -> Result<TaskLease> {
        match ledger.handle(
            who,
            Request::AcquireTask {
                task_id: id.into(),
                consumer,
                ciphertext_digest: "a".repeat(64),
            },
            time,
            &TestProtection,
        )? {
            Response::Lease(lease) => Ok(lease),
            _ => panic!(),
        }
    }
    fn finish(
        ledger: &mut Ledger,
        who: &Principal,
        lease: &TaskLease,
        now: i64,
    ) -> Result<Response> {
        ledger.handle(
            who,
            Request::FinishConsumer {
                task_id: lease.task.task_id.clone(),
                consumer: lease.consumer,
                lease_id: lease.lease_id.clone(),
            },
            now,
            &TestProtection,
        )
    }
    #[test]
    fn consumers_finish_independently_and_replays_never_resurrect_keys() {
        let (mut ledger, who, ds) = fixture();
        let task = prepare(&mut ledger, &who, &ds, 1, 7);
        for consumer in Consumer::ALL {
            let lease = acquire(&mut ledger, &who, &task.task.task_id, consumer, 102).unwrap();
            assert_eq!(lease.key.0, task.key.0);
            finish(&mut ledger, &who, &lease, 103).unwrap();
            finish(&mut ledger, &who, &lease, 104).unwrap();
            assert_eq!(
                acquire(&mut ledger, &who, &task.task.task_id, consumer, 105).unwrap_err(),
                BrokerError::Retired
            );
        }
        let key: Option<Vec<u8>> = ledger
            .conn
            .query_row(
                "SELECT key_blob FROM tasks WHERE id=?1",
                [&task.task.task_id],
                |r| r.get(0),
            )
            .unwrap();
        assert!(key.is_none());
        assert!(ledger
            .handle(
                &who,
                Request::ActivateTask {
                    task_id: task.task.task_id,
                    ciphertext_digest: "a".repeat(64)
                },
                106,
                &TestProtection
            )
            .is_err());
    }
    #[test]
    fn leases_are_fenced_by_kernel_process_identity_and_expiry() {
        let (mut ledger, who, ds) = fixture();
        let task = prepare(&mut ledger, &who, &ds, 2, 1);
        let old = acquire(
            &mut ledger,
            &who,
            &task.task.task_id,
            Consumer::Classification,
            102,
        )
        .unwrap();
        let mut replacement = who.clone();
        replacement.process_identity = "10:200".into();
        assert_eq!(
            finish(&mut ledger, &replacement, &old, 103).unwrap_err(),
            BrokerError::LeaseExpired
        );
        let new = acquire(
            &mut ledger,
            &replacement,
            &task.task.task_id,
            Consumer::Classification,
            403,
        )
        .unwrap();
        assert_eq!(
            finish(&mut ledger, &who, &old, 404).unwrap_err(),
            BrokerError::LeaseExpired
        );
        finish(&mut ledger, &replacement, &new, 404).unwrap();
    }
    #[test]
    fn wrong_user_runtime_digest_and_dataset_are_denied() {
        let (mut ledger, who, ds) = fixture();
        let task = prepare(&mut ledger, &who, &ds, 3, 1);
        let mut other = who.clone();
        other.sid = "user-b".into();
        ledger
            .register_owner(&other.sid, &other.runtime_id, true)
            .unwrap();
        assert!(acquire(
            &mut ledger,
            &other,
            &task.task.task_id,
            Consumer::Classification,
            102
        )
        .is_err());
        other = who.clone();
        other.runtime_id = "old-release".into();
        assert_eq!(
            acquire(
                &mut ledger,
                &other,
                &task.task.task_id,
                Consumer::Classification,
                102
            )
            .unwrap_err(),
            BrokerError::AccessDenied
        );
        assert!(ledger
            .handle(
                &who,
                Request::AcquireTask {
                    task_id: task.task.task_id,
                    consumer: Consumer::Classification,
                    ciphertext_digest: "b".repeat(64)
                },
                102,
                &TestProtection
            )
            .is_err());
        assert_eq!(
            ledger
                .handle(
                    &who,
                    Request::AttachDataset {
                        dataset_id: random_id()
                    },
                    102,
                    &TestProtection
                )
                .unwrap_err(),
            BrokerError::DatasetMismatch
        );
    }
    #[test]
    fn shorter_retention_and_disable_revoke_without_reenable_resurrection() {
        let (mut ledger, who, ds) = fixture();
        let task = prepare(&mut ledger, &who, &ds, 4, 1);
        ledger
            .handle(
                &who,
                Request::SetPolicy {
                    enabled: true,
                    limits: Limits {
                        retention_days: 1,
                        ..Limits::default()
                    },
                },
                100 + 2 * DAY_SECS,
                &TestProtection,
            )
            .unwrap();
        assert_eq!(
            acquire(
                &mut ledger,
                &who,
                &task.task.task_id,
                Consumer::Classification,
                100 + 2 * DAY_SECS
            )
            .unwrap_err(),
            BrokerError::Retired
        );
        ledger
            .handle(
                &who,
                Request::SetPolicy {
                    enabled: false,
                    limits: Limits::default(),
                },
                100 + 2 * DAY_SECS,
                &TestProtection,
            )
            .unwrap();
        ledger
            .handle(
                &who,
                Request::SetPolicy {
                    enabled: true,
                    limits: Limits::default(),
                },
                100 + 2 * DAY_SECS,
                &TestProtection,
            )
            .unwrap();
        assert!(acquire(
            &mut ledger,
            &who,
            &task.task.task_id,
            Consumer::Classification,
            100 + 2 * DAY_SECS
        )
        .is_err());
    }
    #[test]
    fn completed_state_survives_service_restart() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("keys.db");
        let (_, who, ds) = fixture();
        let mut ledger = Ledger::open(&path).unwrap();
        ledger
            .register_owner(&who.sid, &who.runtime_id, true)
            .unwrap();
        ledger
            .handle(
                &who,
                Request::AttachDataset {
                    dataset_id: ds.clone(),
                },
                100,
                &TestProtection,
            )
            .unwrap();
        let task = prepare(&mut ledger, &who, &ds, 5, 1);
        let lease = acquire(
            &mut ledger,
            &who,
            &task.task.task_id,
            Consumer::Classification,
            102,
        )
        .unwrap();
        finish(&mut ledger, &who, &lease, 103).unwrap();
        drop(ledger);
        let mut reopened = Ledger::open(&path).unwrap();
        assert_eq!(
            acquire(
                &mut reopened,
                &who,
                &task.task.task_id,
                Consumer::Classification,
                104
            )
            .unwrap_err(),
            BrokerError::Retired
        );
    }

    #[test]
    fn abandoned_consumer_does_not_destroy_another_consumers_key() {
        let (mut ledger, who, ds) = fixture();
        let task = prepare(&mut ledger, &who, &ds, 6, 7);
        ledger
            .handle(
                &who,
                Request::AbandonConsumer {
                    task_id: task.task.task_id.clone(),
                    consumer: Consumer::Classification,
                },
                102,
                &TestProtection,
            )
            .unwrap();
        assert_eq!(
            acquire(
                &mut ledger,
                &who,
                &task.task.task_id,
                Consumer::Classification,
                103
            )
            .unwrap_err(),
            BrokerError::Retired
        );
        let minilm = acquire(&mut ledger, &who, &task.task.task_id, Consumer::MiniLm, 103).unwrap();
        finish(&mut ledger, &who, &minilm, 104).unwrap();
        ledger
            .handle(
                &who,
                Request::AbandonConsumer {
                    task_id: task.task.task_id.clone(),
                    consumer: Consumer::MiniLm,
                },
                105,
                &TestProtection,
            )
            .unwrap();
        let Response::TaskState(state) = ledger
            .handle(
                &who,
                Request::InspectTask {
                    task_id: task.task.task_id.clone(),
                },
                106,
                &TestProtection,
            )
            .unwrap()
        else {
            panic!()
        };
        assert_eq!(state.finished_consumers, Consumer::MiniLm.bit());
        assert_eq!(state.abandoned_consumers, Consumer::Classification.bit());
        assert!(!state.retired);
        assert!(acquire(&mut ledger, &who, &task.task.task_id, Consumer::Clip, 107).is_ok());
    }

    #[test]
    fn prepare_expiry_and_clock_rollback_never_reactivate_a_key() {
        let (mut ledger, who, ds) = fixture();
        let Response::Prepared(task) = ledger
            .handle(
                &who,
                Request::PrepareTask {
                    dataset_id: ds,
                    screenshot_id: 7,
                    consumers: 1,
                    payload_bytes: 40,
                },
                100,
                &TestProtection,
            )
            .unwrap()
        else {
            panic!()
        };
        ledger.maintenance(100 + PREPARE_TTL_SECS).unwrap();
        assert_eq!(
            ledger
                .handle(
                    &who,
                    Request::ActivateTask {
                        task_id: task.task.task_id.clone(),
                        ciphertext_digest: "a".repeat(64)
                    },
                    101,
                    &TestProtection
                )
                .unwrap_err(),
            BrokerError::Retired
        );
        let stored: Option<Vec<u8>> = ledger
            .conn
            .query_row(
                "SELECT key_blob FROM tasks WHERE id=?1",
                [task.task.task_id],
                |r| r.get(0),
            )
            .unwrap();
        assert!(stored.is_none());
    }

    #[test]
    fn capacity_retires_the_oldest_task_and_policy_increases_do_not_restore_it() {
        let (mut ledger, who, ds) = fixture();
        let limits = Limits {
            capacity_bytes: 256 * 1024 * 1024,
            ..Limits::default()
        };
        ledger
            .handle(
                &who,
                Request::SetPolicy {
                    enabled: true,
                    limits,
                },
                100,
                &TestProtection,
            )
            .unwrap();
        let mut first = None;
        for id in 1..=33 {
            let Response::Prepared(task) = ledger
                .handle(
                    &who,
                    Request::PrepareTask {
                        dataset_id: ds.clone(),
                        screenshot_id: id,
                        consumers: 1,
                        payload_bytes: 8 * 1024 * 1024,
                    },
                    100 + id,
                    &TestProtection,
                )
                .unwrap()
            else {
                panic!()
            };
            ledger
                .handle(
                    &who,
                    Request::ActivateTask {
                        task_id: task.task.task_id.clone(),
                        ciphertext_digest: "a".repeat(64),
                    },
                    100 + id,
                    &TestProtection,
                )
                .unwrap();
            if id == 1 {
                first = Some(task.task.task_id);
            }
        }
        let (count, bytes) = Ledger::usage(&ledger.conn, &who.sid).unwrap();
        assert_eq!(count, 32);
        assert_eq!(bytes, limits.capacity_bytes);
        ledger
            .handle(
                &who,
                Request::SetPolicy {
                    enabled: true,
                    limits: Limits::default(),
                },
                200,
                &TestProtection,
            )
            .unwrap();
        assert_eq!(
            acquire(
                &mut ledger,
                &who,
                &first.unwrap(),
                Consumer::Classification,
                201
            )
            .unwrap_err(),
            BrokerError::Retired
        );
    }

    #[test]
    fn screenshot_revocation_uses_protected_scope_without_any_user_queue_data() {
        let (mut ledger, who, ds) = fixture();
        let first = prepare(&mut ledger, &who, &ds, 8, 7);
        let second = prepare(&mut ledger, &who, &ds, 9, 1);
        ledger
            .handle(
                &who,
                Request::RevokeScreenshots {
                    dataset_id: random_id(),
                    screenshot_ids: vec![8],
                },
                102,
                &TestProtection,
            )
            .unwrap();
        ledger
            .handle(
                &who,
                Request::RevokeScreenshots {
                    dataset_id: ds,
                    screenshot_ids: vec![8],
                },
                103,
                &TestProtection,
            )
            .unwrap();
        assert_eq!(
            acquire(&mut ledger, &who, &first.task.task_id, Consumer::Clip, 104).unwrap_err(),
            BrokerError::Retired
        );
        assert!(acquire(
            &mut ledger,
            &who,
            &second.task.task_id,
            Consumer::Classification,
            104
        )
        .is_ok());
    }

    #[test]
    fn installer_registration_rollback_never_rolls_back_revocation() {
        let (mut ledger, who, ds) = fixture();
        let task = prepare(&mut ledger, &who, &ds, 10, 1);
        let previous = ledger.owner_registration(&who.sid).unwrap();
        ledger
            .register_owner(&who.sid, "new-runtime", false)
            .unwrap();
        let mut updated = who.clone();
        updated.runtime_id = "new-runtime".into();
        ledger
            .handle(
                &updated,
                Request::RevokeTask {
                    task_id: task.task.task_id.clone(),
                },
                102,
                &TestProtection,
            )
            .unwrap();
        ledger.restore_registration(&who.sid, previous).unwrap();
        assert_eq!(
            acquire(
                &mut ledger,
                &who,
                &task.task.task_id,
                Consumer::Classification,
                103
            )
            .unwrap_err(),
            BrokerError::Retired
        );
    }

    #[test]
    fn failed_protection_and_invalid_policy_do_not_publish_partial_changes() {
        struct FailedProtection;
        impl KeyProtector for FailedProtection {
            fn protect(&self, _: &TaskKey) -> Result<Vec<u8>> {
                Err(BrokerError::Protection)
            }
            fn unprotect(&self, _: &[u8]) -> Result<TaskKey> {
                Err(BrokerError::Protection)
            }
        }
        let (mut ledger, who, ds) = fixture();
        let task = prepare(&mut ledger, &who, &ds, 11, 1);
        assert_eq!(
            ledger
                .handle(
                    &who,
                    Request::PrepareTask {
                        dataset_id: ds,
                        screenshot_id: 12,
                        consumers: 1,
                        payload_bytes: 40
                    },
                    102,
                    &FailedProtection
                )
                .unwrap_err(),
            BrokerError::Protection
        );
        assert_eq!(Ledger::usage(&ledger.conn, &who.sid).unwrap().0, 1);
        assert_eq!(
            ledger
                .handle(
                    &who,
                    Request::AcquireTask {
                        task_id: task.task.task_id.clone(),
                        consumer: Consumer::Classification,
                        ciphertext_digest: "a".repeat(64)
                    },
                    103,
                    &FailedProtection
                )
                .unwrap_err(),
            BrokerError::Protection
        );
        let leases: i64 = ledger
            .conn
            .query_row("SELECT COUNT(*) FROM leases", [], |r| r.get(0))
            .unwrap();
        assert_eq!(leases, 0);
        assert!(ledger
            .handle(
                &who,
                Request::SetPolicy {
                    enabled: false,
                    limits: Limits {
                        retention_days: 0,
                        ..Limits::default()
                    }
                },
                104,
                &TestProtection
            )
            .is_err());
        assert!(acquire(
            &mut ledger,
            &who,
            &task.task.task_id,
            Consumer::Classification,
            105
        )
        .is_ok());
    }
}
