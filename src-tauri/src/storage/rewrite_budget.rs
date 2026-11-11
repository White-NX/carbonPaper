//! Shared pacing for online rewrites. A permit is acquired before database or
//! maintenance locks; its next delay is paid only after those locks are released.
use super::connection::ActivityGate;
use std::sync::atomic::Ordering;
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

const IDLE_BYTES_PER_SECOND: u64 = 16 * 1024 * 1024;
const FOREGROUND_BYTES_PER_SECOND: u64 = 4 * 1024 * 1024;
const FOREGROUND_WINDOW: Duration = Duration::from_secs(2);

#[derive(Default)]
struct BudgetState {
    next_batch: Option<Instant>,
}

impl BudgetState {
    fn charge(&mut self, now: Instant, elapsed: Duration, io_bytes: Option<u64>, foreground: bool) {
        // There is no accumulated idle credit or rolling foreground admission
        // gate. Every completed batch schedules one finite wait, even under
        // continuous foreground activity.
        self.next_batch = Some(now + batch_delay(elapsed, io_bytes, foreground));
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
        Some(RewritePermit {
            state,
            activity,
            started: Instant::now(),
            io_before: process_io_bytes(),
        })
    }
}

/// Serializes the two rewrite workers, including their cooldown. This mutex
/// never participates in foreground queries or exclusive database maintenance.
pub(super) struct RewritePermit<'a> {
    state: MutexGuard<'a, BudgetState>,
    activity: &'a ActivityGate,
    started: Instant,
    io_before: Option<u64>,
}

impl Drop for RewritePermit<'_> {
    fn drop(&mut self) {
        let io_bytes = self
            .io_before
            .zip(process_io_bytes())
            .map(|(before, after)| after.saturating_sub(before));
        self.state.charge(
            Instant::now(),
            self.started.elapsed(),
            io_bytes,
            self.activity.has_recent_read_activity(FOREGROUND_WINDOW),
        );
    }
}

fn batch_delay(elapsed: Duration, io_bytes: Option<u64>, foreground: bool) -> Duration {
    let (bytes_per_second, rest_multiplier, minimum_rest) = if foreground {
        (FOREGROUND_BYTES_PER_SECOND, 9, Duration::from_millis(200))
    } else {
        (IDLE_BYTES_PER_SECOND, 3, Duration::from_millis(50))
    };
    let io_interval =
        Duration::from_secs_f64(io_bytes.unwrap_or(0) as f64 / bytes_per_second as f64);
    // Bound average logical I/O as well as duty cycle (25% idle, 10% foreground).
    // A slow batch must still rest in proportion to its work; CNG latency does
    // not replace this rest, and missing counters retain time-based pacing.
    io_interval
        .saturating_sub(elapsed)
        .max(elapsed.saturating_mul(rest_multiplier))
        .max(minimum_rest)
}

fn process_io_bytes() -> Option<u64> {
    #[cfg(windows)]
    {
        use windows::Win32::System::Threading::{
            GetCurrentProcess, GetProcessIoCounters, IO_COUNTERS,
        };
        let mut counters = IO_COUNTERS::default();
        // SAFETY: the borrowed process pseudo-handle stays valid and the output
        // points to a live, correctly sized IO_COUNTERS value.
        unsafe { GetProcessIoCounters(GetCurrentProcess(), &mut counters) }.ok()?;
        // These are process-wide logical transfers, including cached reads and
        // concurrent foreground I/O. Charging that overlap is conservative.
        // No counters are sampled during cooldown, so other work cannot keep
        // postponing the next rewrite. SQLite commits may still produce bursts.
        Some(
            counters
                .ReadTransferCount
                .saturating_add(counters.WriteTransferCount),
        )
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
    fn slow_batches_keep_resting_when_io_counters_are_unavailable() {
        let elapsed = Duration::from_millis(200);
        assert_eq!(
            batch_delay(elapsed, None, false),
            Duration::from_millis(600)
        );
        assert_eq!(
            batch_delay(elapsed, None, true),
            Duration::from_millis(1800)
        );
        assert!(batch_delay(Duration::ZERO, Some(0), false) > Duration::ZERO);
    }

    #[test]
    fn shared_budget_has_no_idle_credit_and_foreground_work_still_has_a_deadline() {
        let now = Instant::now();
        let elapsed = Duration::from_millis(100);
        let io_bytes = Some(16 * 1024 * 1024);
        let mut state = BudgetState::default();
        state.charge(now, elapsed, io_bytes, false);
        assert_eq!(state.next_batch.unwrap() - now, Duration::from_millis(900));

        // Vacuum uses the same state as compaction. A long idle gap must not
        // allow the next large rewrite to spend a banked burst allowance.
        let after_idle = now + Duration::from_secs(3600);
        state.charge(after_idle, elapsed, io_bytes, false);
        assert_eq!(
            state.next_batch.unwrap() - after_idle,
            Duration::from_millis(900)
        );

        state.charge(after_idle, elapsed, io_bytes, true);
        assert_eq!(
            state.next_batch.unwrap() - after_idle,
            Duration::from_millis(3900)
        );
        let activity = ActivityGate::default();
        let reader = activity.read("foreground_budget_test");
        assert!(activity.has_recent_read_activity(FOREGROUND_WINDOW));
        drop(reader);
        assert!(activity.has_recent_read_activity(FOREGROUND_WINDOW));
    }
}
