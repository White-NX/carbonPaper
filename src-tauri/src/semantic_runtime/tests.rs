use super::*;

#[tokio::test]
async fn capture_power_policy_reaches_startup_watches_without_leaking_to_manual_requests() {
    let power = Arc::new(crate::power::PowerState::new());
    let expected = power.clone();
    let watch = crate::power::CAPTURE_WORK
        .scope(power, async {
            RequestWatch::new(Instant::now() + Duration::from_secs(5))
        })
        .await;
    tokio::task::spawn_blocking(move || {
        assert!(Arc::ptr_eq(
            watch.capture_power.as_ref().unwrap(),
            &expected
        ));
    })
    .await
    .unwrap();
    let manual_watch = RequestWatch::new(Instant::now() + Duration::from_secs(5));
    assert!(manual_watch.capture_power.is_none());
    assert!(manual_watch.check(&SemanticRuntimeState::new()).is_ok());
}

#[test]
fn index_status_covers_scheduler_waits_and_ignores_automatic_work() {
    use crate::index_progress::IndexRunProgress;
    let mut task = crate::storage::BackgroundTaskState {
        task_kind: "clip_index".into(),
        ready_since_ms: 0,
        next_attempt_at_ms: 1_000,
        failure_count: 0,
        last_served_seq: 1,
        last_error: None,
        last_completed_at_ms: None,
        status: "queued".into(),
        manual_pending: true,
        manual_in_flight: false,
    };
    let progress = IndexRunProgress {
        processed: 4,
        total: 10,
        ..Default::default()
    };
    assert_eq!(
        IndexTaskProgress::resolve(progress, Some(&task), None).phase,
        "queued"
    );
    let waiting = IndexTaskProgress::resolve(progress, Some(&task), Some("waiting_for_unlock"));
    assert_eq!(waiting.phase, "waiting_for_unlock");
    assert_eq!(waiting.progress.processed, 4);
    task.status = "retry_wait".into();
    let verification =
        IndexTaskProgress::resolve(progress, Some(&task), Some("waiting_for_verification"));
    assert_eq!(verification.phase, "waiting_for_verification");
    assert_eq!(verification.progress.processed, 4);
    let retry = IndexTaskProgress::resolve(progress, Some(&task), None);
    assert_eq!(retry.phase, "retry_wait");
    assert_eq!(retry.retry_at_ms, Some(1_000));
    task.status = "running".into();
    task.manual_pending = false;
    task.manual_in_flight = true;
    assert_eq!(
        IndexTaskProgress::resolve(progress, Some(&task), None).phase,
        "running"
    );
    task.manual_in_flight = false;
    assert_eq!(
        IndexTaskProgress::resolve(progress, Some(&task), None).phase,
        "idle"
    );
    task.status = "failed".into();
    assert_eq!(
        IndexTaskProgress::resolve(progress, Some(&task), None).phase,
        "failed"
    );
}

#[test]
fn native_job_budget_is_idempotent_and_restored_for_foreground_successors() {
    use windows::Win32::System::JobObjects::{
        JobObjectCpuRateControlInformation, QueryInformationJobObject,
        JOBOBJECT_CPU_RATE_CONTROL_INFORMATION,
    };
    let mut child = Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "[Threading.Thread]::Sleep(10000)",
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let job = assign_kill_on_close_job(&child).unwrap();
    let stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let process = Arc::new(SemanticMlChild {
        provider: MlProvider::Cpu,
        supported_models: Vec::new(),
        child: Mutex::new(child),
        stdin: Mutex::new(BufWriter::new(stdin)),
        stdout: Mutex::new(BufReader::new(stdout)),
        request_lock: Mutex::new(()),
        _job: job,
    });
    let read_budget = || {
        let mut info = JOBOBJECT_CPU_RATE_CONTROL_INFORMATION::default();
        // SAFETY: the live job and correctly sized output structure remain
        // owned by this test for the duration of the query.
        unsafe {
            QueryInformationJobObject(
                process._job.0,
                JobObjectCpuRateControlInformation,
                &mut info as *mut _ as *mut _,
                std::mem::size_of_val(&info) as u32,
                None,
            )
            .unwrap();
        }
        info
    };
    // A fresh Job Object has never enabled CPU rate control. Windows
    // rejects a literal ControlFlags=0 update in that state, so the helper
    // must recognize that it is already unlimited.
    let unlimited = RequestCpuBudget::apply(process.clone(), None).unwrap();
    assert_eq!(read_budget().ControlFlags.0, 0);
    drop(unlimited);
    assert_eq!(read_budget().ControlFlags.0, 0);

    let limited = RequestCpuBudget::apply(process.clone(), Some(5)).unwrap();
    assert_ne!(read_budget().ControlFlags.0, 0);
    // SAFETY: HARD_CAP uses the CpuRate member of this Win32 union.
    assert_eq!(unsafe { read_budget().Anonymous.CpuRate }, 500);
    drop(limited);
    assert_eq!(read_budget().ControlFlags.0, 0);
    assert!(!request_failure_requires_restart(Some(
        MlErrorKind::Cancelled
    )));
    assert!(process.is_running());
    drop(InFlightSemanticRequest {
        process: process.clone(),
        completed: false,
    });
    assert!(!process.is_running());
}

#[test]
fn new_runtime_is_stopped_and_does_not_claim_a_provider() {
    let runtime = SemanticRuntimeState::new();
    let status = runtime.status();
    assert_eq!(status.state, "stopped");
    assert_eq!(status.provider, "none");
    assert!(status.loaded_model.is_none());
}

#[test]
fn response_ids_cover_all_response_variants_and_handshakes() {
    use crate::ml_protocol::*;
    let responses = [
        MlResponse::Pong { request_id: 7 },
        MlResponse::OcrComplete {
            request_id: 7,
            blocks: vec![],
            timings: MlOcrTimings {
                image_prepare_ms: 0.0,
                model_total_ms: 0.0,
                request_total_ms: 0.0,
            },
        },
        MlResponse::EmbeddingComplete {
            request_id: 7,
            model: MlSemanticModel::MinilmL12,
            dimensions: 0,
            vectors: vec![],
            timings: MlSemanticTimings {
                model_load_ms: 0.0,
                preprocess_ms: 0.0,
                inference_ms: 0.0,
                request_total_ms: 0.0,
                cpu_ms: 0.0,
                model_load_cpu_ms: 0.0,
            },
        },
        MlResponse::TokenizationComplete {
            request_id: 7,
            model: MlSemanticModel::MinilmL12,
            batch: 0,
            sequence: 0,
            input_ids: vec![],
            attention_mask: vec![],
            token_type_ids: vec![],
        },
        MlResponse::RerankComplete {
            request_id: 7,
            model: MlSemanticModel::BgeRerankerV2M3,
            scores: vec![],
            timings: MlSemanticTimings {
                model_load_ms: 0.0,
                preprocess_ms: 0.0,
                inference_ms: 0.0,
                request_total_ms: 0.0,
                cpu_ms: 0.0,
                model_load_cpu_ms: 0.0,
            },
        },
        MlResponse::SemanticStatus {
            request_id: 7,
            provider: MlProvider::Cpu,
            loaded_model: None,
            model_id: None,
            model_revision: None,
            model_fingerprint: None,
        },
        MlResponse::Unloaded { request_id: 7 },
        MlResponse::Error {
            request_id: 7,
            kind: "inference".into(),
            message: "failed".into(),
        },
        MlResponse::ShuttingDown { request_id: 7 },
    ];
    for response in responses {
        assert_eq!(response_request_id(&response), Some(7), "{response:?}");
    }
    for response in [
        MlResponse::Ready {
            protocol_version: 1,
            worker_version: "test".into(),
            rapidocr_core_version: "test".into(),
            provider: MlProvider::Cpu,
            model_id: "test".into(),
        },
        MlResponse::SemanticReady {
            protocol_version: 1,
            worker_version: "test".into(),
            ort_version: "test".into(),
            provider: MlProvider::Cpu,
            supported_models: vec![],
        },
    ] {
        assert_eq!(response_request_id(&response), None);
    }
}

#[test]
fn cpu_only_model_is_routed_to_cpu_from_worker_capabilities() {
    let supported_models = [MlSemanticModel::ChineseClip, MlSemanticModel::BgeSmallZh];
    assert_eq!(
        route_provider(
            true,
            false,
            Some(MlSemanticModel::MinilmL12),
            Some(&supported_models),
        ),
        Some(MlProvider::Cpu)
    );
    assert_eq!(
        route_provider(
            true,
            false,
            Some(MlSemanticModel::BgeRerankerV2M3),
            Some(&supported_models),
        ),
        Some(MlProvider::Cpu)
    );
}

#[test]
fn cpu_only_route_keeps_directml_available_for_supported_models() {
    let runtime = SemanticRuntimeState::new();
    runtime
        .inner
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .directml_supported_models = Some(vec![
        MlSemanticModel::ChineseClip,
        MlSemanticModel::BgeSmallZh,
    ]);

    assert_eq!(
        runtime.cached_provider_for_request(true, Some(MlSemanticModel::MinilmL12)),
        Some(MlProvider::Cpu)
    );
    assert!(!runtime.status().directml_disabled_for_session);
    assert_eq!(
        runtime.cached_provider_for_request(true, Some(MlSemanticModel::ChineseClip)),
        Some(MlProvider::DirectMl)
    );
    assert_eq!(
        runtime.cached_provider_for_request(true, Some(MlSemanticModel::BgeSmallZh)),
        Some(MlProvider::DirectMl)
    );
}

#[test]
fn directml_fallback_is_limited_to_provider_and_runtime_failures() {
    for kind in [
        MlErrorKind::ProviderUnavailable,
        MlErrorKind::Inference,
        MlErrorKind::Timeout,
        MlErrorKind::WorkerStopped,
        MlErrorKind::Transport,
    ] {
        let error = TaskError::worker(
            kind,
            "model_missing: diagnostic wording cannot change fallback",
        );
        assert!(should_fallback_from_directml(&error), "{error}");
    }
    for kind in [
        MlErrorKind::InvalidRequest,
        MlErrorKind::LimitExceeded,
        MlErrorKind::ModelMissing,
        MlErrorKind::ModelMismatch,
        MlErrorKind::Cancelled,
        MlErrorKind::Protocol,
        MlErrorKind::WorkerMissing,
    ] {
        let error = TaskError::worker(kind, "inference: diagnostic wording cannot enable fallback");
        assert!(!should_fallback_from_directml(&error), "{error}");
    }
}

#[test]
fn local_validation_failures_do_not_restart_the_worker() {
    for kind in [
        MlErrorKind::InvalidRequest,
        MlErrorKind::LimitExceeded,
        MlErrorKind::Cancelled,
    ] {
        assert!(!request_failure_requires_restart(Some(kind)));
    }
    for kind in [
        MlErrorKind::Timeout,
        MlErrorKind::Transport,
        MlErrorKind::Protocol,
        MlErrorKind::WorkerStopped,
        MlErrorKind::Inference,
        MlErrorKind::ModelMissing,
        MlErrorKind::ModelMismatch,
    ] {
        assert!(request_failure_requires_restart(Some(kind)), "{kind:?}");
    }
}

#[test]
fn unrecognized_wire_error_kinds_are_classified_as_protocol() {
    assert_eq!(
        MlError::from_response("inference", "kernel failed".into()).kind,
        MlErrorKind::Inference
    );
    assert_eq!(
        MlError::from_response("transport", "broken pipe".into()).kind,
        MlErrorKind::Transport
    );
    for message in [
        "timeout: expired",
        "cancelled: stopped",
        "provider_unavailable: missing",
    ] {
        assert_eq!(
            MlError::from_response("unknown", message.into()).kind,
            MlErrorKind::Protocol
        );
    }
}

#[test]
fn index_failure_classification_uses_the_kind_not_diagnostic_text() {
    for kind in [
        MlErrorKind::Timeout,
        MlErrorKind::Transport,
        MlErrorKind::WorkerStopped,
        MlErrorKind::Inference,
    ] {
        let error = TaskError::worker(kind, "model_missing: arbitrary message");
        assert!(is_transient_worker_failure(&error));
        assert!(!is_deterministic_worker_failure(&error));
    }
    for kind in [
        MlErrorKind::Protocol,
        MlErrorKind::ModelMissing,
        MlErrorKind::InvalidRequest,
        MlErrorKind::WorkerMissing,
    ] {
        let error = TaskError::worker(kind, "cancelled: arbitrary message");
        assert!(is_deterministic_worker_failure(&error));
        assert!(!is_transient_worker_failure(&error));
        assert!(!error.is_pause());
    }
    for message in [
        "commit failed: protocol: database busy",
        "worker reported inference: but did not provide a kind",
    ] {
        let error = TaskError::from(message);
        assert!(!is_transient_worker_failure(&error));
        assert!(!is_deterministic_worker_failure(&error));
    }
}

#[test]
fn success_requires_matching_response_variant_and_model() {
    let request = MlRequest::EmbedText {
        request_id: 9,
        timeout_ms: 1_000,
        model: MlSemanticModel::BgeSmallZh,
        texts: vec!["text".to_string()],
    };
    let expected = expected_response(&request);
    let model = request_model(&request);
    let timings = MlSemanticTimings {
        model_load_ms: 0.0,
        preprocess_ms: 0.0,
        inference_ms: 0.0,
        request_total_ms: 0.0,
        cpu_ms: 0.0,
        model_load_cpu_ms: 0.0,
    };

    let matching = MlResponse::EmbeddingComplete {
        request_id: 9,
        model: MlSemanticModel::BgeSmallZh,
        dimensions: 2,
        vectors: vec![vec![0.0, 1.0]],
        timings: timings.clone(),
    };
    assert!(response_matches_request(expected, 9, model, &matching));

    let wrong_model = MlResponse::EmbeddingComplete {
        request_id: 9,
        model: MlSemanticModel::MinilmL12,
        dimensions: 2,
        vectors: vec![vec![0.0, 1.0]],
        timings,
    };
    assert!(!response_matches_request(expected, 9, model, &wrong_model));

    let wrong_variant = MlResponse::ShuttingDown { request_id: 9 };
    assert!(!response_matches_request(
        expected,
        9,
        model,
        &wrong_variant
    ));

    let wrong_id = MlResponse::Unloaded { request_id: 8 };
    let unload = MlRequest::Unload { request_id: 9 };
    assert!(!response_matches_request(
        expected_response(&unload),
        9,
        None,
        &wrong_id
    ));
}

#[test]
fn retry_budget_is_rewritten_into_the_request_frame() {
    let mut request = MlRequest::EmbedText {
        request_id: 1,
        timeout_ms: 600_000,
        model: MlSemanticModel::BgeSmallZh,
        texts: vec!["text".to_string()],
    };
    set_request_timeout(&mut request, Duration::from_millis(1_500));
    match request {
        MlRequest::EmbedText { timeout_ms, .. } => assert_eq!(timeout_ms, 1_500),
        other => panic!("unexpected request variant: {other:?}"),
    }
}

#[tokio::test]
async fn a_busy_worker_is_refused_inside_the_deadline_rather_than_waited_out() {
    // The regression this guards: the gate used to be taken before the
    // deadline existed, so a query with a 5 s budget queued behind a
    // background batch with a 120 s one waited the full batch out. Nothing
    // downstream could rescue it, because no failure had been observed yet.
    let gate = tokio::sync::Mutex::new(());
    let held = gate.lock().await;
    let deadline = Instant::now() + Duration::from_millis(50);
    let started = Instant::now();
    let error = acquire_request_slot(&gate, deadline, 7)
        .await
        .expect_err("a held gate must not be waited out past the deadline");
    assert_eq!(error.worker_kind(), Some(MlErrorKind::Timeout));
    assert!(error.message().contains("request 7"), "{error}");
    assert!(started.elapsed() < Duration::from_secs(2), "returned late");
    drop(held);

    // A free gate is handed over without consuming any of the budget.
    let deadline = Instant::now() + Duration::from_secs(5);
    assert!(acquire_request_slot(&gate, deadline, 8).await.is_ok());
}

#[tokio::test]
async fn a_cancelled_queued_request_leaves_the_slot_available_to_its_successor() {
    let gate = tokio::sync::Mutex::new(());
    let held = gate.lock().await;
    let lease = ExecutionLease::new("semantic_index", ExecutionProfile::Background, 20);
    let cancel = lease.clone();
    let future = background_policy::EXECUTION.scope(
        lease,
        acquire_request_slot(&gate, Instant::now() + Duration::from_secs(30), 123),
    );
    let (result, ()) = tokio::join!(future, async move {
        tokio::time::sleep(Duration::from_millis(5)).await;
        cancel.revoke("foreground_request");
    });
    assert!(matches!(
        result.unwrap_err(),
        TaskError::Paused("foreground_request")
    ));
    drop(held);
    assert!(
        acquire_request_slot(&gate, Instant::now() + Duration::from_secs(1), 124)
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn queued_capture_defers_on_power_change_without_interrupting_the_slot_owner() {
    let gate = tokio::sync::Mutex::new(());
    let held = gate.lock().await;
    let saving_power = std::sync::atomic::AtomicBool::new(false);
    let queued = acquire_request_slot_checked(
        &gate,
        Instant::now() + Duration::from_secs(30),
        125,
        true,
        || {
            if saving_power.load(Ordering::SeqCst) {
                Err(TaskError::Paused("waiting_for_ac_power"))
            } else {
                Ok(())
            }
        },
    );
    let (result, ()) = tokio::time::timeout(Duration::from_secs(1), async {
        tokio::join!(queued, async {
            tokio::time::sleep(Duration::from_millis(5)).await;
            saving_power.store(true, Ordering::SeqCst);
        })
    })
    .await
    .expect("power deferral must not wait for the worker deadline");
    let error = result.unwrap_err();
    assert!(matches!(error, TaskError::Paused("waiting_for_ac_power")));
    assert!(error.worker_kind().is_none());
    assert!(
        gate.try_lock().is_err(),
        "the running request still owns its slot"
    );
    drop(held);
    // Foreground requests have no capture power policy and remain usable.
    assert!(
        acquire_request_slot(&gate, Instant::now() + Duration::from_secs(1), 126)
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn power_deferral_takes_precedence_over_a_queued_request_timeout() {
    let gate = tokio::sync::Mutex::new(());
    let _held = gate.lock().await;
    let checks = std::sync::atomic::AtomicUsize::new(0);
    let error = acquire_request_slot_checked(&gate, Instant::now(), 127, true, || {
        if checks.fetch_add(1, Ordering::SeqCst) > 0 {
            Err(TaskError::Paused("waiting_for_ac_power"))
        } else {
            Ok(())
        }
    })
    .await
    .unwrap_err();
    assert!(matches!(error, TaskError::Paused("waiting_for_ac_power")));
}

#[tokio::test]
async fn a_deadline_that_has_already_passed_never_blocks() {
    let gate = tokio::sync::Mutex::new(());
    let _held = gate.lock().await;
    let error = acquire_request_slot(&gate, Instant::now(), 9)
        .await
        .expect_err("an expired deadline cannot acquire a held gate");
    assert_eq!(error.worker_kind(), Some(MlErrorKind::Timeout));
}

#[test]
fn semantic_runtime_directory_is_first_in_worker_path() {
    let runtime_dir = Path::new(r"C:\CarbonPaper\onnxruntime\1.24.2");
    let inherited = OsStr::new(r"C:\Windows\System32;C:\Windows");
    let joined = prepend_runtime_search_path(runtime_dir, Some(inherited)).unwrap();
    let paths = std::env::split_paths(&joined).collect::<Vec<_>>();
    assert_eq!(paths.first().map(PathBuf::as_path), Some(runtime_dir));
    assert!(paths
        .iter()
        .any(|path| path == Path::new(r"C:\Windows\System32")));
}

#[test]
fn a_foreground_lease_is_visible_to_background_callers_and_released_on_drop() {
    // The signal the background loops poll. A lease that outlived its
    // request would stop capture indexing and Smart Cluster scoring for the
    // rest of the process's life, which is why it is a guard and not a
    // matched pair of calls.
    let state = Arc::new(SemanticRuntimeState::new());
    assert!(!state.foreground_waiting());
    {
        let _lease = state.foreground_lease();
        assert!(state.foreground_waiting());
    }
    assert!(!state.foreground_waiting());
}

#[test]
fn concurrent_foreground_queries_hold_the_signal_until_the_last_one_finishes() {
    // Two search boxes, or a retry racing the request it replaced. Counting
    // rather than flagging is what keeps the first one to finish from
    // telling the background loops the coast is clear.
    let state = Arc::new(SemanticRuntimeState::new());
    let first = state.foreground_lease();
    let second = state.foreground_lease();
    assert!(state.foreground_waiting());
    drop(first);
    assert!(state.foreground_waiting());
    drop(second);
    assert!(!state.foreground_waiting());
}

#[test]
fn external_background_leases_are_counted_and_released_on_drop() {
    let state = Arc::new(SemanticRuntimeState::new());
    assert!(!state.external_background_waiting());
    let first = state.external_background_lease();
    let second = state.external_background_lease();
    assert!(state.external_background_waiting());
    drop(first);
    assert!(state.external_background_waiting());
    drop(second);
    assert!(!state.external_background_waiting());
}
