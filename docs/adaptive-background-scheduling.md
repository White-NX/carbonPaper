# Adaptive background scheduling

The scheduler owns four durable task kinds: MiniLM indexing, CLIP indexing,
Smart Cluster scoring, and ANN maintenance. Capture, immediate classification,
and foreground requests retain their own priority and authorization paths.

## Admission

`background_scheduling_mode` is a partial preference in `get_advanced_config`
and authenticated `set_advanced_config`. Its default is `auto`; `idle_only`
restricts automatic work to idle windows. The existing background-processing
switch has priority over either setting.

| Rule | Implementation |
| --- | --- |
| A | AC power, valid authorization, no protected game/fullscreen session or maintenance, at least 60 seconds without input |
| B admission | Total CPU below 30% for 10 seconds; no core at 85% in two consecutive samples |
| B retreat | Total CPU at least 50%, or a core at least 95% in two consecutive samples |
| B memory | `available_bytes >= ceil(expected_additional_peak_bytes * 1.15) + ceil(0.8 * 2^30)` |
| Recovery | 15-second pressure cooldown, then 10 new seconds of low CPU |
| Worker CPU | Windows Job Object hard cap of 5% of the machine during automatic requests |
| Duty | Starts at 20%, increases by 10 points per stable minute, maximum 50%; a pause resets it |
| Short-request CPU bursts | Additional rest accounts for measured CPU time, so resetting a request's job limit cannot accumulate excess CPU |
| Disk | Cold loads and ANN require a measured latency below 10 ms for 10 seconds; 30 ms twice triggers retreat; missing counters keep these operations out of B |

The fixed 0.8 GiB reserve and **15%** extra-peak margin replace the originally
proposed 2 GiB / physical-memory percentage rule. Disk thresholds are explicit
initial parameters. Changes to them or the CPU budgets require new measurements.

Activity is sampled every 250 ms when work is pending, with resource counters
sampled each second. An input transition revokes A directly from the activity
monitor. B continues through ordinary keyboard and mouse activity. A revoked
lease cannot turn into B: the old slice must finish and receive a new admission.

## Local qualification and bounded work

`background_policy.rs` holds pure policy and the bounded history. Metadata is
stored in `app_metadata.background_performance_v1`; resource sampling remains
in memory and dirty summaries are flushed at 30-second intervals. No benchmark
or UI probe seeds production qualifications.

Every key includes task, operation, model fingerprint, input-size bucket, and
execution parameters. Hardware/runtime identity invalidates the whole cache;
model and parameter changes select new keys. CPU identity is used for cache
invalidation. No CPU brand or hardware grade grants eligibility.

Ordinary operations need 20 valid samples, retain the latest 64, and require
P95 execution time at most 500 ms. Cold loading is separate: three samples and
P95 at most one second. A model without qualified cold-loading records can only
be used in B while resident. Qualification samples come from actual A work at
the same 5% worker cap. Unrestricted/manual samples cannot grant B eligibility.
Worker timings exclude request queues and deliberate sleeps; host preparation
has its own bounded-operation measurement. Overruns revoke the affected key and
wait for fresh A samples. ONNX thread-pool spinning is disabled so completion
actually releases CPU when the request budget is restored.

Automatic archive requests claim one record. B finishes a bounded unit without
retention repair or a full maintenance pass. Smart Cluster commits a completely
scored record against the original source revisions and cluster configuration.
Staged paths retain broker receipts, generation fences, and their normal lease
checks. Vector projection is a separate task with acknowledged cursors and a
durable rescan marker for updates arriving during a pass. Source revisions also
travel to the Python consumer, so a delayed page acknowledges a superseded row
without overwriting its newer vector. Every fourth automatic
turn reserves service for authorized archive debt. Completion timestamps change
only when a task finishes its remaining work, not when it pauses or rotates.

## Cancellation and model ownership

ML protocol **4** includes an out-of-band `cancel` control carrying the target
request ID. A separate worker reader delivers it to that request's ONNX
`RunOptions::terminate()`. It emits no extra response frame. Requests retain a
single terminal response, and late controls cannot terminate the next request.

The desktop watches startup, loading and inference. It kills the specific worker
instance after a 500 ms cancellation grace. Intentional cancellation requeues
unfinished records without model-failure backoff or consuming item attempts.
The request gate encloses both budget restoration and cancellation cleanup;
foreground and immediate-classification successors receive the normal budget.
An idle model can remain resident for five minutes and is reclaimed under memory
pressure. ANN workers have the same retention boundary for paused in-memory work.

## ANN recovery

`clip_ann::maybe_rebuild` and the migration bootstrap enqueue `ann_build`.
Construction has no capture-pause or global-maintenance window. An existing
queryable generation stays available until the final publication boundary.

`ann_build_checkpoints` persists the dataset, model/format contract, original
data epoch, fixed keyset upper bound, scan cursor, flat-file cursor, and the two
latest complete graph checkpoints. `ann_build_inputs` freezes paged raw vectors
inside the existing encrypted database. Each input page and its cursor commit
in one short transaction. Concurrent writes stay in `derived_ann_changes` and
overlay the eventual generation, including deletions.

The isolated `carbonpaper-ml --ann-session` builder checks cancellation before
every insertion. After 5000 new insertions or 30 seconds of build computation it
serializes a full checkpoint. Serialization, chunked file writes, readback,
checksum, restoration and graph probes are separate phases. Only a verified,
fsynced file can be renamed and recorded as the new durable cursor; the previous
complete checkpoint remains available. Pause does not force a large save.
Restart may replay the uncommitted suffix from the last complete checkpoint.

Partial flat pages are overwritten from their committed cursor. Missing or
invalid checkpoints fall back to the preceding checkpoint or frozen input.
Dataset, database-generation and model/format changes fence stale work. Final
files are checked before the short manifest transaction and reader switch.
Frozen-input cleanup is paged and runs in A. Ready index formats, tombstone
filtering and exact-query fallback are preserved.

B is limited to 20,000 vectors and 64 MiB each for frozen input and graph. It
requires scale-specific insertion, serialization, restoration, checksum,
validation and I/O qualifications. Explicit reads/writes use chunks of at most
256 KiB and B limits them to 2 MiB/s. Larger jobs wait for A.

## Verification and reproducible probes

Policy, cost invalidation, native Job Object restoration, source-version commits,
flat-page recovery, checkpoint readback/corruption and concurrent ANN changes
have colocated Rust tests. Run the worker tests separately from the library:

```powershell
cargo test --manifest-path src-tauri/Cargo.toml --lib
cargo test --manifest-path src-tauri/Cargo.toml --bin carbonpaper-ml
cargo test --manifest-path src-tauri/semantic-worker/Cargo.toml
npm run test:frontend
npm run i18n:check
npm run test:security
npm run test:app-bound:fast
npm run build
```

The performance probe uses the packaged ONNX runtime and installed model files,
synthetic inputs, a 5% native Job Object quota, 20 warm samples and three cold
loads per model. It tests cancellation followed by a different request ID,
compares native text-input/scroll dispatch with and without B, and records a
separate 1% CPU experiment. It writes only a local JSON report:

```powershell
npm run build:semantic-ml:release
C:\Users\24540\AppData\Local\carbonpaper\.venv\Scripts\python.exe tools/measure-adaptive-background.py
```

The UI probe covers a hidden Windows RichTextBox's input, layout, scrolling and
message dispatch. Application-wide input-to-yield measurements and real Office /
WebView rendering remain separate end-to-end checks in a running desktop app.
The existing app-bound service regression requires an administrator Windows
process; its script creates and removes an isolated temporary service.

### Local Windows measurements (2026-09-15)

Final verification on `feat/adaptive-background-scheduling`:

| Check | Result |
| --- | --- |
| Rust library | 681 passed, one pre-existing ignored test |
| ML / ANN worker | 22 passed, including losing process memory in each build phase |
| Semantic worker | 24 passed |
| Python monitor | 133 passed, with the local timeout/WMI adjustments described below |
| Frontend | 314 passed; i18n and production build passed |
| Security / app-bound | Security guards and the complete native preflight passed; 39 ordinary Rust tests plus the separate administrator service test |
| Rust checks | Both format checks and all-target checking passed |
| Release runtime | ML worker and desktop release builds passed; OCR and semantic runtime probes passed |
| ANN process smoke | A real release worker built, saved, restarted and restored a synthetic index; late cancellation left the successor and complete checkpoint intact |

The release worker used 16 logical processors with a native 5% CPU quota. These
are workload-specific probe results, not production eligibility grants:

| Operation | Warm P95 | Cold P95 | Result for this input |
| --- | ---: | ---: | --- |
| MiniLM | 5.919 ms | 1804.014 ms | Warm execution qualifies; requires a resident model |
| CLIP image encoding | 605.376 ms | 762.005 ms | Warm execution waits for A |
| BGE reranking | 510.811 ms | 2524.679 ms | Waits for A |

Cancellation-to-worker-release P95 was **475.463 ms** over 20 requests. A
different request ID succeeded after cancellation. The native text control's
250 input/layout/scroll samples measured P95 **39.471 ms** without B and
**40.124 ms** with B, a **0.653 ms** increase. A separate 1% quota run is
recorded independently and cannot qualify the 5% execution configuration.

The raw report and component preview are local artifacts at
`.tmp_pytest/adaptive/performance.json` and
`.tmp_pytest/adaptive/settings-preview.png`. The preview renders the actual
settings component in both modes. It contains no user records.

### Remaining interactive acceptance

Use synthetic text and screenshots in a disposable archive. Keep the power and
display settings fixed and finish asset/model downloads before the comparison.
Record foreground input-to-render latency, not only model throughput.

1. Run the same typing and scrolling sequence in Office and the application
   WebView with the background switch off, then with Auto and a qualified
   resident task. Compare P50/P95/P99 and keep the raw trace with the workload.
2. Leave input idle for 60 seconds, then type while A is computing. Repeat at
   least 20 times and measure the native input event through computation release;
   require P95 at most one second. Also test foreground search, power removal,
   game protection, authorization revocation, and maintenance. The diagnostic
   `last_yield_ms` starts at lease revocation, so add activity-detection time when
   measuring this end-to-end target.
3. Keep typing for several minutes under low resource use. Qualified B work
   should advance; unqualified operations and Idle Only should remain queued.
   Confirm that the foreground successor receives its ordinary CPU budget.
4. Apply a reduced worker quota and memory pressure in the disposable test run.
   Verify that unknown/slow operation keys wait for A and that memory retreat
   uses the **15% margin plus 0.8 GiB reserve**, followed by a 15-second cooldown
   and a new 10-second admission window.

These Office/WebView checks require a running, authorized application session.
The controlled worker/native-control measurements above do not claim to replace
that acceptance.

Python's default 15-second test timeout was exceeded by the initial spaCy model
load. The full suite was rerun with `--timeout=60`. CPython's optional WMI query
also blocked during pytest/readline startup on this machine; the final test
process replaced `platform._wmi_query` with an `OSError` so Python used its
standard Windows fallback. No system service or application source was changed
for this workaround. Logs are in `.tmp_pytest/adaptive/`.

The administrator app-bound preflight, including the isolated service repair,
completed before the user went AFK. Subsequent work does not request elevation.
Packaging a signed installer additionally requires
`CARBONPAPER_UPDATE_SIGNING_KEY`, which is absent in this environment; release
compilation and runtime validation can run without that signing step.
