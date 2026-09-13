# Python Removal Roadmap

This page records the Python-removal work as it exists in the current source
tree. It is a maintenance document, not a release announcement. File and
symbol names are the durable references; line numbers are intentionally omitted.

## Source Snapshot

- Repository: `D:\projects\carbonPaper\carbonPaper`
- Branch: `feat/rust-task-vectors-classification`
- Implementation baseline: `693d45a` (task-vector synchronization).
- Application manifest version: `0.8.5`; the repository also has a `v0.8.5` tag.
- The earlier inference cleanup was committed as `62eb619`.
- This page includes the native classification changes committed alongside this
  revision of the page, following the baseline. Historical validation below belongs to its
  recorded source snapshot and does not validate later changes.

## v0.8.4 Comparison

The release gate for `v0.8.4` and earlier migrations is treated as passed for
this roadmap, as required by the release decision for this branch. That
includes the migration, compatibility, and rollback checks completed before
this cleanup. The checks still required here are the checks for deleting the
now-unused implementation and its user-facing controls.

The relevant difference between the tag and the development baseline is:

| Reference | Result relevant to Python removal |
| --- | --- |
| `870deacd` (`v0.8.4`) | Already contains the M2.5 Chinese-CLIP, bge-reranker, and BGE classification Rust cutovers. |
| `8b64857` | Adds the persistent USearch-backed CLIP ANN index and its settings/status surface; the baseline contains this commit. |
| `24a09f3` | Development baseline used for the August cleanup validation. |
| `62eb619` | Commits the retired inference, queue, fallback, rollback-control and migration-oracle cleanup. |

The August release checks remain historical evidence for that cleanup. New
consumer migrations require their own validation.

## Current Target: Remaining Python Consumers

The cleanup keeps these four goals:

1. Keep production inference and derived-index ownership in Rust.
2. Keep Python only where a live product consumer still requires it.
3. Replace feature rollback switches with truthful status, retry, rebuild, or
   migration controls.
4. Make the remaining historical data paths read-only and resumable.

There is no Python production fallback for OCR, Chinese-CLIP retrieval,
bge-reranker scoring, BGE embedding, or Smart Cluster queue draining in this
target. A Rust failure is reported as a failure. It is not silently answered by
starting another inference implementation.

## Ownership Boundary

### Rust-owned production paths

The following paths are implemented and scheduled by the Tauri/Rust backend:

- Capture-side OCR in `src-tauri/src/ml_runtime.rs` and
  `src-tauri/src/bin/ml.rs`, using the pinned `rapidocr-core` model runtime.
- MiniLM text encoding, derived semantic storage, natural-language text
  retrieval, and idle/manual indexing in `semantic_query.rs`,
  `semantic_runtime.rs`, and `minilm_index.rs`. Historical clustering ranges and
  Chroma consumer reconciliation are owned by `task_vector_sync.rs`.
- Chinese-CLIP image encoding, image retrieval, persistent ANN maintenance, and
  exact-search recovery in `clip_index.rs`, `clip_query.rs`, and `clip_ann.rs`.
- Category scoring, scoped anchors, correction learning and postprocessing in
  `classification/`, with BGE embeddings from `classification_runtime.rs`.
  Anchor migration and durable feedback are stored in `storage/classification.rs`.
- Cross-encoder reranking in `rerank.rs` and the Smart Cluster scoring worker
  in `smart_cluster_scoring.rs`. The queue is drained by the Rust worker only.
- Screenshot, OCR, vector, Smart Cluster, MCP, lifecycle, and index-health
  persistence in the storage and command modules.

The backend status commands expose model, index, queue, timing, and last-error
data from these Rust paths. They do not expose a Python backend selector or a
Python fallback counter.

### Python-owned product paths that remain

Python remains a deliberately smaller service for live consumers that have not
yet moved:

- HDBSCAN/PaCMAP task clustering in `monitor/task_clustering.py`, including the
  `task_vectors` hot layer and `task_centroids` cold layer. Rust produces new
  MiniLM vectors and sends them to the Python Chroma hot layer through the
  `upsert_task_vectors` command; Python consumes those vectors for clustering.
- Presidio and spaCy PII analysis in the Presidio worker modules.
- Monitor lifecycle, authenticated named-pipe dispatch, and storage-session gating in `monitor/monitor/__init__.py` and related IPC
  modules. Automatic scheduling, authorization admission and retries are owned
  by `background_scheduler.rs`; the Python scheduler is a compatibility facade.
- The read-only legacy CLIP exporter in `monitor/legacy_clip_export.py`, only
  for an interrupted migration of the old Chroma `screenshots` collection.

Python must not regain OCR, Chinese-CLIP inference, semantic retrieval,
reranking, classification or BGE inference, or Smart Cluster queue-write/drain ownership as a
side effect of a restart, missing model, or ordinary Rust error.

## Data and Migration Contracts

SQLite remains the source of truth for screenshots, OCR, metadata, and Smart
Cluster state. Vectors and ANN structures are derived data. They can be copied,
rebuilt, or discarded, but they must remain versioned and diagnosable.

### CLIP image migration

`src-tauri/src/clip_migration.rs` drives a resumable page cursor against four
Python commands:

- `start_clip_vectors_export`
- `get_clip_vectors_export_status`
- `export_clip_vectors_page`
- `finish_clip_vectors_export`

Those commands are implemented by `LegacyClipVectorExporter`. The exporter may
read the existing Chroma `screenshots` collection, create no collection for a
missing source, and has no encode, query, upsert, or delete operation. Rust
maps exported IDs to live SQLite image hashes, validates dimensions and
finiteness, commits pages transactionally, and records unmappable rows.

The old collection is retained only until the persisted CLIP migration is
settled. New captures and normal image search use Rust storage/index paths.

### MiniLM task-vector migration

`src-tauri/src/minilm_migration.rs` can import the existing Chroma
`task_vectors` collection through its snapshot commands. The migration is
read-only on the Python side and uses a persisted cursor. The Rust capture/index
worker writes current vectors through `upsert_task_vectors`; Python task
clustering continues to read the hot layer and writes cold centroids.

Before every clustering run, `task_vector_sync.rs` reconciles the requested
range against the live SQLite screenshots. Current vectors are reused only
when their model/source contract matches. Missing vectors in a manual range,
including ranges older than 30 days, are encoded by the Rust MiniLM worker.
Automatic runs queue missing semantic work and defer clustering until it exists.

`storage/task_vector_sync.rs` stores a bounded snapshot's range, high watermark,
Chroma collection identity and acknowledged cursor in `task_vector_sync`.
A failed write or lost response leaves the page replayable. Recreating the
collection resets the cursor; each completed reconciliation starts a fresh scan
on the next run, including partial gaps. A failed immediate capture mirror wakes
the durable clustering task. Archive authorization, database generation,
maintenance and model arbitration still apply. `monitor_get_clustering_status`
includes the persisted `vector_sync` state.

The Python consumer has no MiniLM encoder or historical backfill. Both Chroma
collections disable embedding functions. Python's CPU ONNX dependency remains
for Chroma; the environment installer repairs the overlapping DirectML wheel
left by earlier versions. Native model runtime assets are independent.

### Classification anchors and feedback

`classification/scoring.rs` owns title/OCR blending, local/global anchor scopes,
weighted cosine scores, browser-title cleanup, process priors, deduplication and
negative feedback. Synthetic fixtures in `classification/fixtures/parity.json`
record outputs from the retired classifier at `693d45a`, including its distinct
production and diagnostic title-channel behavior.

`load_classification_anchors` imports the data directory's `anchors.json` into
the versioned `classification_anchors` SQLite row. Both legacy string entries and
structured entries are accepted; scope, process, source, weight, date and extra
metadata are retained. Invalid input leaves the source file and database import
state unchanged. The original file is retained, and later loads use SQLite.

A user category update writes a `classification_feedback` intent in the same
transaction. The intent holds screenshot/category references and a source
revision. Native learning reads the archive only with silent-read authorization,
then commits changed anchors and removes the intent together. Inference errors
retain the intent with bounded retries. Old database generations, anchor revisions
and changed screenshot sources cannot commit stale learning results.

The native queue admits up to eight capture/staged jobs. Ordinary work uses
`postprocess_lease` plus source and user-correction revisions; staged work retains
the broker receipt and transactional completion contract. Queue backpressure and
foreground scheduling deferrals preserve the retry budget. A classification or
feedback operation shares a two-minute embedding deadline across its requests.
Classification runs during user activity and yields to foreground model work.
The Python classification worker, BGE reverse-IPC bridge and category/staged-result
callback commands have been removed.

### Historical Smart Cluster thresholds

`commands/smart_cluster.rs` and `smart_cluster_scoring.rs` retain the provenance
of thresholds created by the retired Python scorer. A Python provenance record
causes re-derivation under the current Rust scorer; it is not a runtime rollback
and it does not start the deleted Python worker.

## Controls and Failure Behavior

The frontend settings surface in `src/components/settings/advanced/` describes
Rust ownership and operational state. Removed controls include semantic/CLIP/
classification backend selectors, Python fallback counters, and monitor-side
runtime ownership fields.

Remaining controls are operational:

- refresh status;
- run or stop an explicit Rust index pass;
- rebuild or retry a derived index where the Rust command supports it;
- repair a missing Rust OCR model; and
- start/stop the remaining Python monitor service when a live Python consumer
  requires it.

An unavailable Rust model or index returns a visible error/status state. It does
not silently switch to Python. A foreground query can refuse with a reason such
as migration or maintenance in progress; that refusal is not a fallback.

Bulk background model work remains idle-gated or explicitly user initiated.
Capture classification uses the immediate policy and can run during user activity. Named
pipe requests retain authentication, sequence/replay checks, bounded payloads,
and deadlines.

## v0.8.5 Beta Release Gates

The cleanup is ready for a Beta release only when all of the following are true:

1. `cargo fmt --manifest-path src-tauri/Cargo.toml --all` produces no diff.
2. `cargo check --manifest-path src-tauri/Cargo.toml` succeeds and refreshes
   the bundled monitor archive when the build script requires it.
3. Rust library tests pass with
   `cargo test --manifest-path src-tauri/Cargo.toml --lib`.
4. The Python suite passes in the CarbonPaper Python 3.12 environment with
   `python -m pytest monitor/tests -q --timeout=60`, including the pre-bundle
   synchronization tests.
5. Frontend tests, i18n validation, and the production build pass:
   `npm run test:frontend`, `npm run i18n:check`, and `npm run build`.
6. Security and focused backend checks pass:
   `npm run test:security` and `npm run test:backend:fast`.
7. `npm run tauri:build` completes, or any failure is demonstrated to be an
   external machine/asset/signing limitation rather than a source failure.
8. Migration and export contract tests cover resumable persisted cursors, an
   empty export for a missing legacy CLIP collection, and the absence of Python
   write or inference operations from the legacy exporter.
9. A source scan finds no feature selector or dispatch path that can restore
   Python OCR, CLIP inference, reranking, BGE inference, or Smart Cluster
   draining.

### Validation record: 2026-08-20

These results were recorded against `24a09f3` plus the dirty branch working
tree described in the source snapshot:

| Gate | Result |
| --- | --- |
| Rust formatting and compile | Passed `cargo fmt`, `cargo fmt --check`, and `cargo check`. |
| Rust library tests | Passed: 471 tests, 1 ignored. |
| Python tests | Passed in `C:\Users\24540\AppData\Local\carbonpaper\.venv`: 162 tests. The only warning is a Torch deprecation warning loaded indirectly by the retained Presidio test environment. |
| Frontend and i18n | Passed: 41 Vitest files and 273 tests, `npm run i18n:check`, and `npm run build`. Vite reported only its existing large-chunk advisory. |
| Security and focused backend | Passed `npm run test:security` and `npm run test:backend:fast`; the latter included 63 Python regression tests, 471 Rust tests with 1 ignored, and 9 pre-bundle synchronization tests. |
| Release build | Passed `npm run tauri:build`, including release OCR, Office, and semantic runtime probes, NSIS packaging, portable packaging, and final bundle verification. The artifacts retain `0.8.4` in their filenames because the manifest version has not been bumped. |
| Migration/export contracts | Covered by the Rust migration tests plus `test_minilm_migration.py`, `test_monitor_worker_contracts.py`, and `test_legacy_clip_export.py`. The tests pin resumable state, authenticated command dispatch, a harmless missing legacy collection, and the exporter's read-only surface. No separate manual end-to-end legacy Chroma smoke was run. |
| Retired-path source scan | Passed for `use_onnx`, `CARBONPAPER_USE_ONNX`, `pytorch_fallback`, `rerank_runtime`, `clip_runtime`, `external_backend`, and Python inference fallback dispatch. Remaining matches are negative contract tests or historical scorer provenance. |

The retained fallback terms describe different failure domains: DirectML to
CPU provider recovery, CLIP ANN to exact search, classification's OCR-text
channel, Presidio service recovery, and historical Python scorer provenance.
They do not restore a Python OCR, retrieval, embedding, rerank, or queue-drainer
implementation.

### Task-vector synchronization validation: 2026-09-13

Validated against `49f4761` plus this branch's task-vector changes: Rust library
tests passed (640 passed, 1 ignored); the CarbonPaper Python 3.12 suite passed
(183 tests); `cargo check` and the security guards passed. The tests cover
acknowledged cursor recovery after reopening SQLite, partial/replayed writes,
collection replacement, stale database rejection, old date ranges, background
authorization and absence of the Python encoder. Release packaging and a real
legacy-data migration were not rerun for this batch.

### Native classification validation: 2026-09-14

Source: the native classification change following `693d45a`, recorded with this
revision of the page. Rust formatting and `cargo check` passed. Rust library
tests passed (658 passed, 1 ignored), including 40 frozen classification cases,
three feedback sequences, anchor import, transactional feedback, result leases,
user-correction precedence and staged completion. The Python suite passed
(132 tests), frontend tests passed (43 files, 305 tests), and i18n validation,
the production frontend build and security guards passed.

`npm run test:app-bound` passed 39 Rust tests and nine packaging/signature tests;
the administrator-only service-repair test remained ignored. A disposable Chroma
1.5.1 database verified compatibility with an existing default-embedding
collection, partial repair, replay, collection replacement and stale-target
rejection. Classification parity uses synthetic embeddings. A full signed
release build and end-to-end classification against existing user archives were
not run for these changes.

## Later Milestones

The following work is intentionally not part of the v0.8.5 Beta cleanup:

- Move task clustering's HDBSCAN/PaCMAP orchestration and its Chroma hot/cold
  consumer only after a replacement consumer and data migration are accepted.
- Keep Presidio/spaCy until the MCP PII contract has a replacement with the same
  language/model behavior and an explicit resource policy.
- After those consumers are gone, remove the Python monitor process, its named
  pipe lifecycle, and the remaining Chroma operational dependencies.
- Remove the legacy CLIP exporter and old `screenshots` collection only after
  every persisted CLIP migration is settled or has an explicit, recoverable
  discard decision.

No future milestone may reintroduce a hidden fallback merely to make a missing
model appear available. Recovery must be an explicit repair, rebuild, retry, or
resumable migration.

## Evidence Index

The current implementation is backed by these source areas:

| Topic | Evidence |
| --- | --- |
| Rust composition and command registration | `src-tauri/src/lib.rs`, `src-tauri/src/commands/` |
| Semantic indexing and query | `src-tauri/src/semantic_query.rs`, `src-tauri/src/minilm_index.rs`, `src-tauri/src/semantic_runtime.rs` |
| CLIP indexing, ANN, and migration | `src-tauri/src/clip_index.rs`, `src-tauri/src/clip_ann.rs`, `src-tauri/src/clip_migration.rs`, `src-tauri/src/clip_query.rs` |
| Rerank and Smart Cluster scoring | `src-tauri/src/rerank.rs`, `src-tauri/src/smart_cluster_scoring.rs`, `src-tauri/src/commands/smart_cluster.rs` |
| Native classification | `src-tauri/src/classification/`, `src-tauri/src/storage/classification.rs`, `src-tauri/src/classification_runtime.rs` |
| Python retained service | `monitor/monitor/__init__.py`, `monitor/task_clustering.py`, `monitor/monitor/presidio_service.py` |
| Legacy export and task-vector IPC | `monitor/legacy_clip_export.py`, `monitor/monitor/clustering_commands.py`, `monitor/storage_client.py`, `monitor/tests/test_legacy_clip_export.py` |
| Frontend status and controls | `src/components/settings/advanced/InferenceCards.jsx`, `src/components/settings/useAdvancedSectionController.js`, `src/lib/monitor_api.js` |
| Security and contract tests | `scripts/security-guards.cjs`, `monitor/tests/`, `src/lib/api_contracts.test.js`, Rust module tests |

## Maintenance Record

| Date | Source | Maintenance |
| --- | --- | --- |
| 2026-08-20 | `24a09f3` plus the dirty branch working tree | Rebased the roadmap on the passed v0.8.4 gates, documented the v0.8.5 Beta ownership boundary, recorded the successful Rust, Python, frontend, security, and release-build checks, and limited the migration claim to the automated contracts that were actually run. |
| 2026-09-14 | `693d45a` plus the native classification change committed with this page | Recorded Rust task-vector reconciliation, native classification and feedback ownership, retained Python consumers, and the validation for both batches. |
