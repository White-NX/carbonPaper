//! Resumable ANN task. Frozen input, files and complete graph checkpoints each
//! have their own commit boundary; queries keep using the previous generation.
use super::*;
use crate::ann_protocol::{AnnReply, AnnRequest, ANN_WORKER_PROTOCOL};
use crate::background_policy::{
    self as policy, CostKey, CostSample, ExecutionProfile, IO_BYTES_PER_SECOND, IO_CHUNK_BYTES,
};
use crate::background_resources::{process_usage, set_cpu_rate};
use crate::background_scheduler::{BackgroundSchedulerState, ScheduledSliceResult, TASK_ANN_BUILD};
use crate::storage::{AnnBuildCheckpoint, AnnCheckpointFile};
use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom, Write};
use std::process::ChildStdin;
use std::sync::mpsc::{self, Receiver};
use std::time::Instant;

pub(super) struct BuildWorker {
    generation: u64,
    db_generation: u64,
    child: Child,
    stdin: ChildStdin,
    replies: Receiver<AnnReply>,
    job: BuilderJob,
    request_id: u64,
    phase: String,
    last_used: Instant,
}

impl Drop for BuildWorker {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl BuildWorker {
    pub(super) fn expired(&self) -> bool {
        self.last_used.elapsed() >= Duration::from_secs(300)
    }
    fn spawn(app: &AppHandle, generation: u64, db_generation: u64) -> Result<Self, String> {
        let child = Command::new(resolve_ml_executable(app)?)
            .arg("--ann-session")
            .creation_flags(CREATE_NO_WINDOW | BELOW_NORMAL_PRIORITY_CLASS)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| e.to_string())?;
        let mut child = PendingBuilder::new(child);
        let job = create_builder_job()?;
        assign_builder_job(&job, child.child())?;
        let mut process = child.take();
        let stdin = process.stdin.take().ok_or("ANN worker stdin unavailable")?;
        let stdout = process
            .stdout
            .take()
            .ok_or("ANN worker stdout unavailable")?;
        let (tx, replies) = mpsc::sync_channel(2);
        std::thread::Builder::new()
            .name("carbonpaper-ann-replies".into())
            .spawn(move || {
                for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                    if line.len() > 64 * 1024 {
                        break;
                    }
                    let Ok(reply) = serde_json::from_str::<AnnReply>(&line) else {
                        break;
                    };
                    if tx.send(reply).is_err() {
                        break;
                    }
                }
            })
            .map_err(|e| e.to_string())?;
        Ok(Self {
            generation,
            db_generation,
            child: process,
            stdin,
            replies,
            job,
            request_id: 0,
            phase: "ann_io".into(),
            last_used: Instant::now(),
        })
    }
    fn write(&mut self, request: &AnnRequest) -> Result<(), String> {
        serde_json::to_writer(&mut self.stdin, request).map_err(|e| e.to_string())?;
        writeln!(self.stdin)
            .and_then(|_| self.stdin.flush())
            .map_err(|e| e.to_string())
    }

    async fn request(
        &mut self,
        app: &AppHandle,
        state: &AnnBuildCheckpoint,
        open: bool,
        directory: &Path,
    ) -> Result<AnnReply, String> {
        policy::check_current()?;
        let lease = policy::current_execution();
        let background = lease
            .as_ref()
            .is_some_and(|l| l.profile == ExecutionProfile::Background);
        let scheduler = app.state::<Arc<BackgroundSchedulerState>>();
        let key = ann_key(state, &self.phase);
        scheduler.admit_operation(&key, false, true)?;
        set_cpu_rate(self.job.0, lease.as_ref().map(|_| policy::CPU_RATE_PERCENT))?;
        self.request_id += 1;
        let request_id = self.request_id;
        let request = if open {
            AnnRequest::Open {
                request_id,
                protocol: ANN_WORKER_PROTOCOL,
                flat: directory
                    .join(state.flat_name())
                    .to_string_lossy()
                    .into_owned(),
                checkpoint: state
                    .complete
                    .as_ref()
                    .map(|c| directory.join(&c.name).to_string_lossy().into_owned()),
                checksum: state.complete.as_ref().map(|c| c.checksum.clone()),
                checkpoint_rows: state.complete.as_ref().map_or(0, |c| c.rows),
            }
        } else {
            AnnRequest::Step {
                request_id,
                background,
            }
        };
        let before_usage = process_usage(HANDLE(self.child.as_raw_handle()));
        self.write(&request)?;
        let baseline = before_usage.map_or(0, |u| u.private_bytes);
        let mut peak = baseline;
        let started = Instant::now();
        let mut cancelled: Option<(Instant, String)> = None;
        let reply = loop {
            if let Some(usage) = process_usage(HANDLE(self.child.as_raw_handle())) {
                peak = peak.max(usage.peak_since(before_usage));
            }
            match self.replies.try_recv() {
                Ok(reply) if reply.request_id == request_id => break reply,
                Ok(_) => return Err("ANN worker response ID mismatch".into()),
                Err(mpsc::TryRecvError::Disconnected) => {
                    return Err("ANN builder failed: worker disconnected".into())
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
            if cancelled.is_none() {
                let error = policy::check_current().err().or_else(|| {
                    if background && started.elapsed() > Duration::from_millis(1000) {
                        scheduler.revoke_cost(&key);
                        if let Some(lease) = &lease {
                            lease.revoke("cost_overrun");
                        }
                        Some("background_paused: ANN cost overrun".into())
                    } else if started.elapsed() > Duration::from_secs(300) {
                        Some("ANN builder failed: stage watchdog expired".into())
                    } else {
                        None
                    }
                });
                if let Some(error) = error {
                    self.write(&AnnRequest::Cancel { request_id })?;
                    cancelled = Some((Instant::now(), error));
                }
            }
            if cancelled
                .as_ref()
                .is_some_and(|(at, _)| at.elapsed() >= Duration::from_millis(500))
            {
                let _ = self.child.kill();
                let _ = self.child.wait();
                return Err(cancelled.unwrap().1);
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        };
        self.last_used = Instant::now();
        self.phase = reply.phase.clone();
        if let Some((_, error)) = cancelled {
            return Err(error);
        }
        if let Some(error) = &reply.error {
            return Err(error.clone());
        }
        if let Some(lease) = &lease {
            for sample in &reply.samples {
                lease.account_cpu(sample.cpu_ms);
                let mut key = ann_key(state, &sample.operation);
                if sample.operation != "ann_io" {
                    key.input_bucket = reply
                        .built
                        .max(1)
                        .checked_next_power_of_two()
                        .unwrap_or(u64::MAX);
                }
                scheduler.record_cost(
                    key,
                    CostSample {
                        elapsed_ms: sample.elapsed_ms,
                        cpu_ms: sample.cpu_ms,
                        peak_private_bytes: peak,
                        additional_peak_bytes: peak.saturating_sub(baseline),
                        cpu_rate_percent: policy::CPU_RATE_PERCENT,
                    },
                    lease.profile,
                );
            }
        }
        policy::check_current()?;
        Ok(reply)
    }
}

fn ann_key(state: &AnnBuildCheckpoint, operation: &str) -> CostKey {
    CostKey::new(
        TASK_ANN_BUILD,
        operation,
        CLIP_VECTOR_SPACE_REVISION,
        state.expected_rows.max(state.frozen_rows),
        "usearch-2.26.0:i8:m16:ef160:threads1:io256k:v1",
    )
}

fn check_checkpoint_name(state: &AnnBuildCheckpoint, name: &str) -> bool {
    name.starts_with(&format!("clip_image-{}.checkpoint-", state.generation))
        && Path::new(name).components().count() == 1
        && !name.contains(['/', '\\', ':'])
}

fn header(state: &AnnBuildCheckpoint) -> Result<Header, String> {
    Header::for_snapshot(
        state.generation,
        state.covered_epoch,
        state.frozen_rows,
        CLIP_DIMENSIONS as u32,
        "clip_image",
        CLIP_MODEL_ID,
        CLIP_VECTOR_SPACE_REVISION,
        expansion_search(state.frozen_rows as usize) as u32,
        state.key_bytes,
    )
}

async fn io_rest(bytes: u64) -> Result<(), String> {
    if policy::is_background() {
        let until =
            Instant::now() + Duration::from_secs_f64(bytes as f64 / IO_BYTES_PER_SECOND as f64);
        while Instant::now() < until {
            policy::check_current()?;
            tokio::time::sleep(
                until
                    .saturating_duration_since(Instant::now())
                    .min(Duration::from_millis(25)),
            )
            .await;
        }
    }
    policy::check_current()
}

fn materialize_page(
    storage: &StorageState,
    state: &mut AnnBuildCheckpoint,
    directory: &Path,
) -> Result<u64, String> {
    let path = directory.join(format!(".{}.partial", state.flat_name()));
    let h = header(state)?;
    if state.materialized_rows > 0 && !path.exists() {
        // Replay after a crash between final rename and cursor commit.
        state.materialized_rows = 0;
        state.materialized_key_bytes = 0;
        storage.save_ann_build_checkpoint(state)?;
    }
    let mut writer = if state.materialized_rows == 0 {
        if path.exists() {
            fs::remove_file(&path).map_err(|e| e.to_string())?;
        }
        FlatFileWriter::create(&path, h)?
    } else {
        match FlatFileWriter::resume(
            &path,
            h,
            state.materialized_rows,
            state.materialized_key_bytes,
        ) {
            Ok(writer) => writer,
            Err(error) if error == "ANN partial flat checkpoint mismatch" => {
                // A truncated or incompatible scratch page is disposable; the
                // frozen database input remains the authoritative restart point.
                state.materialized_rows = 0;
                state.materialized_key_bytes = 0;
                storage.save_ann_build_checkpoint(state)?;
                return Ok(0);
            }
            Err(error) => return Err(error),
        }
    };
    let page = storage.frozen_ann_page(state, state.materialized_rows, 64)?;
    let bytes = page
        .iter()
        .map(|r| r.vector_f32.len() as u64 + r.subject_key.len() as u64 + 8)
        .sum();
    let mut written = state.materialized_rows;
    write_ann_snapshot_page(&mut writer, CLIP_DIMENSIONS as u32, &mut written, &page)?;
    writer.sync_checkpoint()?;
    let mut next = state.clone();
    next.materialized_rows = written;
    next.materialized_key_bytes += page.iter().map(|r| r.subject_key.len() as u64).sum::<u64>();
    if next.materialized_rows == state.frozen_rows {
        writer.finish()?;
        let final_path = directory.join(state.flat_name());
        if final_path.exists() {
            fs::remove_file(&final_path).map_err(|e| e.to_string())?;
        }
        fs::rename(path, final_path).map_err(|e| e.to_string())?;
        next.phase = "hash_flat".into();
    }
    storage.save_ann_build_checkpoint(&next)?;
    *state = next;
    Ok(bytes)
}

async fn hash_file(path: &Path) -> Result<String, String> {
    let mut file = fs::File::open(path).map_err(|e| e.to_string())?;
    let mut hasher = Sha256::new();
    let mut bytes = vec![0u8; IO_CHUNK_BYTES];
    loop {
        policy::check_current()?;
        let count = file.read(&mut bytes).map_err(|e| e.to_string())?;
        if count == 0 {
            break;
        }
        hasher.update(&bytes[..count]);
        io_rest(count as u64).await?;
    }
    Ok(hex::encode(hasher.finalize()))
}

async fn digest_matches(path: &Path, expected: Option<&str>) -> Result<bool, String> {
    if !path.is_file() || expected.is_none() {
        return Ok(false);
    }
    Ok(Some(hash_file(path).await?.as_str()) == expected)
}

/// Repair unpublished copies without replacing either the ready generation or
/// a complete checkpoint. The caller persists the returned restart phase.
async fn recover_publication(
    state: &mut AnnBuildCheckpoint,
    directory: &Path,
) -> Result<bool, String> {
    if !digest_matches(
        &directory.join(state.flat_name()),
        state.flat_checksum.as_deref(),
    )
    .await?
    {
        state.phase = "materialize".into();
        state.materialized_rows = 0;
        state.materialized_key_bytes = 0;
        state.copy_offset = 0;
        state.flat_checksum = None;
        return Ok(true);
    }
    let complete = state
        .complete
        .as_ref()
        .ok_or("ANN complete checkpoint missing")?;
    if digest_matches(&directory.join(state.ann_name()), Some(&complete.checksum)).await? {
        return Ok(false);
    }
    state.copy_offset = 0;
    if digest_matches(&directory.join(&complete.name), Some(&complete.checksum)).await? {
        state.phase = "copy_ann".into();
    } else {
        state.complete = state.previous.take();
        state.phase = "build".into();
    }
    Ok(true)
}

fn manifest(state: &AnnBuildCheckpoint) -> Result<DerivedAnnGeneration, String> {
    Ok(DerivedAnnGeneration {
        index_kind: DerivedIndexKind::ClipImage,
        generation: state.generation,
        covered_epoch: state.covered_epoch,
        flat_file_name: state.flat_name(),
        flat_checksum_sha256: state
            .flat_checksum
            .clone()
            .ok_or("ANN flat checksum missing")?,
        ann_file_name: state.ann_name(),
        ann_checksum_sha256: state
            .complete
            .as_ref()
            .ok_or("ANN complete checkpoint missing")?
            .checksum
            .clone(),
        row_count: state.frozen_rows,
        dimensions: CLIP_DIMENSIONS as u32,
        model_id: CLIP_MODEL_ID.into(),
        model_revision: CLIP_VECTOR_SPACE_REVISION.into(),
        embedding_version: CLIP_EMBEDDING_VERSION,
        sidecar_format_version: FORMAT_VERSION,
        ann_format_version: ANN_FILE_FORMAT_VERSION,
        algorithm: ANN_ALGORITHM.into(),
        implementation_version: ANN_IMPLEMENTATION_VERSION.into(),
        metric: ANN_METRIC.into(),
        quantization: ANN_QUANTIZATION.into(),
        connectivity: ANN_CONNECTIVITY,
        expansion_add: ANN_EXPANSION_ADD,
        expansion_search: expansion_search(state.frozen_rows as usize) as u32,
        created_at: Utc::now().to_rfc3339(),
    })
}

pub(crate) async fn run_scheduled_slice(
    app: &AppHandle,
    manual: bool,
) -> Result<ScheduledSliceResult, String> {
    let deadline = Instant::now() + Duration::from_secs(1800);
    loop {
        let result = run_unit(app, manual).await?;
        if !manual || !result.has_more || result.skipped_reason.is_some() {
            return Ok(result);
        }
        if Instant::now() >= deadline {
            return Ok(ScheduledSliceResult::skipped("manual_deadline"));
        }
        if let Some(reason) = crate::background_scheduler::gate_reason(app, true) {
            return Ok(ScheduledSliceResult::skipped(reason));
        }
        tokio::task::yield_now().await;
    }
}

fn wait_for_idle() -> ScheduledSliceResult {
    if let Some(lease) = policy::current_execution() {
        lease.revoke("waiting_for_idle");
    }
    ScheduledSliceResult::skipped("waiting_for_idle")
}

async fn run_unit(app: &AppHandle, manual: bool) -> Result<ScheduledSliceResult, String> {
    if !enabled() {
        return Ok(ScheduledSliceResult::skipped("disabled"));
    }
    let ann = app.state::<Arc<ClipAnnState>>().inner().clone();
    let Ok(_build) = ann.build_lock.try_lock() else {
        return Ok(ScheduledSliceResult::skipped("ann_busy"));
    };
    let storage = app.state::<Arc<StorageState>>().inner().clone();
    if !ann_retry_due(
        storage
            .get_derived_ann_build_state(DerivedIndexKind::ClipImage)?
            .as_ref(),
        Utc::now(),
        manual,
    ) {
        return Ok(ScheduledSliceResult::skipped("retry_wait"));
    }
    let lifecycle = ann.lifecycle_token();
    let db_generation = storage.db_generation();
    let dataset = storage.processing_dataset_id()?;
    let mut state = match storage.ann_build_checkpoint() {
        Err(error) if error.starts_with("invalid ANN checkpoint metadata:") => None,
        result => result?,
    };
    let contract = format!("{CLIP_VECTOR_SPACE_REVISION}:{ANN_IMPLEMENTATION_VERSION}:{FORMAT_VERSION}:{ANN_WORKER_PROTOCOL}");
    if state
        .as_ref()
        .is_some_and(|s| s.dataset_id != dataset || s.model_fingerprint != contract)
    {
        ann.resumable_worker.lock().await.take();
        state = None;
    }
    if state.as_ref().is_none_or(|s| s.phase == "complete") {
        if !ann_retry_due(
            storage
                .get_derived_ann_build_state(DerivedIndexKind::ClipImage)?
                .as_ref(),
            Utc::now(),
            manual,
        ) {
            return Ok(ScheduledSliceResult::skipped("retry_wait"));
        }
        if !rebuild_needed(&storage, manual || !ann.has_generation())? {
            return Ok(ScheduledSliceResult::complete(false));
        }
        if policy::is_background() {
            return Ok(wait_for_idle());
        }
        let (epoch, rows, key_bytes, dimensions, model, revision, version) =
            storage.derived_index_snapshot_for_ann(DerivedIndexKind::ClipImage)?;
        if dimensions as usize != CLIP_DIMENSIONS
            || model != CLIP_MODEL_ID
            || revision != CLIP_VECTOR_SPACE_REVISION
            || version != CLIP_EMBEDDING_VERSION
        {
            return Err("ANN source model contract does not match current CLIP".into());
        }
        let mut new = AnnBuildCheckpoint {
            runtime_generation: Some(db_generation),
            generation: next_generation_id()?,
            dataset_id: dataset,
            model_fingerprint: contract,
            covered_epoch: epoch,
            scan_upper: String::new(),
            scan_cursor: String::new(),
            expected_rows: rows,
            frozen_rows: 0,
            key_bytes: 0,
            phase: "freeze".into(),
            materialized_rows: 0,
            materialized_key_bytes: 0,
            flat_checksum: None,
            graph_bytes: 0,
            complete: None,
            previous: None,
            copy_offset: 0,
        };
        // Metadata estimates are used only for memory/size admission. Frozen
        // counts are established by the paged durable input, not the live DB.
        let _ = key_bytes;
        storage.begin_ann_build(&mut new)?;
        state = Some(new);
    }
    let mut state = state.unwrap();
    state.runtime_generation = Some(db_generation);
    if state.phase == "cleanup" && policy::is_background() {
        return Ok(wait_for_idle());
    }
    if policy::is_background() {
        if !state.small() {
            return Ok(wait_for_idle());
        }
        let scheduler = app.state::<Arc<BackgroundSchedulerState>>();
        // Smallness alone is never a hardware qualification.
        for op in [
            "ann_insert",
            "ann_serialize",
            "ann_restore",
            "ann_validate",
            "ann_checksum",
            "ann_io",
        ] {
            scheduler.admit_operation(&ann_key(&state, op), false, true)?;
        }
    }
    for file in [state.complete.as_ref(), state.previous.as_ref()]
        .into_iter()
        .flatten()
    {
        if !check_checkpoint_name(&state, &file.name) {
            return Err("ANN checkpoint filename is invalid".into());
        }
    }
    let data_dir = storage
        .data_dir
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    let directory = data_dir.join("derived-indexes");
    fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
    policy::check_current()?;
    if storage.db_generation() != db_generation {
        return Err("background_paused: database changed".into());
    }
    let result: Result<ScheduledSliceResult, String> = async {
        match state.phase.as_str() {
            "freeze" => {
                storage
                    .freeze_ann_page(&mut state, if policy::is_background() { 1 } else { 64 })?;
            }
            "materialize" => {
                if state.frozen_rows == 0 {
                    state.phase = "complete".into();
                    storage.save_ann_build_checkpoint(&state)?;
                    return Ok(ScheduledSliceResult::complete(false));
                }
                let bytes = materialize_page(&storage, &mut state, &directory)?;
                io_rest(bytes).await?;
            }
            "hash_flat" => {
                state.flat_checksum = Some(hash_file(&directory.join(state.flat_name())).await?);
                state.phase = if state
                    .complete
                    .as_ref()
                    .is_some_and(|c| c.rows == state.frozen_rows)
                {
                    "copy_ann"
                } else {
                    "build"
                }
                .into();
                storage.save_ann_build_checkpoint(&state)?;
            }
            "build" => {
                let mut slot = ann.resumable_worker.lock().await;
                if slot.as_ref().is_some_and(|w| {
                    w.generation != state.generation
                        || w.db_generation != db_generation
                        || w.last_used.elapsed() >= Duration::from_secs(300)
                }) {
                    slot.take();
                }
                if slot.is_none() {
                    let flat = directory.join(state.flat_name());
                    if !flat.is_file() || Some(hash_file(&flat).await?) != state.flat_checksum {
                        state.phase = "materialize".into();
                        state.materialized_rows = 0;
                        state.materialized_key_bytes = 0;
                        storage.save_ann_build_checkpoint(&state)?;
                        return Ok(ScheduledSliceResult::skipped("checkpoint_recovery"));
                    }
                    // A missing checkpoint is normal recovery: replay the last
                    // complete prefix, or rebuild from the durable frozen input.
                    if state
                        .complete
                        .as_ref()
                        .is_some_and(|c| !directory.join(&c.name).is_file())
                    {
                        state.complete = state.previous.take();
                        storage.save_ann_build_checkpoint(&state)?;
                    }
                    *slot = Some(BuildWorker::spawn(app, state.generation, db_generation)?);
                    let result = slot
                        .as_mut()
                        .unwrap()
                        .request(app, &state, true, &directory)
                        .await;
                    if result.is_err() {
                        slot.take();
                    }
                    result?;
                }
                let result = slot
                    .as_mut()
                    .unwrap()
                    .request(app, &state, false, &directory)
                    .await;
                let reply = match result {
                    Ok(reply) => reply,
                    Err(error) => {
                        // A cancelled serialization may have overwritten its
                        // scratch buffer. Recreate the worker from a complete
                        // checkpoint; the authoritative frozen rows survive.
                        slot.take();
                        if error.contains("checkpoint") && !policy::is_pause(&error) {
                            state.complete = state.previous.take();
                            storage.save_ann_build_checkpoint(&state)?;
                            return Ok(ScheduledSliceResult::skipped("checkpoint_recovery"));
                        }
                        return Err(error);
                    }
                };
                if let Err(error) = storage.check_ann_runtime(&state) {
                    slot.take();
                    return Err(error);
                }
                if let (Some(path), Some(checksum)) = (reply.checkpoint, reply.checksum) {
                    let expected = directory
                        .join(state.flat_name())
                        .with_extension(format!("checkpoint-{}.partial", reply.built));
                    if Path::new(&path) != expected {
                        return Err("ANN checkpoint path mismatch".into());
                    }
                    let name = format!(
                        "clip_image-{}.checkpoint-{}-{}.cpdann",
                        state.generation,
                        reply.built,
                        next_generation_id()?
                    );
                    fs::rename(&expected, directory.join(&name)).map_err(|e| e.to_string())?;
                    let obsolete = state.previous.take();
                    state.previous = state.complete.take();
                    state.complete = Some(AnnCheckpointFile {
                        name,
                        checksum,
                        rows: reply.built,
                    });
                    state.graph_bytes = reply.graph_bytes;
                    if reply.built == state.frozen_rows {
                        state.phase = "copy_ann".into();
                        slot.take();
                    }
                    policy::check_current()?;
                    storage.save_ann_build_checkpoint(&state)?;
                    if let Some(obsolete) = obsolete {
                        if check_checkpoint_name(&state, &obsolete.name) {
                            let _ = fs::remove_file(directory.join(obsolete.name));
                        }
                    }
                }
            }
            "copy_ann" => {
                let complete = state.complete.as_ref().ok_or("ANN checkpoint missing")?;
                if !directory.join(&complete.name).is_file() {
                    state.complete = state.previous.take();
                    state.phase = "build".into();
                    state.copy_offset = 0;
                    storage.save_ann_build_checkpoint(&state)?;
                    return Ok(ScheduledSliceResult::skipped("checkpoint_recovery"));
                }
                let final_path = directory.join(state.ann_name());
                if final_path.exists() && hash_file(&final_path).await? == complete.checksum {
                    state.phase = "publish".into();
                    storage.save_ann_build_checkpoint(&state)?;
                    return Ok(ScheduledSliceResult::complete(true));
                }
                let mut source =
                    File::open(directory.join(&complete.name)).map_err(|e| e.to_string())?;
                let target = directory.join(format!(".{}.partial", state.ann_name()));
                if state.copy_offset > source.metadata().map_err(|e| e.to_string())?.len()
                    || fs::metadata(&target).map_or(true, |m| m.len() < state.copy_offset)
                {
                    state.copy_offset = 0;
                }
                let mut output = fs::OpenOptions::new()
                    .write(true)
                    .create(true)
                    .truncate(state.copy_offset == 0)
                    .open(&target)
                    .map_err(|e| e.to_string())?;
                source
                    .seek(SeekFrom::Start(state.copy_offset))
                    .map_err(|e| e.to_string())?;
                output
                    .seek(SeekFrom::Start(state.copy_offset))
                    .map_err(|e| e.to_string())?;
                let mut buffer = vec![0u8; IO_CHUNK_BYTES];
                let count = source.read(&mut buffer).map_err(|e| e.to_string())?;
                output
                    .write_all(&buffer[..count])
                    .and_then(|_| output.sync_all())
                    .map_err(|e| e.to_string())?;
                state.copy_offset += count as u64;
                if count == 0 {
                    drop(output);
                    fs::rename(target, directory.join(state.ann_name()))
                        .map_err(|e| e.to_string())?;
                    state.phase = "publish".into();
                }
                storage.save_ann_build_checkpoint(&state)?;
                io_rest(count as u64).await?;
            }
            "publish" => {
                if storage
                    .get_derived_ann_generation(DerivedIndexKind::ClipImage)?
                    .is_some_and(|ready| ready.generation == state.generation)
                {
                    state.phase = "cleanup".into();
                    storage.save_ann_build_checkpoint(&state)?;
                    return Ok(ScheduledSliceResult::complete(true));
                }
                if recover_publication(&mut state, &directory).await? {
                    storage.save_ann_build_checkpoint(&state)?;
                    return Ok(ScheduledSliceResult::skipped("checkpoint_recovery"));
                }
                let manifest = manifest(&state)?;
                policy::check_current()?;
                // Validation and all large I/O have finished. Only mmap setup,
                // the manifest transaction and reader switch enter this guard.
                let reader = open_generation_validated(&data_dir, manifest)?;
                let _publish = storage.derived_generation_publish_guard();
                policy::check_current()?;
                if storage.db_generation() != db_generation
                    || storage.processing_dataset_id()? != state.dataset_id
                {
                    return Err("background_paused: database changed".into());
                }
                if !ann.publish_from_lifecycle(
                    lifecycle,
                    &storage,
                    PreparedGeneration::new(reader),
                )? {
                    return Err("background_paused: ANN lifecycle changed".into());
                }
                state.phase = "cleanup".into();
                storage.save_ann_build_checkpoint(&state)?;
                return Ok(ScheduledSliceResult::complete(true).with_processed(state.frozen_rows));
            }
            "cleanup" => {
                if !storage.cleanup_ann_inputs(&state)? {
                    state.phase = "complete".into();
                    storage.save_ann_build_checkpoint(&state)?;
                    return Ok(ScheduledSliceResult::complete(false));
                }
            }
            _ => return Err("Unknown ANN checkpoint phase".into()),
        }
        Ok(ScheduledSliceResult::complete(true))
    }
    .await;
    if let Err(error) = &result {
        if !policy::is_pause(error) {
            record_ann_build_failure(app, &storage, &ann, lifecycle, error)?;
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> AnnBuildCheckpoint {
        AnnBuildCheckpoint {
            runtime_generation: Some(0),
            generation: 7,
            dataset_id: "test".into(),
            model_fingerprint: "clip-test".into(),
            covered_epoch: 10,
            scan_upper: "last".into(),
            scan_cursor: "last".into(),
            expected_rows: 10,
            frozen_rows: 10,
            key_bytes: 100,
            phase: "publish".into(),
            materialized_rows: 10,
            materialized_key_bytes: 100,
            flat_checksum: Some(hex::encode(Sha256::digest(b"flat input"))),
            graph_bytes: 10,
            complete: Some(AnnCheckpointFile {
                name: "clip_image-7.checkpoint-10-1.cpdann".into(),
                checksum: hex::encode(Sha256::digest(b"graph")),
                rows: 10,
            }),
            previous: Some(AnnCheckpointFile {
                name: "clip_image-7.checkpoint-5-1.cpdann".into(),
                checksum: hex::encode(Sha256::digest(b"prefix")),
                rows: 5,
            }),
            copy_offset: 5,
        }
    }

    #[tokio::test]
    async fn publication_recovers_partial_copies_and_preserves_complete_and_ready_files() {
        let directory = tempfile::tempdir().unwrap();
        let dir = directory.path();
        let mut state = state();
        let complete = state.complete.clone().unwrap();
        fs::write(dir.join("ready.cpdann"), b"previous queryable generation").unwrap();
        fs::write(dir.join(state.flat_name()), b"flat input").unwrap();
        fs::write(dir.join(&complete.name), b"graph").unwrap();
        fs::write(dir.join(state.ann_name()), b"gra").unwrap();
        assert!(recover_publication(&mut state, dir).await.unwrap());
        assert_eq!(state.phase, "copy_ann");
        assert_eq!(state.copy_offset, 0);
        assert_eq!(fs::read(dir.join(&complete.name)).unwrap(), b"graph");
        assert_eq!(
            fs::read(dir.join("ready.cpdann")).unwrap(),
            b"previous queryable generation"
        );
        fs::write(dir.join(state.ann_name()), b"graph").unwrap();
        state.phase = "publish".into();
        assert!(!recover_publication(&mut state, dir).await.unwrap());
    }

    #[tokio::test]
    async fn publication_falls_back_to_frozen_input_or_the_previous_checkpoint() {
        let directory = tempfile::tempdir().unwrap();
        let dir = directory.path();
        let mut state = state();
        fs::write(dir.join(state.flat_name()), b"flat input").unwrap();
        fs::write(dir.join(state.ann_name()), b"corrupt copy").unwrap();
        fs::write(
            dir.join(&state.complete.as_ref().unwrap().name),
            b"corrupt checkpoint",
        )
        .unwrap();
        assert!(recover_publication(&mut state, dir).await.unwrap());
        assert_eq!(state.phase, "build");
        assert_eq!(state.complete.as_ref().unwrap().rows, 5);
        assert!(state.previous.is_none());
        fs::write(dir.join(state.flat_name()), b"corrupt flat").unwrap();
        state.phase = "publish".into();
        assert!(recover_publication(&mut state, dir).await.unwrap());
        assert_eq!(state.phase, "materialize");
        assert_eq!(state.materialized_rows, 0);
        assert_eq!(state.materialized_key_bytes, 0);
        assert!(state.flat_checksum.is_none());
        assert_eq!(state.complete.as_ref().unwrap().rows, 5);
    }
}
