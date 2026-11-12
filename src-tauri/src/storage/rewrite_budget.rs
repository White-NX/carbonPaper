//! Shared pacing for online rewrites. A permit is acquired before database or
//! maintenance locks; its next delay is paid only after those locks are released.
use super::connection::ActivityGate;
use rusqlite::{ffi, types::ValueRef, Connection};
use std::cell::Cell;
use std::sync::atomic::Ordering;
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

const IDLE_BYTES_PER_SECOND: u64 = 16 * 1024 * 1024;
const FOREGROUND_BYTES_PER_SECOND: u64 = 4 * 1024 * 1024;
const FOREGROUND_WINDOW: Duration = Duration::from_secs(2);

#[derive(Clone, Copy)]
struct IoAccounting {
    bytes: u64,
    complete: bool,
}

thread_local! {
    // A synchronous rewrite permit cannot leave its owning thread. Database
    // guards on other threads never add foreground/capture I/O to this batch.
    static REWRITE_IO: Cell<Option<IoAccounting>> = const { Cell::new(None) };
}

fn update_io(update: impl FnOnce(&mut IoAccounting)) {
    REWRITE_IO.with(|slot| {
        if let Some(mut accounting) = slot.get() {
            update(&mut accounting);
            slot.set(Some(accounting));
        }
    });
}

/// Sample only while the named guard exclusively owns its SQLite connection.
/// Leaving the mutex for CNG preparation ends the sample, so other users of the
/// same connection are not charged to maintenance when it reacquires the mutex.
pub(super) struct ConnectionIoSample {
    connection: usize,
    before: [u32; 2],
    page_bytes: u64,
}

impl ConnectionIoSample {
    pub(super) fn begin(conn: Option<&Connection>) -> Option<Self> {
        if !REWRITE_IO.with(|slot| slot.get().is_some()) {
            return None;
        }
        let sample = conn.and_then(|conn| {
            let before = page_counters(conn)?;
            // SQLCipher maps page_size to cipher_page_size and returns decimal
            // TEXT; ordinary SQLite returns INTEGER. Accept both engine forms.
            let page_bytes = conn
                .pragma_query_value(None, "page_size", |row| {
                    Ok(match row.get_ref(0)? {
                        ValueRef::Integer(bytes) => u64::try_from(bytes).ok(),
                        ValueRef::Text(bytes) => std::str::from_utf8(bytes)
                            .ok()
                            .and_then(|value| value.parse::<u64>().ok()),
                        _ => None,
                    })
                })
                .ok()??;
            if !(512..=65_536).contains(&page_bytes) || !page_bytes.is_power_of_two() {
                return None;
            }
            // SAFETY: the named guard owns and retains this live connection.
            let connection = unsafe { conn.handle() } as usize;
            Some(Self {
                connection,
                before,
                page_bytes,
            })
        });
        if sample.is_none() {
            update_io(|accounting| accounting.complete = false);
        }
        sample
    }

    pub(super) fn finish(self, conn: Option<&Connection>) {
        let after = conn.and_then(|conn| {
            // A database replacement must never mix counters from two files.
            // SAFETY: the guard still owns the currently live connection.
            if unsafe { conn.handle() } as usize != self.connection {
                return None;
            }
            page_counters(conn)
        });
        update_io(|accounting| {
            if let Some(after) = after {
                let reads = u64::from(after[0].wrapping_sub(self.before[0]));
                let writes = u64::from(after[1].wrapping_sub(self.before[1]));
                // Cache misses are logical page reads, including OS cache hits.
                // Reserve three transfers per write for journal/WAL and later
                // checkpoint traffic. This is a conservative page-I/O estimate,
                // not a physical-disk counter or a filesystem throughput cap.
                let bytes = reads
                    .saturating_add(writes.saturating_mul(3))
                    .saturating_mul(self.page_bytes);
                accounting.bytes = accounting.bytes.saturating_add(bytes);
            } else {
                accounting.complete = false;
            }
        });
    }
}

fn page_counters(conn: &Connection) -> Option<[u32; 2]> {
    let mut values = [0; 2];
    for (value, operation) in values.iter_mut().zip([
        ffi::SQLITE_DBSTATUS_CACHE_MISS,
        ffi::SQLITE_DBSTATUS_CACHE_WRITE,
    ]) {
        let (mut current, mut highwater) = (0, 0);
        // SAFETY: the caller exclusively owns this connection, and both output
        // pointers remain live. Do not reset counters used by other diagnostics.
        if unsafe {
            ffi::sqlite3_db_status(conn.handle(), operation, &mut current, &mut highwater, 0)
        } != ffi::SQLITE_OK
        {
            return None;
        }
        *value = current as u32;
    }
    Some(values)
}

#[derive(Default)]
struct BudgetState {
    next_batch: Option<Instant>,
}

impl BudgetState {
    fn charge(&mut self, now: Instant, elapsed: Duration, usage: RewriteUsage, foreground: bool) {
        // Every completed batch schedules a finite wait, without accumulated
        // idle credit or a foreground gate that could indefinitely stop progress.
        self.next_batch = Some(now + batch_delay(elapsed, usage, foreground));
    }
}

#[derive(Default)]
pub(super) struct RewriteBudget {
    state: Mutex<BudgetState>,
}

impl RewriteBudget {
    pub(super) fn acquire<'a>(&'a self, activity: &'a ActivityGate) -> Option<RewritePermit<'a>> {
        let state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        loop {
            if crate::IS_QUITTING.load(Ordering::Relaxed)
                || crate::IS_UPDATING.load(Ordering::Relaxed)
            {
                return None;
            }
            let delay = state.next_batch.map_or(Duration::ZERO, |next| {
                next.saturating_duration_since(Instant::now())
            });
            if delay.is_zero() {
                break;
            }
            std::thread::sleep(delay.min(Duration::from_millis(100)));
        }
        REWRITE_IO.with(|slot| {
            debug_assert!(slot.get().is_none(), "rewrite permits cannot be nested");
            slot.set(Some(IoAccounting {
                bytes: 0,
                complete: true,
            }));
        });
        Some(RewritePermit {
            state,
            activity,
            started: Instant::now(),
            cpu_before: thread_cpu_time(),
        })
    }
}

#[derive(Clone, Copy)]
pub(super) struct RewriteUsage {
    pub(super) io_bytes: Option<u64>,
    pub(super) cpu_time: Option<Duration>,
}

/// Serializes both rewrite workers, including cooldown, without holding any
/// database or maintenance lock. MutexGuard also keeps accounting on one thread.
pub(super) struct RewritePermit<'a> {
    state: MutexGuard<'a, BudgetState>,
    activity: &'a ActivityGate,
    started: Instant,
    cpu_before: Option<Duration>,
}

impl RewritePermit<'_> {
    pub(super) fn usage(&self) -> RewriteUsage {
        RewriteUsage {
            io_bytes: REWRITE_IO.with(|slot| {
                slot.get()
                    .and_then(|accounting| accounting.complete.then_some(accounting.bytes))
            }),
            cpu_time: self
                .cpu_before
                .zip(thread_cpu_time())
                .map(|(before, after)| after.saturating_sub(before)),
        }
    }
}

impl Drop for RewritePermit<'_> {
    fn drop(&mut self) {
        let usage = self.usage();
        REWRITE_IO.with(|slot| slot.set(None));
        self.state.charge(
            Instant::now(),
            self.started.elapsed(),
            usage,
            self.activity
                .has_recent_foreground_activity(FOREGROUND_WINDOW),
        );
    }
}

fn batch_delay(elapsed: Duration, usage: RewriteUsage, foreground: bool) -> Duration {
    let (bytes_per_second, cpu_interval_multiplier, minimum_rest) = if foreground {
        (FOREGROUND_BYTES_PER_SECOND, 4, Duration::from_millis(25))
    } else {
        (IDLE_BYTES_PER_SECOND, 2, Duration::from_millis(10))
    };
    // Target 50%/25% of one CPU core. Lock and CNG waits already provide rest;
    // only CPU work or owned page I/O can require an additional delay. Missing
    // counters conservatively fall back to wall time for that resource.
    let fallback_interval = elapsed.saturating_mul(cpu_interval_multiplier);
    let cpu_interval = usage.cpu_time.map_or(fallback_interval, |cpu| {
        cpu.saturating_mul(cpu_interval_multiplier)
    });
    let io_interval = usage.io_bytes.map_or(fallback_interval, |bytes| {
        Duration::from_secs_f64(bytes as f64 / bytes_per_second as f64)
    });
    cpu_interval
        .max(io_interval)
        .saturating_sub(elapsed)
        .max(minimum_rest)
}

fn thread_cpu_time() -> Option<Duration> {
    #[cfg(windows)]
    {
        use windows::Win32::Foundation::FILETIME;
        use windows::Win32::System::Threading::{GetCurrentThread, GetThreadTimes};
        let (mut creation, mut exit, mut kernel, mut user) = (
            FILETIME::default(),
            FILETIME::default(),
            FILETIME::default(),
            FILETIME::default(),
        );
        // SAFETY: the borrowed thread pseudo-handle stays valid on this thread
        // and all output values have the documented FILETIME layout and size.
        unsafe {
            GetThreadTimes(
                GetCurrentThread(),
                &mut creation,
                &mut exit,
                &mut kernel,
                &mut user,
            )
        }
        .ok()?;
        let ticks =
            |time: FILETIME| (u64::from(time.dwHighDateTime) << 32) | u64::from(time.dwLowDateTime);
        Some(Duration::from_nanos(
            ticks(kernel)
                .saturating_add(ticks(user))
                .saturating_mul(100),
        ))
    }
    #[cfg(not(windows))]
    {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connection_samples_exclude_other_threads_between_rewrite_locks() {
        use crate::{credential_manager::CredentialManagerState, storage::StorageState};
        use std::sync::Arc;

        let directory = tempfile::tempdir().unwrap();
        let storage = Arc::new(StorageState::new(
            directory.path().to_path_buf(),
            Arc::new(CredentialManagerState::new(directory.path().to_path_buf())),
        ));
        let conn = Connection::open(directory.path().join("budget.db")).unwrap();
        super::super::connection::configure_sqlcipher_connection(&conn, &[7; 32]).unwrap();
        conn.execute_batch(
            "PRAGMA cache_size=-16;
            CREATE TABLE budget_payload(id INTEGER PRIMARY KEY,body BLOB);
            WITH RECURSIVE items(n) AS (VALUES(1) UNION ALL SELECT n+1 FROM items WHERE n<64)
            INSERT INTO budget_payload SELECT n,randomblob(4096) FROM items;",
        )
        .unwrap();
        *storage.db.lock().unwrap() = Some(conn);
        let permit = storage
            .rewrite_budget
            .acquire(&storage.foreground_db_activity)
            .unwrap();
        let noisy_storage = storage.clone();
        std::thread::spawn(move || {
            let guard = noisy_storage
                .get_connection_named("unrelated_reader")
                .unwrap();
            let bytes: i64 = guard
                .as_ref()
                .unwrap()
                .query_row(
                    "SELECT SUM(length(hex(body))/2) FROM budget_payload",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(bytes, 64 * 4096);
        })
        .join()
        .unwrap();
        assert_eq!(permit.usage().io_bytes, Some(0));
        {
            let guard = storage.get_connection_named("rewrite_reader").unwrap();
            // Blob length can be answered from cached record headers. Force a
            // cold payload read rather than assuming an advisory cache limit
            // evicted those headers under every parallel test workload.
            guard.as_ref().unwrap().release_memory().unwrap();
            let bytes: i64 = guard
                .as_ref()
                .unwrap()
                .query_row(
                    "SELECT SUM(length(hex(body))/2) FROM budget_payload",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(bytes, 64 * 4096);
        }
        let owned_bytes = permit.usage().io_bytes.unwrap();
        assert!(
            owned_bytes > 0,
            "bounded SQLite cache must report owned page reads"
        );
        let noisy_storage = storage.clone();
        std::thread::spawn(move || {
            let guard = noisy_storage
                .get_connection_named("unrelated_writer")
                .unwrap();
            guard
                .as_ref()
                .unwrap()
                .execute(
                    "UPDATE budget_payload SET body=randomblob(4096) WHERE id=1",
                    [],
                )
                .unwrap();
        })
        .join()
        .unwrap();
        assert_eq!(permit.usage().io_bytes, Some(owned_bytes));
        {
            let guard = storage.get_connection_named("rewrite_writer").unwrap();
            guard
                .as_ref()
                .unwrap()
                .execute(
                    "UPDATE budget_payload SET body=randomblob(4096) WHERE id=2",
                    [],
                )
                .unwrap();
        }
        assert!(permit.usage().io_bytes.unwrap() > owned_bytes);
    }

    #[test]
    fn waiting_for_locks_or_cng_does_not_generate_more_cpu_debt() {
        let busy = RewriteUsage {
            cpu_time: Some(Duration::from_millis(100)),
            io_bytes: Some(0),
        };
        assert_eq!(
            batch_delay(Duration::from_millis(100), busy, false),
            Duration::from_millis(100)
        );
        assert_eq!(
            batch_delay(Duration::from_millis(100), busy, true),
            Duration::from_millis(300)
        );
        assert_eq!(
            batch_delay(Duration::from_secs(1), busy, false),
            Duration::from_millis(10)
        );
        assert_eq!(
            batch_delay(Duration::from_secs(1), busy, true),
            Duration::from_millis(25)
        );

        let unavailable = RewriteUsage {
            cpu_time: None,
            io_bytes: None,
        };
        assert_eq!(
            batch_delay(Duration::from_millis(200), unavailable, false),
            Duration::from_millis(200)
        );
        assert_eq!(
            batch_delay(Duration::from_millis(200), unavailable, true),
            Duration::from_millis(600)
        );
    }

    #[test]
    fn shared_budget_has_no_idle_credit_and_foreground_work_still_has_a_deadline() {
        let now = Instant::now();
        let elapsed = Duration::from_millis(100);
        let usage = RewriteUsage {
            io_bytes: Some(16 * 1024 * 1024),
            cpu_time: Some(Duration::ZERO),
        };
        let mut state = BudgetState::default();
        state.charge(now, elapsed, usage, false);
        assert_eq!(state.next_batch.unwrap() - now, Duration::from_millis(900));
        let after_idle = now + Duration::from_secs(3600);
        state.charge(after_idle, elapsed, usage, false);
        assert_eq!(
            state.next_batch.unwrap() - after_idle,
            Duration::from_millis(900)
        );
        state.charge(after_idle, elapsed, usage, true);
        assert_eq!(
            state.next_batch.unwrap() - after_idle,
            Duration::from_millis(3900)
        );
    }
}
