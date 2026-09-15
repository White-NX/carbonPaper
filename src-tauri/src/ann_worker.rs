//! A persistent builder: pausing retains RAM, while only verified, fsynced
//! checkpoints are offered to the desktop for a durable cursor commit.
use crate::ann_format::{self, Header, MappedFlatIndex};
use crate::ann_protocol::*;
use crate::semantic_metrics::cpu_ms;
use sha2::{Digest, Sha256};
use std::borrow::Cow;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};
use usearch::{Index, IndexOptions, MetricKind, ScalarKind};

#[derive(Default)]
struct Control(Mutex<(u64, bool)>);
impl Control {
    fn begin(&self, id: u64) {
        *self.0.lock().unwrap() = (id, false);
    }
    fn cancel(&self, id: u64) {
        let mut active = self.0.lock().unwrap();
        if active.0 == id {
            active.1 = true;
        }
    }
    fn check(&self) -> Result<(), String> {
        if self.0.lock().unwrap().1 {
            Err("background_paused: ANN request revoked".into())
        } else {
            Ok(())
        }
    }
    fn throttle(&self, bytes: usize, background: bool) -> Result<(), String> {
        if background {
            let until = Instant::now() + Duration::from_secs_f64(bytes as f64 / IO_RATE as f64);
            while Instant::now() < until {
                self.check()?;
                std::thread::sleep(
                    until
                        .saturating_duration_since(Instant::now())
                        .min(Duration::from_millis(10)),
                );
            }
        }
        self.check()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Flat,
    ReadCheckpoint,
    Restore,
    Build,
    Serialize,
    Write,
    ReadBack,
    VerifyRestore,
    Validate,
}
impl Phase {
    fn operation(self) -> &'static str {
        match self {
            Self::Flat | Self::ReadCheckpoint | Self::Write | Self::ReadBack => "ann_io",
            Self::Restore | Self::VerifyRestore => "ann_restore",
            Self::Build => "ann_insert",
            Self::Serialize => "ann_serialize",
            Self::Validate => "ann_validate",
        }
    }
}

struct Session {
    flat_path: PathBuf,
    flat_bytes: Vec<u8>,
    flat_map: Option<MappedFlatIndex>,
    header: Option<Header>,
    read_path: Option<PathBuf>,
    read_bytes: Vec<u8>,
    expected_checksum: Option<String>,
    index: Option<Index>,
    verify_index: Option<Index>,
    built: u64,
    committed: u64,
    compute_ms: f64,
    graph_bytes: Vec<u8>,
    output: Option<(PathBuf, File, usize)>,
    phase: Phase,
}

impl Session {
    fn new(flat: String, checkpoint: Option<String>, checksum: Option<String>, rows: u64) -> Self {
        Self {
            flat_path: flat.into(),
            flat_bytes: Vec::new(),
            flat_map: None,
            header: None,
            read_path: checkpoint.map(PathBuf::from),
            read_bytes: Vec::new(),
            expected_checksum: checksum,
            index: None,
            verify_index: None,
            built: rows,
            committed: rows,
            compute_ms: 0.0,
            graph_bytes: Vec::new(),
            output: None,
            phase: Phase::Flat,
        }
    }
    fn options(&self) -> IndexOptions {
        let h = self.header.as_ref().unwrap();
        IndexOptions {
            dimensions: h.dimensions as usize,
            metric: MetricKind::IP,
            quantization: ScalarKind::I8,
            connectivity: h.connectivity as usize,
            expansion_add: h.expansion_add as usize,
            expansion_search: h.expansion_search as usize,
            ..Default::default()
        }
    }
    fn vector(&self, ordinal: usize) -> Result<Cow<'_, [f32]>, String> {
        if let Some(mapped) = &self.flat_map {
            return mapped.vector(ordinal).map(Cow::Borrowed);
        }
        let h = self.header.as_ref().ok_or("ANN flat header missing")?;
        if ordinal >= h.row_count as usize {
            return Err("ANN ordinal outside frozen input".into());
        }
        let start = h.vectors_offset as usize + ordinal * h.dimensions as usize * 4;
        let bytes = self
            .flat_bytes
            .get(start..start + h.dimensions as usize * 4)
            .ok_or("truncated ANN input")?;
        Ok(Cow::Owned(
            bytes
                .chunks_exact(4)
                .map(|b| f32::from_le_bytes(b.try_into().unwrap()))
                .collect(),
        ))
    }
    fn new_index(&self) -> Result<Index, String> {
        Index::new(&self.options()).map_err(|e| e.to_string())
    }
    fn restore_index(&self, bytes: &[u8], rows: u64) -> Result<Index, String> {
        let index = self.new_index()?;
        index.load_from_buffer(bytes).map_err(|e| e.to_string())?;
        index.change_expansion_search(self.options().expansion_search);
        if index.size() as u64 != rows
            || index.dimensions() != self.options().dimensions
            || index.metric_kind() != MetricKind::IP
            || index.scalar_kind() != ScalarKind::I8
            || index.connectivity() != self.options().connectivity
        {
            return Err("ANN checkpoint contract mismatch".into());
        }
        index
            .reserve_capacity_and_threads(self.header.as_ref().unwrap().row_count as usize, 1)
            .map_err(|e| e.to_string())?;
        Ok(index)
    }

    fn step(
        &mut self,
        background: bool,
        control: &Control,
        reply: &mut AnnReply,
    ) -> Result<(), String> {
        control.check()?;
        let phase = self.phase;
        let start = Instant::now();
        let cpu = cpu_ms();
        let mut io_bytes = 0;
        match phase {
            Phase::Flat => {
                let size = self.flat_path.metadata().map_err(|e| e.to_string())?.len();
                if size > 64 * 1024 * 1024 {
                    if background {
                        return Err("background_paused: ANN input exceeds B limit".into());
                    }
                    let flat = MappedFlatIndex::open(&self.flat_path)?;
                    self.header = Some(flat.header.clone());
                    self.flat_map = Some(flat);
                } else {
                    io_bytes = read_chunk(&self.flat_path, &mut self.flat_bytes)?;
                    if self.flat_bytes.len() as u64 == size {
                        let h = Header::decode(&self.flat_bytes)?;
                        if h.file_len()? != size {
                            return Err("ANN input length mismatch".into());
                        }
                        self.header = Some(h);
                    }
                }
                if self.header.is_some() {
                    if self.read_path.is_some() {
                        self.phase = Phase::ReadCheckpoint;
                    } else {
                        let index = self.new_index()?;
                        index
                            .reserve_capacity_and_threads(
                                self.header.as_ref().unwrap().row_count as usize,
                                1,
                            )
                            .map_err(|e| e.to_string())?;
                        self.index = Some(index);
                        self.phase = Phase::Build;
                    }
                }
            }
            Phase::ReadCheckpoint | Phase::ReadBack => {
                let path = self
                    .read_path
                    .as_ref()
                    .ok_or("ANN checkpoint path missing")?;
                io_bytes = read_chunk(path, &mut self.read_bytes)?;
                if self.read_bytes.len() as u64 == path.metadata().map_err(|e| e.to_string())?.len()
                {
                    let checksum_started = Instant::now();
                    let checksum_cpu = cpu_ms();
                    let digest = hex::encode(Sha256::digest(&self.read_bytes));
                    reply.samples.push(AnnCost {
                        operation: "ann_checksum".into(),
                        elapsed_ms: (checksum_started.elapsed().as_secs_f64() * 1000.0).max(0.001),
                        cpu_ms: (cpu_ms() - checksum_cpu).max(0.0),
                    });
                    if self.expected_checksum.as_deref() != Some(digest.as_str()) {
                        return Err("ANN checkpoint checksum mismatch".into());
                    }
                    self.phase = if phase == Phase::ReadCheckpoint {
                        Phase::Restore
                    } else {
                        Phase::VerifyRestore
                    };
                }
            }
            Phase::Restore => {
                self.index = Some(self.restore_index(&self.read_bytes, self.built)?);
                self.read_bytes.clear();
                self.phase = Phase::Build;
            }
            Phase::Build => {
                let total = self.header.as_ref().unwrap().row_count;
                let max_rows = if background { 1 } else { CHECKPOINT_ROWS };
                for _ in 0..max_rows {
                    control.check()?;
                    if self.built >= total {
                        break;
                    }
                    let insert_start = Instant::now();
                    let insert_cpu = cpu_ms();
                    self.index
                        .as_ref()
                        .unwrap()
                        .add(self.built + 1, self.vector(self.built as usize)?.as_ref())
                        .map_err(|e| e.to_string())?;
                    self.built += 1;
                    let elapsed = insert_start.elapsed().as_secs_f64() * 1000.0;
                    self.compute_ms += elapsed;
                    if reply.samples.len() == 64 {
                        reply.samples.remove(0);
                    }
                    reply.samples.push(AnnCost {
                        operation: "ann_insert".into(),
                        elapsed_ms: elapsed.max(0.001),
                        cpu_ms: (cpu_ms() - insert_cpu).max(0.0),
                    });
                    if self.built - self.committed >= CHECKPOINT_ROWS
                        || self.compute_ms >= CHECKPOINT_COMPUTE_MS
                    {
                        break;
                    }
                }
                if self.built >= total
                    || self.built - self.committed >= CHECKPOINT_ROWS
                    || self.compute_ms >= CHECKPOINT_COMPUTE_MS
                {
                    self.phase = Phase::Serialize;
                }
            }
            Phase::Serialize => {
                let index = self.index.as_ref().unwrap();
                let size = index.serialized_length();
                if background && size > 64 * 1024 * 1024 {
                    return Err("background_paused: ANN graph exceeds B limit".into());
                }
                self.graph_bytes.resize(size, 0);
                index
                    .save_to_buffer(&mut self.graph_bytes)
                    .map_err(|e| e.to_string())?;
                control.check()?;
                let path = self
                    .flat_path
                    .with_extension(format!("checkpoint-{}.partial", self.built));
                // This is an unpublished scratch file belonging to this build.
                let file = OpenOptions::new()
                    .write(true)
                    .create(true)
                    .truncate(true)
                    .open(&path)
                    .map_err(|e| e.to_string())?;
                self.output = Some((path, file, 0));
                self.phase = Phase::Write;
            }
            Phase::Write => {
                let (path, file, cursor) = self.output.as_mut().unwrap();
                let end = (*cursor + IO_CHUNK).min(self.graph_bytes.len());
                file.write_all(&self.graph_bytes[*cursor..end])
                    .map_err(|e| e.to_string())?;
                io_bytes = end - *cursor;
                *cursor = end;
                if end == self.graph_bytes.len() {
                    file.sync_all().map_err(|e| e.to_string())?;
                    self.read_path = Some(path.clone());
                    self.read_bytes.clear();
                    self.expected_checksum = Some(hex::encode(Sha256::digest(&self.graph_bytes)));
                    self.graph_bytes.clear();
                    self.phase = Phase::ReadBack;
                }
            }
            Phase::VerifyRestore => {
                self.verify_index = Some(self.restore_index(&self.read_bytes, self.built)?);
                self.read_bytes.clear();
                self.phase = Phase::Validate;
            }
            Phase::Validate => {
                let restored = self.verify_index.as_ref().unwrap();
                // Probe both ends of the committed prefix, including a segment
                // added after restore. Approximate search may legitimately tie.
                for ordinal in [0, self.built.saturating_sub(1)] {
                    control.check()?;
                    if self.built == 0 {
                        break;
                    }
                    let vector = self.vector(ordinal as usize)?;
                    let mut recovered = vec![0f32; vector.len()];
                    let found = restored
                        .get(ordinal + 1, &mut recovered)
                        .map_err(|e| e.to_string())?;
                    ann_format::validate_ann_recovered_probe(vector.as_ref(), &recovered, found)?;
                    let result = restored
                        .search(vector.as_ref(), 1)
                        .map_err(|e| e.to_string())?;
                    ann_format::validate_ann_search_result(
                        &result.keys,
                        &result.distances,
                        self.built,
                    )?;
                }
                self.verify_index = None;
                let (path, file, _) = self.output.take().unwrap();
                file.sync_all().map_err(|e| e.to_string())?;
                drop(file);
                reply.checkpoint = Some(path.to_string_lossy().into_owned());
                reply.checksum = self.expected_checksum.clone();
                self.committed = self.built;
                self.compute_ms = 0.0;
                self.phase = Phase::Build;
            }
        }
        if phase != Phase::Build {
            let checksum_ms: f64 = reply
                .samples
                .iter()
                .filter(|s| s.operation == "ann_checksum")
                .map(|s| s.elapsed_ms)
                .sum();
            let checksum_cpu: f64 = reply
                .samples
                .iter()
                .filter(|s| s.operation == "ann_checksum")
                .map(|s| s.cpu_ms)
                .sum();
            reply.samples.push(AnnCost {
                operation: phase.operation().into(),
                elapsed_ms: (start.elapsed().as_secs_f64() * 1000.0 - checksum_ms).max(0.001),
                cpu_ms: (cpu_ms() - cpu - checksum_cpu).max(0.0),
            });
        }
        reply.built = self.built;
        reply.graph_bytes = self
            .index
            .as_ref()
            .map_or(0, |i| i.serialized_length() as u64);
        reply.phase = self.phase.operation().into();
        // Throttling is intentionally outside execution measurements.
        control.throttle(io_bytes, background)?;
        Ok(())
    }
}

fn read_chunk(path: &Path, buffer: &mut Vec<u8>) -> Result<usize, String> {
    let mut file = File::open(path).map_err(|e| e.to_string())?;
    let length = file.metadata().map_err(|e| e.to_string())?.len();
    if length > 1024 * 1024 * 1024 {
        return Err("ANN checkpoint exceeds worker memory budget".into());
    }
    file.seek(SeekFrom::Start(buffer.len() as u64))
        .map_err(|e| e.to_string())?;
    let mut chunk = [0u8; IO_CHUNK];
    let count = file.read(&mut chunk).map_err(|e| e.to_string())?;
    buffer.extend_from_slice(&chunk[..count]);
    if count == 0 && buffer.len() as u64 != length {
        return Err("ANN input truncated during read".into());
    }
    Ok(count)
}

pub fn run() -> Result<(), String> {
    let control = Arc::new(Control::default());
    let reader_control = control.clone();
    let (tx, rx) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let stdin = std::io::stdin();
        let mut reader = BufReader::new(stdin.lock());
        loop {
            let mut line = String::new();
            if reader
                .by_ref()
                .take(16 * 1024)
                .read_line(&mut line)
                .ok()
                .filter(|n| *n > 0)
                .is_none()
            {
                break;
            }
            let Ok(request) = serde_json::from_str::<AnnRequest>(&line) else {
                break;
            };
            if let AnnRequest::Cancel { request_id } = request {
                reader_control.cancel(request_id);
                continue;
            }
            reader_control.begin(request.id());
            if tx.send(request).is_err() {
                break;
            }
        }
    });
    let stdout = std::io::stdout();
    let mut writer = stdout.lock();
    let mut session: Option<Session> = None;
    for request in rx {
        let id = request.id();
        let mut reply = AnnReply {
            request_id: id,
            phase: "ann_io".into(),
            built: 0,
            graph_bytes: 0,
            checkpoint: None,
            checksum: None,
            samples: Vec::new(),
            error: None,
        };
        let result = match request {
            AnnRequest::Open {
                protocol,
                flat,
                checkpoint,
                checksum,
                checkpoint_rows,
                ..
            } => {
                if protocol != ANN_WORKER_PROTOCOL {
                    Err("ANN worker protocol mismatch".into())
                } else {
                    session = Some(Session::new(flat, checkpoint, checksum, checkpoint_rows));
                    Ok(())
                }
            }
            AnnRequest::Step { background, .. } => session
                .as_mut()
                .ok_or("ANN session not initialized".into())
                .and_then(|s| s.step(background, &control, &mut reply)),
            AnnRequest::Cancel { .. } => unreachable!(),
        };
        reply.error = result.err();
        serde_json::to_writer(&mut writer, &reply).map_err(|e| e.to_string())?;
        writeln!(writer)
            .and_then(|_| writer.flush())
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cancel_is_scoped_to_the_request_id() {
        let c = Control::default();
        c.begin(10);
        c.cancel(10);
        assert!(c.check().is_err());
        c.begin(11);
        c.cancel(10);
        assert!(c.check().is_ok());
    }

    fn input(directory: &Path, rows: usize) -> PathBuf {
        let path = directory.join("input.cpdvec");
        let keys: Vec<String> = (0..rows).map(|i| format!("v{i:05}")).collect();
        let header = Header::for_snapshot(
            1,
            7,
            rows as u64,
            8,
            "clip_image",
            "model",
            "revision",
            96,
            keys.iter().map(|k| k.len() as u64).sum(),
        )
        .unwrap();
        let mut writer = ann_format::FlatFileWriter::create(&path, header).unwrap();
        for (i, key) in keys.iter().enumerate() {
            writer.push_key(key).unwrap();
            let mut vector = vec![0.0f32; 8];
            vector[i % 8] = 1.0;
            writer.push_vector(&vector).unwrap();
        }
        writer.finish().unwrap();
        path
    }

    fn reply() -> AnnReply {
        AnnReply {
            request_id: 1,
            phase: String::new(),
            built: 0,
            graph_bytes: 0,
            checkpoint: None,
            checksum: None,
            samples: Vec::new(),
            error: None,
        }
    }

    fn checkpoint(session: &mut Session, control: &Control) -> AnnReply {
        for _ in 0..10_000 {
            let mut reply = reply();
            session.step(false, control, &mut reply).unwrap();
            if reply.checkpoint.is_some() {
                return reply;
            }
        }
        panic!("checkpoint did not complete")
    }

    #[test]
    fn complete_checkpoint_restores_and_replays_only_the_uncommitted_suffix() {
        let directory = tempfile::tempdir().unwrap();
        let path = input(directory.path(), 5100);
        let control = Control::default();
        control.begin(1);
        let mut session = Session::new(path.to_string_lossy().into_owned(), None, None, 0);
        let first = checkpoint(&mut session, &control);
        assert_eq!(first.built, CHECKPOINT_ROWS);
        let first_path = first.checkpoint.clone().unwrap();
        let first_digest = first.checksum.clone().unwrap();
        // Build more in RAM, then exit before another complete checkpoint.
        session.step(false, &control, &mut reply()).unwrap();
        assert_eq!(session.built, 5100);
        drop(session);
        let mut resumed = Session::new(
            path.to_string_lossy().into_owned(),
            Some(first_path.clone()),
            Some(first_digest.clone()),
            CHECKPOINT_ROWS,
        );
        let final_checkpoint = checkpoint(&mut resumed, &control);
        assert_eq!(final_checkpoint.built, 5100);
        assert_eq!(resumed.index.as_ref().unwrap().size(), 5100);
        assert_eq!(
            hex::encode(Sha256::digest(std::fs::read(first_path).unwrap())),
            first_digest
        );
        for ordinal in [0, 4999, 5099] {
            let mut recovered = vec![0.0f32; 8];
            let found = resumed
                .index
                .as_ref()
                .unwrap()
                .get(ordinal + 1, &mut recovered)
                .unwrap();
            ann_format::validate_ann_recovered_probe(
                resumed.vector(ordinal as usize).unwrap().as_ref(),
                &recovered,
                found,
            )
            .unwrap();
        }
    }

    #[test]
    fn losing_process_memory_in_each_phase_resumes_the_last_complete_prefix() {
        let directory = tempfile::tempdir().unwrap();
        let path = input(directory.path(), 5020);
        let control = Control::default();
        control.begin(1);
        let mut first = Session::new(path.to_string_lossy().into_owned(), None, None, 0);
        let committed = checkpoint(&mut first, &control);
        drop(first);
        let checkpoint_path = committed.checkpoint.unwrap();
        let checksum = committed.checksum.unwrap();
        for phase in [
            Phase::Flat,
            Phase::ReadCheckpoint,
            Phase::Restore,
            Phase::Build,
            Phase::Serialize,
            Phase::Write,
            Phase::ReadBack,
            Phase::VerifyRestore,
            Phase::Validate,
        ] {
            let reopen = || {
                Session::new(
                    path.to_string_lossy().into_owned(),
                    Some(checkpoint_path.clone()),
                    Some(checksum.clone()),
                    CHECKPOINT_ROWS,
                )
            };
            let mut interrupted = reopen();
            for _ in 0..1000 {
                if interrupted.phase == phase {
                    break;
                }
                interrupted.step(false, &control, &mut reply()).unwrap();
            }
            assert_eq!(interrupted.phase, phase);
            // Exiting here loses buffers, the in-memory graph and partial I/O;
            // none of those has been acknowledged as a durable checkpoint.
            drop(interrupted);
            let mut recovered = reopen();
            assert_eq!(checkpoint(&mut recovered, &control).built, 5020);
            let index = recovered.index.as_ref().unwrap();
            assert_eq!(index.size(), 5020);
            assert!((1..=5020).all(|id| index.contains(id)), "{phase:?}");
            assert_eq!(
                hex::encode(Sha256::digest(std::fs::read(&checkpoint_path).unwrap())),
                checksum
            );
        }
    }

    #[test]
    fn interrupted_save_cannot_replace_a_complete_checkpoint_and_corruption_is_rejected() {
        let directory = tempfile::tempdir().unwrap();
        let path = input(directory.path(), 5050);
        let control = Control::default();
        control.begin(1);
        let mut session = Session::new(path.to_string_lossy().into_owned(), None, None, 0);
        let first = checkpoint(&mut session, &control);
        session.step(false, &control, &mut reply()).unwrap(); // insert suffix
        session.step(false, &control, &mut reply()).unwrap(); // serialize scratch
        control.cancel(1);
        assert!(session
            .step(false, &control, &mut reply())
            .unwrap_err()
            .starts_with("background_paused:"));
        assert_eq!(
            hex::encode(Sha256::digest(
                std::fs::read(first.checkpoint.as_ref().unwrap()).unwrap()
            )),
            first.checksum.as_ref().unwrap().as_str()
        );
        drop(session);
        std::fs::write(first.checkpoint.as_ref().unwrap(), b"incomplete checkpoint").unwrap();
        let mut bad = Session::new(
            path.to_string_lossy().into_owned(),
            first.checkpoint,
            first.checksum,
            first.built,
        );
        control.begin(2);
        let mut failure = None;
        for _ in 0..100 {
            if let Err(error) = bad.step(false, &control, &mut reply()) {
                failure = Some(error);
                break;
            }
        }
        assert!(failure.unwrap().contains("checksum mismatch"));
    }
}
