//! A separate OS thread observes the bounded synchronous service lane. An IPC
//! deadline may expire without cancelling DPAPI; a lasting stall restarts the
//! whole service so SQLite recovery and SCM failure actions remain effective.
use super::diagnostics;
use crate::protocol::*;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex, OnceLock,
};
use std::time::{Duration, Instant};

const WORK_TIMEOUT: Duration = Duration::from_secs(45);
const POLL_TIMEOUT: Duration = Duration::from_secs(15);
static HEALTH: OnceLock<Arc<Health>> = OnceLock::new();

struct Health {
    work_started: Mutex<Option<Instant>>,
    last_poll: Mutex<Instant>,
    serving: AtomicBool,
}

impl Health {
    fn new() -> Self {
        Self {
            work_started: Mutex::new(None),
            last_poll: Mutex::new(Instant::now()),
            serving: AtomicBool::new(false),
        }
    }
    fn begin(self: &Arc<Self>) -> Work {
        *self.work_started.lock().unwrap_or_else(|e| e.into_inner()) = Some(Instant::now());
        Work(self.clone())
    }
    fn stalled(&self, now: Instant, work_timeout: Duration, poll_timeout: Duration) -> bool {
        self.work_started
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_some_and(|start| now.saturating_duration_since(start) >= work_timeout)
            || (self.serving.load(Ordering::Acquire)
                && now.saturating_duration_since(
                    *self.last_poll.lock().unwrap_or_else(|e| e.into_inner()),
                ) >= poll_timeout)
    }
    fn resume(&self, now: Instant) {
        let mut started = self.work_started.lock().unwrap_or_else(|e| e.into_inner());
        if started.is_some() {
            *started = Some(now);
        }
        *self.last_poll.lock().unwrap_or_else(|e| e.into_inner()) = now;
    }
}

pub(super) struct Work(Arc<Health>);
impl Drop for Work {
    fn drop(&mut self) {
        *self
            .0
            .work_started
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = None;
    }
}

pub(super) struct Watchdog {
    stop: Arc<AtomicBool>,
}
impl Drop for Watchdog {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
    }
}

fn monitor(
    health: Arc<Health>,
    stop: Arc<AtomicBool>,
    interval: Duration,
    work_timeout: Duration,
    poll_timeout: Duration,
    on_stall: impl FnOnce() + Send + 'static,
) -> std::io::Result<()> {
    std::thread::Builder::new()
        .name("key-service-watchdog".into())
        .spawn(move || {
            let mut previous_check = Instant::now();
            while !stop.load(Ordering::Acquire) {
                let now = Instant::now();
                // Suspend/resume or machine-wide scheduler starvation pauses this
                // independent thread too. Allow a full recovery window afterward.
                if now.saturating_duration_since(previous_check) > Duration::from_secs(10) {
                    health.resume(now);
                }
                previous_check = now;
                if health.stalled(now, work_timeout, poll_timeout) {
                    on_stall();
                    return;
                }
                std::thread::sleep(interval);
            }
        })
        .map(|_| ())
}

pub(super) fn start() -> Result<Watchdog> {
    let health = Arc::new(Health::new());
    HEALTH
        .set(health.clone())
        .map_err(|_| BrokerError::Unavailable)?;
    let stop = Arc::new(AtomicBool::new(false));
    monitor(
        health,
        stop.clone(),
        Duration::from_secs(1),
        WORK_TIMEOUT,
        POLL_TIMEOUT,
        || {
            diagnostics::event(
                "error",
                "service_unresponsive",
                "watchdog",
                Some(BrokerError::Unavailable),
            );
            // Process termination safely releases impersonation and file handles.
            // The next process recovers its transactional ledger before serving.
            std::process::exit(1);
        },
    )
    .map_err(|_| BrokerError::Unavailable)?;
    Ok(Watchdog { stop })
}

pub(super) fn begin_work() -> Option<Work> {
    HEALTH.get().map(Health::begin)
}
pub(super) fn poll() {
    if let Some(health) = HEALTH.get() {
        *health.last_poll.lock().unwrap_or_else(|e| e.into_inner()) = Instant::now();
        health.serving.store(true, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn watchdog_observes_work_that_survives_an_ipc_timeout_and_completion_clears_it() {
        let health = Arc::new(Health::new());
        let health_in_job = health.clone();
        let (started, started_rx) = std::sync::mpsc::channel();
        let (release, release_rx) = std::sync::mpsc::channel();
        let handle = tokio::task::spawn_blocking(move || {
            let _work = health_in_job.begin();
            started.send(()).unwrap();
            release_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        });
        started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(tokio::time::timeout(Duration::from_millis(5), async {
            while !handle.is_finished() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .is_err());
        let (restart, restart_rx) = std::sync::mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        monitor(
            health.clone(),
            stop.clone(),
            Duration::from_millis(5),
            Duration::from_millis(50),
            Duration::from_secs(1),
            move || restart.send(()).unwrap(),
        )
        .unwrap();
        restart_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        release.send(()).unwrap();
        handle.await.unwrap();
        stop.store(true, Ordering::Release);
        assert!(!health.stalled(
            Instant::now() + Duration::from_secs(60),
            WORK_TIMEOUT,
            POLL_TIMEOUT
        ));
    }

    #[test]
    fn idle_service_does_not_timeout_but_a_stalled_accept_loop_does() {
        let health = Health::new();
        let now = Instant::now() + Duration::from_secs(60);
        assert!(!health.stalled(now, WORK_TIMEOUT, POLL_TIMEOUT));
        health.serving.store(true, Ordering::Release);
        assert!(health.stalled(now, WORK_TIMEOUT, POLL_TIMEOUT));
        health.resume(now);
        assert!(!health.stalled(now, WORK_TIMEOUT, POLL_TIMEOUT));
    }
}
