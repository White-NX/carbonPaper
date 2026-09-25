# Python Removal Roadmap

This page records the Python-removal work as it exists in the current source
tree. It is a maintenance document, not a release announcement. File and
symbol names are the durable references; line numbers are intentionally omitted.

## Source Snapshot

- Repository: `D:\projects\carbonPaper\carbonPaper`
- Branch: `feat/adaptive-background-scheduling`
- Implementation baseline: `ed55d73` (merged Rust task-vector synchronization and classification).
- Application manifest version: `0.8.5`; the repository also has a `v0.8.5` tag.
- The earlier inference cleanup was committed as `62eb619`.
- This page includes the native classification changes committed alongside this
  revision of the page, following the baseline. Historical validation below belongs to its
  recorded source snapshot and does not validate later changes.
- The current working tree also retires the automatic task module and its UI.
- The 2026-09-20 revision retires the legacy Chroma vector exporters and both
  sentinel-triggered vector copies in favour of an explicit, recorded discard.
- The 2026-09-24 revision moves personal information detection for MCP
  responses to Rust rules and removes Presidio and spaCy.
- The 2026-09-25 revision removes the Python monitor process, its named pipes,
  the Python launcher, the `monitor.pyz` package and the Python environment
  setup. The application no longer starts or installs Python.

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

Rust automatic work now uses A (60 seconds without input) and locally qualified,
CPU-budgeted B during ordinary use. MiniLM, CLIP, Smart Cluster and the resumable ANN task use independent admission and cancellation
boundaries. ANN input pages and complete checkpoints survive restarts without a
long capture-pause window. Details and repeatable checks are in
[Adaptive background scheduling](adaptive-background-scheduling.md).

The PaCMAP/HDBSCAN task module has been retired, including its UI, scheduled
runs, Chroma vector writes and synchronization. The read-only legacy vector
exports and the two startup Chroma copies are retired as well. Personal
information detection now runs as Rust rules in `pii/`. With no product feature
left in Python, the Python monitor process itself has been removed. ML worker protocol 4
supplies request-level cancellation and uses the shared semantic worker for
foreground requests.

The cleanup keeps these four goals:

1. Keep production inference and derived-index ownership in Rust.
2. Run no Python at all: no child process, no environment and no installer.
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
  `semantic_runtime.rs`, and `minilm_index.rs`. Automatic text indexing follows
  the Smart Cluster feature setting; explicit indexing remains available.
- Chinese-CLIP image encoding, image retrieval, persistent ANN maintenance, and
  exact-search recovery in `clip_index.rs`, `clip_query.rs`, and `clip_ann.rs`.
- Category scoring, scoped anchors, correction learning and postprocessing in
  `classification/`, with BGE embeddings from `classification_runtime.rs`.
  Anchor migration and durable feedback are stored in `storage/classification.rs`.
- Cross-encoder reranking in `rerank.rs` and the Smart Cluster scoring worker
  in `smart_cluster_scoring.rs`. The queue is drained by the Rust worker only.
- Screenshot, OCR, vector, Smart Cluster, MCP, lifecycle, and index-health
  persistence in the storage and command modules.
- Personal information detection for MCP responses in `pii/`, combined with
  the sensitive-word dictionary in `sensitive_filter.rs` and applied per field
  in `mcp_server.rs`.

The backend status commands expose model, index, queue, timing, and last-error
data from these Rust paths. They do not expose a Python backend selector or a
Python fallback counter.

### Python monitor removal

Nothing runs in Python. The capture lifecycle (start, pause, resume, stop) is
held by `monitor.rs` as in-process state around the Rust capture loop; the
monitor status command reports that state directly. Automatic scheduling,
authorization admission and retries are owned by `background_scheduler.rs`.

The removal deleted:

- the `monitor/` Python package, its tests and requirements;
- the monitor named-pipe client, the Job Object that constrained the child
  process, and the Python-to-Rust reverse storage pipe (`reverse_ipc.rs` keeps
  only the browser native-messaging pipe);
- `carbonpaper-python.exe`, `python_launcher.rs`, `python.rs` (interpreter
  discovery, venv creation, dependency synchronisation and the elevated
  `--silent-install-python` entry point);
- `build_pyz.py`, the `monitor.pyz` archive, `script_integrity.rs` and the
  security-alert overlay that reported a tampered archive;
- the bundled Python 3.12.10 installer and its release-asset checks;
- the first-run environment wizard and dependency-update overlay in the
  frontend; the required-model download overlay remains.

Game mode no longer restarts anything when DirectML suppression changes. The
OCR and semantic workers read the suppression flags at each request, and the
resident DirectML semantic worker is stopped when suppression begins.

`build.rs` removes `monitor/`, `monitor.pyz`, the Python installer and the
launcher from `pre-bundle/` if an older build left them there, because Tauri
bundles that directory as a whole. An upgrade only overwrites what the new
bundle carries, so the installer's post-install hook deletes the same four
names (`monitor/`, `monitor.pyz`, `carbonpaper-python.exe` and the Python
installer) from the install directory.

The Python environment at `%LOCALAPPDATA%\CarbonPaper\.venv` (several GiB on a
typical machine) is removed by the application itself
(`legacy_python_cleanup.rs`). About 90 seconds after startup, a thread in
Windows background mode checks that `.venv` is a real directory rather than a
link and that it contains `pyvenv.cfg`. It then renames the directory to
`.venv.removing` and deletes it. The rename means a downgraded release sees
either a whole environment or none. A failed pass (for example a leftover
`python.exe` holding a file) is retried on the next launch, and a leftover
`.venv.removing` is finished first. The Python interpreter the environment was
created from is never touched, because other software may use it. The
uninstaller still removes `.venv` and `.venv.removing` when the user chooses to
delete application data. The ONNX Runtime lookup no longer falls back to the
copy inside `.venv`; the pinned runtime ships in `onnxruntime/1.24.2`.

Python must not return as a side effect of a restart, missing model, or
ordinary Rust error, nor as a new host for any feature.

## Data and Migration Contracts

SQLite remains the source of truth for screenshots, OCR, metadata, and Smart
Cluster state. Vectors and ANN structures are derived data. They can be copied,
rebuilt, or discarded, but they must remain versioned and diagnosable.

### Legacy vector collections: explicit discard

Releases up to `v0.8.5` seeded the two derived indexes by copying the old
Chroma collections (`task_vectors` for MiniLM, `screenshots` for Chinese-CLIP)
through four Python export commands per collection, under global maintenance
mode, and kept the query and repair paths closed until a once-per-revision
sentinel in `app_metadata` said the copy had settled. Both copies, the
`LegacyVectorExporter`, `collection_export.py`, the `chromadb`/`numpy`
requirements, and the installer's Chroma ONNX repair are removed.

`src-tauri/src/legacy_vector_discard.rs` replaces them. At startup, for each
index whose sentinel is missing, it:

1. writes a `derived_migration_runs` row with mode `discard_legacy_chroma_v1`
   and status `discarded`, plus a `legacy_collection_discarded` diagnostic that
   records whether a `chroma_db` directory was present;
2. clears any earlier CLIP backfill answer, because the question is now posed
   over a different corpus;
3. settles the sentinel, so `semantic_query.rs`, `clip_query.rs`,
   `clip_index.rs::repair_scope` and the ANN bootstrap open exactly as they did
   after a completed copy;
4. once both sentinels are settled, removes the `chroma_db` directory.

Nothing is copied and nothing needs Python, an unlocked vault, or maintenance
mode. The recovery cost is bounded: MiniLM re-encodes its 30-day window from
OCR text during idle time; CLIP re-encodes the last 7 days on its own and
offers the full-history backfill through the existing
`get_clip_backfill_offer` dialog, which reports a discard under "skipped", not
"failed". Installations that completed the real copy on `v0.8.4`/`v0.8.5` keep
their sentinels, run history and backfill answer untouched.

Release note for users upgrading from `v0.8.3` or earlier: to keep the old
image vectors instead of re-encoding, upgrade to `v0.8.5` first and let its
migration finish, then upgrade again. Otherwise the discard is the documented
path and the backfill dialog is how the vectors come back.

The `derived_migration_runs`, `derived_migration_subjects` and
`derived_migration_run_errors` tables stay: they hold the history of copies
that did run, and the discard writes into the first and third. The page-import
methods that filled them are gone from `storage/derived_migration.rs`.
`clip_contract.rs` and `minilm_contract.rs` keep the vector-space constants,
job specifications and validators the live indexes share; `maintenance_support.rs`
keeps the capture pause/restore the blind-index repair uses. Backups no longer
archive `chroma_db`; older archives that contain it still restore, after which
the same discard removes it.

### Personal information detection

The Presidio worker, its Chinese recognizers, the spaCy model download and
check commands, the startup model installation, the language-sync command and
the MCP idle-unload timer are removed, together with `worker_supervisor.py`,
which only served that worker. Before removal the service ran an English
pipeline for Chinese users, because the language never reached Python, and it
returned unfiltered text on any timeout or error.

`src-tauri/src/pii/` finds phone numbers, resident ID numbers, bank cards,
e-mail addresses, street addresses, credentials and, when selected, IP
addresses. It runs in process and cannot time out. The rules are shaped by how
PP-OCRv5 misreads numbers after the capture downscale: most errors are dropped
digits, swaps between similar digits and misread separators, while letters in
place of digits are rare. A checksum therefore counts as evidence rather than
a requirement: an ID number that one inserted, deleted or replaced character
would make valid is still caught, because a reader can recover it from a
handful of candidates. Card numbers are also recognised by their four-digit
grouping, and labels such as "身份证号" in the neighbouring OCR block on the same
line or directly above relax the rules. Names are not detected, and neither are
the entities that only Presidio's English built-in recognizers produced, such as
US social security numbers and IBAN account codes. A 15-digit first-generation
resident ID number is recognised next to a label; on its own, a string must have
the 18-digit shape before the province, birth date and repair checks apply.
`pii/fixtures/ocr_error_patterns.json` records 365 observed OCR edits without
the numbers they came from; the tests replay them on generated numbers.

`SensitiveFilterConfig` version 2 makes `remove_paragraph` the default: an OCR
segment or link with a sensitive word or personal information is dropped and
an affected title or URL is replaced. A search snippet joins several segments,
so it is replaced as a whole and the hit is kept; the snapshot details return
the clean segments. `reject` and `mask` keep their meaning. The optional
long-number setting hides digit strings of eleven or more characters that
match no rule, and never removes content. Stored version 0 configurations are
upgraded on load: `presidio_*` fields are read under their new names, the old
default `reject` becomes `remove_paragraph`, an empty entity list becomes the
default set, and credentials are added to an explicit list.

The sensitive-word level and the personal information rules are switched
independently. `enabled`, the content-filter level, governs the dictionary and
its categories alone; `pii_enabled` governs the rules above. Setting the level
to 关闭 therefore stops keyword filtering only, and personal information is
still removed or masked according to `mode` and the selected kinds.

Older Python environments may still hold the Presidio and spaCy packages and
models; the application no longer starts Python, so nothing loads them.

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
- start, pause, resume or stop screenshot capture.

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
4. No Python suite remains after 2026-09-25; the historical records below ran
   `python -m pytest monitor/tests` in the CarbonPaper Python 3.12 environment.
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

### Task module retirement validation: 2026-09-19

Frontend tests passed (50 files, 324 tests), Rust library tests passed (694 passed,
1 ignored), and Python tests passed (91 service tests plus 9 bundle checks).
The Python service suite used a 60-second per-test timeout for Presidio's cold
dependency imports; the committed test timeout is unchanged. The frontend
production build, Rust formatting, i18n checks and security guards passed.
An ephemeral Chroma client with synthetic vectors verified both missing-source
exports and existing MiniLM/CLIP exports without creating missing collections.
A full signed release build and an interactive desktop smoke test were not run.

## Later Milestones

The following work is intentionally not part of the v0.8.5 Beta cleanup:

- None related to Python. The Python monitor process and its named pipe
  lifecycle were removed on 2026-09-25, after Presidio, its last product
  consumer, was replaced on 2026-09-24.

No future milestone may reintroduce a hidden fallback merely to make a missing
model appear available. Recovery must be an explicit repair, rebuild, retry, or
resumable migration.

## Evidence Index

The current implementation is backed by these source areas:

| Topic | Evidence |
| --- | --- |
| Rust composition and command registration | `src-tauri/src/lib.rs`, `src-tauri/src/commands/` |
| Semantic indexing and query | `src-tauri/src/semantic_query.rs`, `src-tauri/src/minilm_index.rs`, `src-tauri/src/semantic_runtime.rs` |
| CLIP indexing, ANN, and contract | `src-tauri/src/clip_index.rs`, `src-tauri/src/clip_ann.rs`, `src-tauri/src/clip_contract.rs`, `src-tauri/src/clip_query.rs` |
| Legacy vector discard | `src-tauri/src/legacy_vector_discard.rs`, `src-tauri/src/storage/derived_migration.rs`, `src-tauri/src/minilm_contract.rs` |
| Rerank and Smart Cluster scoring | `src-tauri/src/rerank.rs`, `src-tauri/src/smart_cluster_scoring.rs`, `src-tauri/src/commands/smart_cluster.rs` |
| Native classification | `src-tauri/src/classification/`, `src-tauri/src/storage/classification.rs`, `src-tauri/src/classification_runtime.rs` |
| Personal information detection | `src-tauri/src/pii/`, `src-tauri/src/sensitive_filter.rs`, `src-tauri/src/mcp_server.rs` |
| Capture lifecycle | `src-tauri/src/monitor.rs`, `src-tauri/src/capture.rs`, `src/hooks/useMonitorLifecycle.js` |
| Browser native-messaging pipe | `src-tauri/src/reverse_ipc.rs`, `src-tauri/src/reverse_ipc_protocol.rs` |
| Frontend status and controls | `src/components/settings/advanced/InferenceCards.jsx`, `src/components/settings/useAdvancedSectionController.js`, `src/lib/monitor_api.js` |
| Security and contract tests | `scripts/security-guards.cjs`, `src/lib/api_contracts.test.js`, Rust module tests |

## Maintenance Record

| Date | Source | Maintenance |
| --- | --- | --- |
| 2026-08-20 | `24a09f3` plus the dirty branch working tree | Rebased the roadmap on the passed v0.8.4 gates, documented the v0.8.5 Beta ownership boundary, recorded the successful Rust, Python, frontend, security, and release-build checks, and limited the migration claim to the automated contracts that were actually run. |
| 2026-09-14 | `693d45a` plus the native classification change committed with this page | Recorded Rust task-vector reconciliation, native classification and feedback ownership, retained Python consumers, and the validation for both batches. |
| 2026-09-15 | `feat/adaptive-background-scheduling` | Added A/B scheduling, task-specific local qualification, request cancellation, resumable ANN checkpoints and source-versioned vector projection. Retained the 1800-second Python full-clustering gate. See [adaptive scheduling](adaptive-background-scheduling.md) for measurements and acceptance limits. |
| 2026-09-19 | Task module retirement | Removed PaCMAP/HDBSCAN, its UI and MCP commands, vector synchronization and dedicated tests; preserved legacy records and read-only vector migration. |
| 2026-09-20 | Legacy vector discard | Removed the Chroma exporters, both startup copies, the migration overlay states, the `chromadb`/`numpy` requirements and the installer's Chroma ONNX repair; added the startup discard that settles the index sentinels and records the decision. Rust library tests (687 passed, 1 ignored), Python tests (92), frontend tests (50 files, 321 tests), i18n check, security guards and lint passed. A release build and an upgrade smoke against a real `v0.8.3` data directory were not run. |
| 2026-09-24 | Rust personal information detection | Replaced Presidio/spaCy with the rules in `pii/`, made removing the affected OCR segment the default filter mode with a version 2 configuration upgrade, loaded corpus OCR as blocks so single segments can be dropped, and removed the Presidio worker, spaCy model management, the language-sync command, the MCP idle-unload timer and their settings UI. Rust library tests (717 passed, 1 ignored), Python tests (44), frontend tests (50 files, 321 tests), i18n check, security guards, lint and the frontend production build passed. A release build and an interactive MCP session against a real archive were not run. |
| 2026-09-25 | Python monitor removal | Removed the Python monitor, its named pipes, reverse storage pipe, Job Object, launcher binary, `monitor.pyz` packaging and integrity check, Python environment setup and installer, the unused `storage_get_categories` command, and the related frontend overlays; the capture lifecycle is now in-process state in `monitor.rs`, and game mode stops restarting on DirectML changes. Rust library tests (697 passed, 1 ignored), frontend tests (50 files, 320 tests), protected-runtime packaging tests (9), i18n check, security guards, lint and `cargo check --all-targets` passed. A release build and an interactive desktop smoke test were not run. |
