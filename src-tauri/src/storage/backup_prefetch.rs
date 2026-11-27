//! Bounded screenshot prefetching. Only the archive thread authenticates and
//! writes payloads; one reader overlaps file I/O without retaining extra keys.

use super::backup_format::check_cancelled;
use carbonpaper_backup_core::ZeroizingBuffer;
use std::{
    fs::File,
    io::{self, Read, Seek, SeekFrom},
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError},
        Arc, Condvar, Mutex,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

const PREFETCH_BYTES: usize = 32 * 1024 * 1024;
const PREFETCH_FILES: usize = 8;
const BUFFER_POOL_BYTES: usize = 8 * 1024 * 1024;
const WAIT_INTERVAL: Duration = Duration::from_millis(50);

struct Shared {
    cancelled: Arc<AtomicBool>,
    stopping: AtomicBool,
    used_bytes: Mutex<usize>,
    byte_limit: usize,
    available: Condvar,
    pool: Mutex<Vec<ZeroizingBuffer>>,
    read_ns: AtomicU64,
}

impl Shared {
    fn check(&self) -> io::Result<()> {
        check_cancelled(&self.cancelled)?;
        check_cancelled(&self.stopping)
    }

    fn reserve(self: &Arc<Self>, bytes: usize) -> io::Result<Reservation> {
        let mut used = self
            .used_bytes
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        loop {
            self.check()?;
            if bytes <= self.byte_limit.saturating_sub(*used) {
                *used += bytes;
                return Ok(Reservation {
                    shared: self.clone(),
                    bytes,
                });
            }
            (used, _) = self
                .available
                .wait_timeout(used, WAIT_INTERVAL)
                .unwrap_or_else(|error| error.into_inner());
        }
    }

    fn take_buffer(&self) -> ZeroizingBuffer {
        self.pool
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .pop()
            .unwrap_or_default()
    }

    fn recycle(&self, mut buffer: ZeroizingBuffer) {
        let mut pool = self.pool.lock().unwrap_or_else(|error| error.into_inner());
        let retained: usize = pool.iter().map(|buffer| buffer.capacity()).sum();
        if pool.len() < PREFETCH_FILES
            && buffer.capacity() <= BUFFER_POOL_BYTES.saturating_sub(retained)
        {
            buffer.clear();
            pool.push(buffer);
        }
    }

    fn record_read(&self, started: Instant) {
        self.read_ns.fetch_add(
            started.elapsed().as_nanos().min(u64::MAX as u128) as u64,
            Ordering::Relaxed,
        );
    }
}

struct Reservation {
    shared: Arc<Shared>,
    bytes: usize,
}

impl Drop for Reservation {
    fn drop(&mut self) {
        let mut used = self
            .shared
            .used_bytes
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        *used -= self.bytes;
        self.shared.available.notify_all();
    }
}

struct FileReader<'a> {
    file: &'a mut File,
    cancelled: &'a AtomicBool,
    stopping: Option<&'a AtomicBool>,
}

impl Read for FileReader<'_> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        check_cancelled(self.cancelled)?;
        if let Some(stopping) = self.stopping {
            check_cancelled(stopping)?;
        }
        self.file.read(output)
    }
}

pub(super) struct FilePayload {
    buffer: Option<ZeroizingBuffer>,
    deferred: Option<File>,
    shared: Arc<Shared>,
    _reservation: Option<Reservation>,
}

impl FilePayload {
    pub(super) fn bytes(&mut self, name: &str) -> Result<&[u8], String> {
        if let Some(mut file) = self.deferred.take() {
            // Oversized or growing files are read on the archive thread, outside
            // the prefetch queue. At most one such payload is processed at a time.
            let mut buffer = self.shared.take_buffer();
            let started = Instant::now();
            let result = buffer.read_from(
                &mut FileReader {
                    file: &mut file,
                    cancelled: &self.shared.cancelled,
                    stopping: None,
                },
                None,
            );
            self.shared.record_read(started);
            self.buffer = Some(buffer);
            result.map_err(|error| format!("Failed to read screenshot {name}: {error}"))?;
        }
        self.buffer
            .as_ref()
            .map(|buffer| buffer.as_slice())
            .ok_or_else(|| "Screenshot prefetch payload is unavailable".into())
    }
}

impl Drop for FilePayload {
    fn drop(&mut self) {
        if let Some(buffer) = self.buffer.take() {
            self.shared.recycle(buffer);
        }
    }
}

pub(super) struct PrefetchedFile {
    pub(super) name: String,
    pub(super) size: u64,
    pub(super) payload: Option<FilePayload>,
}

fn prepare_file(
    shared: &Arc<Shared>,
    path: PathBuf,
    name: String,
    size: u64,
) -> Result<PrefetchedFile, String> {
    shared.check().map_err(|error| error.to_string())?;
    let open_started = Instant::now();
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            shared.record_read(open_started);
            return Ok(PrefetchedFile {
                name,
                size,
                payload: None,
            });
        }
        Err(error) => return Err(format!("Failed to open screenshot {name}: {error}")),
    };
    let actual_size = file
        .metadata()
        .map_err(|error| format!("Failed to inspect screenshot {name}: {error}"))?
        .len();
    shared.record_read(open_started);
    let deferred = |file| PrefetchedFile {
        name: name.clone(),
        size,
        payload: Some(FilePayload {
            buffer: None,
            deferred: Some(file),
            shared: shared.clone(),
            _reservation: None,
        }),
    };
    let Some(actual_size) = usize::try_from(actual_size)
        .ok()
        .filter(|size| *size <= shared.byte_limit)
    else {
        return Ok(deferred(file));
    };
    let mut buffer = shared.take_buffer();
    let reserved = actual_size.max(buffer.capacity());
    if reserved > shared.byte_limit {
        shared.recycle(buffer);
        return Ok(deferred(file));
    }
    // Reserve before allocating so a reader blocked on backpressure cannot
    // retain another full-size allocation outside the byte budget.
    let reservation = shared
        .reserve(reserved)
        .map_err(|error| error.to_string())?;
    buffer
        .reserve_capacity(reserved)
        .map_err(|error| error.to_string())?;
    let read_started = Instant::now();
    let read = buffer.read_from(
        &mut FileReader {
            file: &mut file,
            cancelled: &shared.cancelled,
            stopping: Some(&shared.stopping),
        },
        Some(reserved),
    );
    shared.record_read(read_started);
    if !read.map_err(|error| format!("Failed to read screenshot {name}: {error}"))? {
        // Preserve support for a file that grew after its inventory/metadata
        // read, while preventing that growth from overrunning the queue budget.
        file.seek(SeekFrom::Start(0))
            .map_err(|error| error.to_string())?;
        shared.recycle(buffer);
        drop(reservation);
        return Ok(deferred(file));
    }
    Ok(PrefetchedFile {
        name,
        size,
        payload: Some(FilePayload {
            buffer: Some(buffer),
            deferred: None,
            shared: shared.clone(),
            _reservation: Some(reservation),
        }),
    })
}

pub(super) struct ImagePrefetcher {
    receiver: Option<Receiver<Result<PrefetchedFile, String>>>,
    worker: Option<JoinHandle<()>>,
    shared: Arc<Shared>,
}

impl ImagePrefetcher {
    pub(super) fn new(
        files: Vec<(PathBuf, String, u64)>,
        cancelled: Arc<AtomicBool>,
    ) -> Result<Self, String> {
        Self::with_limits(files, cancelled, PREFETCH_BYTES, PREFETCH_FILES)
    }

    fn with_limits(
        files: Vec<(PathBuf, String, u64)>,
        cancelled: Arc<AtomicBool>,
        byte_limit: usize,
        file_limit: usize,
    ) -> Result<Self, String> {
        let shared = Arc::new(Shared {
            cancelled,
            stopping: AtomicBool::new(false),
            used_bytes: Mutex::new(0),
            byte_limit,
            available: Condvar::new(),
            pool: Mutex::new(Vec::new()),
            read_ns: AtomicU64::new(0),
        });
        let (sender, receiver) = mpsc::sync_channel(file_limit);
        let reader = shared.clone();
        let worker = thread::Builder::new()
            .name("backup-prefetch".into())
            .spawn(move || {
                for (path, name, size) in files {
                    let result = prepare_file(&reader, path, name, size);
                    let failed = result.is_err();
                    if sender.send(result).is_err() || failed {
                        break;
                    }
                }
            })
            .map_err(|error| format!("Failed to start screenshot prefetch: {error}"))?;
        Ok(Self {
            receiver: Some(receiver),
            worker: Some(worker),
            shared,
        })
    }

    pub(super) fn next(&mut self) -> Result<Option<PrefetchedFile>, String> {
        loop {
            check_cancelled(&self.shared.cancelled).map_err(|error| error.to_string())?;
            match self
                .receiver
                .as_ref()
                .ok_or("Screenshot prefetch is closed")?
                .recv_timeout(WAIT_INTERVAL)
            {
                Ok(result) => return result.map(Some),
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => {
                    if let Some(worker) = self.worker.take() {
                        worker
                            .join()
                            .map_err(|_| "Screenshot prefetch worker panicked")?;
                    }
                    return Ok(None);
                }
            }
        }
    }

    pub(super) fn read_ms(&self) -> u64 {
        self.shared.read_ns.load(Ordering::Relaxed) / 1_000_000
    }
}

impl Drop for ImagePrefetcher {
    fn drop(&mut self) {
        self.shared.stopping.store(true, Ordering::Release);
        self.shared.available.notify_all();
        // Disconnect before joining: a full queue must not keep the worker
        // asleep in send() after cancellation, authentication failure or I/O error.
        drop(self.receiver.take());
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files(root: &std::path::Path, sizes: &[usize]) -> Vec<(PathBuf, String, u64)> {
        sizes
            .iter()
            .enumerate()
            .map(|(index, size)| {
                let name = format!("image-{index}");
                let path = root.join(&name);
                std::fs::write(&path, vec![index as u8; *size]).unwrap();
                (path, name, *size as u64)
            })
            .collect()
    }

    #[test]
    fn prefetch_preserves_order_missing_files_and_oversized_payloads() {
        let root = tempfile::tempdir().unwrap();
        let input = files(root.path(), &[8, 9, 40, 7]);
        std::fs::remove_file(&input[1].0).unwrap();
        let mut prefetch =
            ImagePrefetcher::with_limits(input, Arc::new(AtomicBool::new(false)), 16, 2).unwrap();
        for (index, size) in [8, 9, 40, 7].into_iter().enumerate() {
            let mut item = prefetch.next().unwrap().unwrap();
            assert_eq!(item.name, format!("image-{index}"));
            if index == 1 {
                assert!(item.payload.is_none());
            } else {
                assert_eq!(
                    item.payload.as_mut().unwrap().bytes(&item.name).unwrap(),
                    vec![index as u8; size]
                );
            }
        }
        assert!(prefetch.next().unwrap().is_none());
    }

    #[test]
    fn stopping_unblocks_both_a_full_queue_and_a_byte_budget_wait() {
        for byte_limit in [8, 64] {
            let root = tempfile::tempdir().unwrap();
            let cancelled = Arc::new(AtomicBool::new(false));
            let mut prefetch = ImagePrefetcher::with_limits(
                files(root.path(), &[8; 8]),
                cancelled.clone(),
                byte_limit,
                1,
            )
            .unwrap();
            let held = prefetch.next().unwrap().unwrap();
            cancelled.store(true, Ordering::Release);
            assert!(prefetch.next().is_err());
            let (completed, receive) = mpsc::channel();
            let cleanup = thread::spawn(move || {
                drop(prefetch);
                completed.send(()).unwrap();
            });
            // Keep the first payload's byte reservation live until shutdown has
            // finished, exercising the cancellation/backpressure deadlock path.
            receive.recv_timeout(Duration::from_secs(5)).unwrap();
            cleanup.join().unwrap();
            drop(held);
        }
    }
}
