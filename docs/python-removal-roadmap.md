# CarbonPaper v0.8.4 Beta M2 / 0.8.x Python Removal Roadmap

Baseline: CarbonPaper `0.8.3`, revised on 2026-07-18 from a direct source-code audit of the shipped tree.
Supersedes the 0.8.1-baseline plan (kept for reference in `roadmap.0.8.1-original.md`).
2026-07-19 update: Milestone 1 was reduced to an **internal migration baseline**: collection semantics, scope decisions, a stable count-level CLIP baseline, and the Chroma version pin. It is not a user-facing health feature and does not need a separate release; its per-row ledger, rebuild executor, and actionable maintenance UI belong to Milestone 2.
2026-07-19 correction: the earlier Milestone 1 "demote Chinese-CLIP / one NL text path" decision is **reversed**. It rested on a dead-code rationale (`search_by_image` / `search_by_ocr_text`) that missed the *live* CLIP consumer — `search_by_text`, the text→image backend actually serving `search_nl` from two frontend surfaces and the MCP. Text→image visual search ("search 'sea', get sea-looking screenshots") is a distinct cross-modal capability that neither the shipped Rust keyword search (`storage/search.rs::search_text`) nor a future MiniLM-over-OCR semantic search can reproduce. CLIP is now **retained as a first-class search surface**; see the Milestone 1 decisions and Milestone 2 below.
2026-07-19 implementation review: Milestone 2 is revised around **behavioral equivalence**, not merely moving an ONNX call site. Rust cutover must preserve feature availability, data coverage, filters, response contracts, ranking quality, foreground isolation, upgrade/rollback, and the still-live Python consumers. ChromaDB cannot be removed in Milestone 2 while `task_vectors` / `task_centroids` still serve Milestone 4 task clustering, and Python BGE inference cannot be removed before the classification consumer has a Rust path or an explicit Rust inference bridge.
2026-07-19 delivery decision: **Milestone 2 is the committed scope for v0.8.4 Beta**, delivered as short stacked PRs with disabled/shadow infrastructure allowed to land before individual capability cutovers. This accelerates implementation without weakening the parity, migration, rollback, foreground-isolation, or one-release fallback gates. v0.8.4 Beta completing M2 does not mean that ChromaDB or the Python distribution can be removed in the same release.
2026-07-28 cutover-scope correction (three changes, all forced by reading the shipped code rather than the plan). First, the old step 4 bundled two things the code cannot separate the way the plan assumed: Smart Cluster calibration is hard-wired to `enable_rerank = true` (`NlClusterView.jsx:95`) and Python's `query_by_text` fuses retrieval and reranking into one call with no standalone rerank command, so "cut over the calibration prefilter" would require a throwaway Rust→Python rerank bridge. Calibration moves to the reranker step instead, and step 4 cuts over the non-reranked path only. Second, the M2.5 release gate's "top-10 overlap ≥ 99%" wording was written assuming two implementations of the *same* retrieval method; the shipped Rust path is an exact scan and Chroma is ANN, so the measured divergence is a recall superset, not a defect. The gate is rewritten to say so rather than shipping against a gate known to fail on its literal text. Third, the numbered steps never contained a Rust capture-side MiniLM indexing step — new screenshots are still encoded and enqueued only by Python, which the M2.5 "with Python stopped" gate silently requires. It is now an explicit step between the query cutover and the reranker.

Goal (unchanged): gradually remove the Python monitor stack from CarbonPaper while keeping user data safe, keeping OCR/search usable, and avoiding foreground ML interference.

## Why This Revision

The original plan assigned one milestone per `0.8.x` version and assumed a **direct Python-to-Rust hop, one feature at a time**. The audit shows the codebase diverged from that plan in three important ways, so version numbers no longer describe reality:

1. **OCR is already Rust-default** (the original 0.8.5 target) even though the shipped version is `0.8.3`. It was implemented not via the in-process `OcrEngine` trait but as a standalone `carbonpaper-ml.exe` worker built on the pinned crate `rapidocr-core = "=0.2.2"`. The Python-default beta phase (original 0.8.4) and its dual-run diagnostics were **skipped entirely**.
2. **A "two-hop" migration is actually in progress** (see below). The team first unified all ML onto ONNX inside Python and lifted model-asset management into Rust, rather than porting each feature straight to Rust.
3. **The Agent/MCP parallel track ran ahead** of the Python-removal track, reaching the original 0.8.3 target.

Consequence for this document: Milestone 2 is explicitly targeted at **v0.8.4 Beta**. Later milestones remain ordered commitments rather than promises tied to a specific patch number. Do not infer later milestone status from the version number.

## The Two-Hop Migration Reality

The real path is not `Python(PyTorch) -> Rust`. It is:

- **Hop 1 — ONNX unification + Rust asset ownership (mostly done).** All ML models (OCR PP-OCRv5, CLIP, BGE, MiniLM, bge-reranker) are ONNX; `use_onnx` defaults to `true` (`model_management.rs:390`) and `torch`/`sentence-transformers` are fallback-only (`monitor/main.py:12-18` ONNX sentinel skips the torch import). Model registration, install status, sizing, and OCR **inference** are Rust-owned. Python is compressed into a thin layer: `onnxruntime-directml` inference + ChromaDB + post-process orchestration.
- **Hop 2 — move ONNX inference from Python to Rust (only OCR done).** Everything except OCR still runs its ONNX inference inside the Python `onnxruntime`.

This matters for planning: because embedding, reranking, and classification already share one Python ONNX runtime, they are cheaper to move **together** (swap the inference layer once) than one feature per release. The milestone plan is reorganized around that.

## What Actually Shipped (audited 2026-07-18)

Line references are as of the audit and will drift.

**Reliability & migration baseline (original 0.8.2) — DONE**
- Frontend image/detail queue deadlines with `deadline_exceeded` (`src/lib/monitor_api.js:15-122`).
- Python `StorageClient` reverse-IPC watchdog with per-phase deadlines and status reporting (`monitor/storage_client.py:105-113,344-497`).
- Monitor crash recovery via `MonitorRecoveryState` — chose the "recoverable state + restart" option, `policy = manual_restart`, crash counter, `monitor-recovery` events (`src-tauri/src/monitor.rs:44-74`).
- Non-silent OCR/index failure: screenshot preserved on OCR failure, `screenshot_ocr_status` carries `status` + `postprocess_status/attempts/next_retry` (`storage/schema.rs:162-173`; `capture.rs:1734-1749`); retry command `storage_retry_vector_indexing`.
- Index health panel wired to UI (`components/settings/storage/IndexHealthCard.jsx` + `storage_get_index_health`).
- Scheduled HDBSCAN is idle-gated (`monitor/task_clustering.py:1347-1354`).
- Repo hygiene clean: `git ls-files` shows no tracked db/key/pyz/venv/installer.

**OCR = Rust default (original 0.8.4 + 0.8.5) — DONE / exceeded**
- Capture hot path calls Rust OCR directly; status recorded as `engine="rust"` (`capture.rs:1785-1855`). Engine is `rapidocr-core "=0.2.2"` with `directml` feature (`src-tauri/Cargo.toml:74`).
- Rust OCR runtime has a watchdog timeout that kills a stuck worker (`ml_runtime.rs:216-295`) and a post-process retry loop with a real-failure-only retry budget (`ml_runtime.rs:1143-1270`).
- Online model repair + status UI (`components/OcrModelRepairCard.jsx`, `settings/advanced/InferenceCards.jsx`).
- Divergence from the original plan, recorded as debt: there is **no `ocr_engine` config flag and no registry-level Python OCR fallback**; Python OCR recognition is disabled by a runtime handshake (`monitor/ocr_service.py:129`, `_rust_ocr_provider_active`). Dual-run diagnostics were never built.

**ONNX unification + Rust model asset registry (not in original plan) — DONE**
- `ModelInventoryEntry` reports id / purpose / installed / `active_runtime` (`onnx`|`pytorch`) / size / per-runtime variants; exposed via `get_model_inventory` (`model_management.rs:878-1063,1160`; UI `settings/organize/ModelInventoryTable.jsx`).
- This over-delivers the original 0.8.3 "ModelRegistry" but only covers assets — there is **no `rust-native` runtime** for anything but OCR.

**Rust ML contracts (original 0.8.3) — PARTIAL / different shape**
- `ml_contracts.rs` defines `OcrEngine`/`TextEmbedder`/`Reranker`/`VectorIndex`/`ModelRegistry`, but the four non-OCR traits are deliberate placeholders with no method surface and **no `impl` anywhere** (`ml_contracts.rs:1-4,25-39`). Real OCR bypasses the trait via the worker process + `ml_protocol` IPC.
- The "bounded job runner" exists only as an OCR-specific narrow implementation (watchdog + post-process retry). There is **no general named-queue runner and no idle gating** in Rust.

**Agent/MCP track — reached original 0.8.3** (see the parallel-track section).

## Non-Negotiable Direction

Unchanged except where the audit forces a correction.

- SQLite-owned screenshots, OCR, and metadata remain the source of truth. Embeddings and ANN indexes are derived caches: they may be persisted and migrated to avoid expensive recomputation, but they must remain versioned, diagnosable, and rebuildable from SQLite-owned inputs. ChromaDB is currently the operational vector store, not an owner of unrecoverable user data.
- Every cross-thread, IPC, model, and UI request path must have a deadline, cancellation behavior, and user-visible failure state.
- A Rust replacement is not complete merely because it can load the same ONNX file. Before default cutover it must pass the Python-oracle behavior contract for preprocessing/tokenization, pooling/normalization, response shape, filtering/pagination, ranking quality, lifecycle, and rollback.
- **Corrected flag rule:** prefer shipping a Rust replacement behind a config flag, then default, then remove the Python fallback a release later. Where a replacement ships default without a flag (as OCR did), it **must** still expose an observable fallback/degrade path and a diagnostic. Track any skipped flag as debt rather than pretending it exists.
- Heavy ML must use the existing idle policy or an explicit manual-run path. No background model load/inference should surprise a foreground app.
- **Migration diagnostics are development scaffolding, not product surface.** Shadow comparison toggles, parity probes, and their sample tables exist only to prove one cutover gate. Once that gate is proven and the capability is cut over, they must be removed from the default build before the capability ships in a production (non-beta) release. Shipping one to end users is a defect, not a harmless extra: it exposes a meaningless switch, keeps a second inference path alive on a user's machine, and invites bug reports about numbers no user can act on. Whatever must survive for the "observable fallback" rule is a minimal read-only status field on an existing status command, never a dev panel.
- Delete infrastructure only after the previous release ran without needing it by default.
- Model downloads must be explicit, pinned by expected files and hashes where practical, and described as local ML assets.
- Do not port a Python feature just because it exists. If it is low value, hard to explain, or expensive to maintain, demote or remove it.

## Target End State

By the end of the `0.8.x` line:

- Rust owns capture, OCR, OCR storage, keyword search, thumbnails, model registry, task/smart-cluster persistence, MCP, and app lifecycle. *(Already true today except for the ML inference and vector-store layers.)*
- Python is not installed by default.
- `monitor.pyz`, `requirements.txt`, bundled Python installer, pip sync UI, Python venv checks, Python named-pipe server, reverse storage IPC, and Python worker supervision are removed from the default build.
- Optional legacy/experimental ML features, if kept, are external add-ons rather than part of the core capture pipeline.

## Milestone Plan (Revised)

Milestones are ordered. Version hints are estimates. Each keeps the original Purpose / Recommended changes / Release gate shape and adds **Current reality** and **Depends on** so the plan stays honest.

### Milestone 1 — Vector Semantics & Internal Migration Baseline — MINI, FOLDED INTO M2 PREPARATION

Slimmed on 2026-07-19 after a direct source audit. Key correction to the original framing: ChromaDB holds **no unrecoverable data** — every vector, document, and metadata field is derived from SQLite-owned sources (images + OCR text + window metadata), so "make vector state safe" was already true by construction. What was actually missing was *verifiability* and *scope decisions*. The per-row status ledger and the rebuild executor originally planned here are deliberately **moved to Milestone 2**: a ledger is only trustworthy when the index writer maintains it first-party, and the writer becomes Rust in Milestone 2 — building a Python-driven ledger/rebuilder first would be throwaway scaffolding.

Audit facts this rests on (2026-07-19):

- Three collections, two key schemes: `screenshots` (CLIP image vectors, keyed `md5("memory://" + image_hash)`), `task_vectors` (MiniLM text vectors, keyed `str(screenshot_id)`), `task_centroids` (cold centroids).
- **Important scope of the Milestone 1 health check:** `actual_clip_image_rows` is only the live row count of the Python Chroma `screenshots` collection — normalized Chinese-CLIP **image embeddings** used by `search_nl` text→image search. It is not the Rust OCR/keyword index, not the MiniLM `task_vectors` collection, not the `task_centroids` collection, and not a check of vector quality or search ranking.
- `postprocess_status = 'completed'` never meant "vector exists": text-less screenshots skip vector indexing but still complete.
- `task_vectors` ingest is fire-and-forget with no status anywhere (model-missing / session-locked / queue-full drops are silent); screenshot deletion does not clean it — the 30-day hot-layer expiry is the only reaper.
- Restart marks unfinished postprocess `discarded` terminally, and the retry button only drains a ≤32-entry in-memory backlog. Both are **confirmed intentional design** (bounded failure, no unbounded retry/IO). Consequence: any rebuild must be explicit, budgeted, and idempotent — delivered in Milestone 2, never as automatic resurrection.

Internal implementation candidate in the current working tree:

- Expected CLIP-image baseline owned by Rust: `count_expected_clip_image_rows` = distinct `image_hash` among non-deleted screenshots with an active OCR row (`storage/screenshot.rs`). This deliberately keeps the same stable proxy for all pre/post migration comparisons; it does not add a temporary schema field that Milestone 2's ledger would immediately replace.
- Count-level CLIP diagnostic in `storage_get_index_health`: `clip_image_index {expected_eligible_images, actual_rows, missing_lower_bound, orphaned_lower_bound, assessment}`. It is retained only as internal JSON/log/copy-diagnostics input for M2 migration comparisons. The existing user UI is unchanged and does not display the gap or terminal postprocess counts.
- The purpose is coverage observability for one rebuildable derived cache: detect that CLIP visual search may have fewer eligible image rows than SQLite suggests, or may retain rows for deleted/non-eligible inputs. It does not prove that the vectors are numerically correct, that nearest-neighbor ranking is good, or that the other semantic/task indexes are healthy.
- `chromadb==1.5.1` pinned (was `>=0.4.0` while 1.5.1 was actually deployed; on-disk format is not stable across majors and the cache is keyed to this version until Milestone 2 replaces it).

Decisions recorded (binding for later milestones):

- **Chinese-CLIP text→image search is retained as a distinct surface — reversal of the earlier demotion.** `search_nl`'s live backend is `search_by_text` (CLIP text encoder → `screenshots` image-vector collection), reachable from two frontend surfaces (`AdvancedSearch.jsx`, `SearchBox.jsx`) and the MCP `search_nl` tool. It is cross-modal — a text query matching a screenshot's *visual* content — which neither the shipped Rust keyword search (`storage/search.rs::search_text`, a blind bigram index over OCR text) nor a future MiniLM-over-OCR semantic search can reproduce. The withdrawn "demote, no Rust port" decision evaluated the two genuinely dead functions (`search_by_image`, `search_by_ocr_text`) and missed the live one. Its valid parts survive as *scoping*, not deletion: CLIP indexing is gated on `ocr_text.strip()` (`worker_process.py:166`), so the retained capability is exactly "text→image over text-bearing screenshots"; and the expensive image re-encode is avoided by **keeping the already-populated collection** — deletion, not retention, is what would force the costly rebuild.
- **Three labeled search surfaces, not one collapsed NL path.** OCR keyword (Rust, shipped) · semantic text over OCR (MiniLM, → Rust in Milestone 2) · visual/NL image search (CLIP text→image, retained). Different modalities, non-comparable scores; the "two NL surfaces confuse users" problem is solved by clear labeling (Milestone 2 UI copy), not by folding text→image into text→OCR.
- **This retention makes the internal baseline coherent.** `count_expected_clip_image_rows` counts `DISTINCT image_hash` with an active OCR row (`storage/screenshot.rs`) versus the live `screenshots` count (`get_collection_stats`). Deleting that collection later would have voided the migration baseline.
- `task_vectors` (MiniLM, keyed `screenshot_id`) and `screenshots` (CLIP, keyed `image_hash`) are **independent collections serving different surfaces** — Milestone 2 re-homes each on its own terms; neither replaces the other. `task_vectors` deletion-orphan rows stay accepted debt (30-day self-expiry) until its Milestone 2 ledger.
- Discard-on-restart and memory-only retry stay exactly as designed; the Milestone 2 rebuild is the explicit, user-triggered complement that makes discarding lossless.

Internal preparation gate:

- Internal diagnostics can record the same estimated-vs-actual CLIP count before and after migration without changing the ordinary settings UI.
- The scope decisions above are written down here and reflected in the Milestone 2 plan.
- A storage-backed test proves multiple OCR rows and duplicate image hashes do not inflate the expected CLIP row count.
- Keep the unrelated `.github/workflows/release.yml` change out of the migration branch/PR.

Depends on: nothing. This preparation may land as the first `m2/contracts-and-baseline` PR rather than as a separately released milestone.

### Milestone 2 — Behavior-Equivalent Rust ONNX + Per-Kind Index Ownership (target: v0.8.4 Beta)

Purpose: complete "hop 2" without changing product capability. Introduce Rust ONNX inference and Rust-owned derived indexes, but cut over **one capability at a time** only after Python-oracle parity, dual-write, shadow query, data migration, foreground-isolation, and rollback gates pass. CLIP text→image, MiniLM semantic-text retrieval, reranking, classification, and unsupervised task clustering are separate consumers even when they share ONNX Runtime; shared runtime code does not justify a big-bang consumer switch.

v0.8.4 Beta delivery shape: the complete M2 implementation is developed in this version, but still lands incrementally. Infrastructure may merge while disabled; MiniLM, reranker, and CLIP each move through `python/chroma -> rust_shadow/dual -> rust with Python fallback`. A capability is part of the v0.8.4 Beta completion claim only after its own release gate passes. BGE classification and `task_centroids` remain explicitly outside the production cutover even though the shared Rust runtime may exercise BGE in shadow mode.

Current reality:

- `TextEmbedder`/`Reranker` are empty traits; `ml_protocol` only exposes OCR; `search_nl` still forwards to Python (`monitor.rs:600`).
- Python owns CLIP image/text inference and the `screenshots` collection; MiniLM and the `task_vectors` / `task_centroids` collections still support semantic retrieval and Milestone 4 task clustering; BGE still feeds Python classification; the reranker feeds both calibration and the Python Smart Cluster worker.
- Therefore "remove Python ONNX + Chroma in Milestone 2" would be a functional regression. Chroma remains available for the still-live task-clustering collections until Milestone 4, and Python BGE remains available until classification has a Rust consumer (Milestone 5) or an explicit Python→Rust inference bridge.
- Milestone 1's worktree implementation provides only count-level CLIP observability. Milestone 2 upgrades each migrated index kind to subject-level status.

#### M2.1 — Freeze the Python behavior contract

- Build a local, telemetry-free Python oracle/golden harness using non-sensitive fixture text/images. Record and test the exact contracts already shipped:
  - Chinese-CLIP: RGB conversion; direct square BICUBIC resize; `preprocessor_config.json` rescale/mean/std; tokenizer padding with no truncation; explicit text/image output selection; L2 normalization.
  - MiniLM: tokenizer max length 256; attention-mask mean pooling; L2 normalization; combined text format `process | title | OCR[:200]`.
  - BGE: tokenizer max length 512; CLS pooling; L2 normalization.
  - bge-reranker: pair tokenization; max length 512; raw logits, not sigmoid; variant-specific model file and output.
  - CLIP search: cosine distance, minimum similarity 0.32, current over-fetch/filter/pagination order, and the existing JSON response fields.
- Rust token IDs must match exactly. CPU vectors should match to a very tight cosine/absolute-error tolerance; DirectML may use a slightly wider numeric tolerance, but ranking and threshold decisions must remain within the release gate below.
- Treat known current bugs separately from migration parity. For example, changing NL time/category filtering behavior is an explicit bug fix with tests and release notes, not an accidental consequence of the backend switch.

#### M2.2 — Add a separate Rust semantic runtime

- Implement batch-capable Rust interfaces with real method surfaces: text embedding, image embedding, and reranking. Reuse the pinned model assets and tokenizer JSON files; make input/output tensor names and pooling/preprocessing explicit model descriptors rather than output-name heuristics.
- Extend the versioned ML protocol with bounded `embed_text`, `embed_image`, `rerank`, status, and unload operations, including maximum batch/token/body limits, deadlines, cancellation behavior, provider/model/version diagnostics, and stable error kinds.
- Keep OCR on a dedicated high-priority worker/process. Semantic inference must not serialize behind or hold memory inside the OCR critical worker. A separate semantic worker may share executable/runtime code, but not the OCR queue or failure domain.
- Idle-gate only background capture indexing, rebuild, and maintenance model loads. A user-initiated search/calibration request is foreground/manual work: it remains deadline-bound but must not silently fail merely because the system is not idle.
- Match the existing ONNX Runtime safety settings or justify/test any change in graph optimization, allocator, file-vs-buffer loading, thread counts, CPU fallback, and DirectML device selection.

#### M2.3 — Rust-owned derived embedding storage and ledger — DONE 2026-07-21

Implementation status: `m2/derived-vector-store` adds the first-party SQLite
`derived_embeddings` cache, the `(index_kind, subject_key)`
`derived_index_jobs` ledger, and generation metadata. Vector + completed-ledger
writes and vector + ledger deletion are transactional. Query-visible reads join
both tables and require exact model revision, embedding version, and source
fingerprint agreement, so pending, failed, discarded, invalidated, or partial
rows cannot leak into search. The generalized keys are `screenshot_id` strings
for `semantic_text` and `image_hash` for `clip_image`.

Workers claim queued subjects with random execution leases; completion, failure,
and discard are compare-and-set transitions against the active lease, and commits
also require the referenced screenshot or image hash to remain active. This
prevents late workers from resurrecting deleted/discarded work or hiding a
newer completion. Startup requeues any `processing` lease left behind by an
interrupted process so rebuilds remain resumable after crashes.

The branch also publishes immutable, checksummed `.cpdvec` generations through
temporary-file fsync and atomic rename. The initial payload is a flat exact-scan
snapshot; SQLite remains authoritative, and the payload can be replaced by an
ANN layout when the M2.5 query path needs it without changing persistence or
generation semantics. Runtime publication never deletes finalized sidecars;
unreferenced generations are cleaned during the next startup, before readers
are exposed. M2.4 owns the paged MiniLM migration and the temporary
Chroma/Rust index dual-write; M2.5 owns shadow queries, production query
cutover, and the later CLIP overlap.

- Prefer a two-layer derived design:
  1. a SQLite `derived_embeddings` cache holding the migrated/generated float32 vector plus `index_kind`, `subject_key`, dimensions, model id/revision, embedding version, source fingerprint, and timestamps;
  2. a generation-versioned ANN sidecar used only as a rebuildable acceleration layer, written via temporary file + fsync + atomic replace and validated by header/checksum.
  SQLite screenshots/OCR/metadata remain the source inputs. Persisting derived vectors avoids expensive CLIP re-encoding during migration and gives ledger/vector writes one transactional boundary; the ANN file is never authoritative.
- Use a generalized subject key rather than `(screenshot_id, index_kind)`:
  - `text_embedding` → `subject_key = screenshot_id`;
  - `image_embedding` → `subject_key = image_hash`.
- Ledger sketch: `derived_index_jobs(index_kind, subject_key, status, error_code, error, attempts, next_retry_at, model_id, model_revision, embedding_version, source_fingerprint, updated_at)`, PK `(index_kind, subject_key)`. `discarded` remains legal and visible; rebuild is explicit, never automatic resurrection.
- New vectors become query-visible only after the embedding row and completed ledger state commit. ANN generation lag must be safe: queries use the last complete generation plus a bounded exact-scan delta, or wait for an atomic generation swap.
- Deletion, model-version invalidation, duplicate image hashes, session lock/unlock, and interrupted writes need first-party tests.

#### M2.4 — Sentinel-triggered MiniLM migration — DONE 2026-07-24

The Chroma `task_vectors` collection is a ~30-day hot layer whose expired
vectors are compressed into `task_centroids`, so "every SQLite screenshot has
a MiniLM vector" is **not** a migration completion condition, and the Rust
coverage denominator for the later cutover gate is the set of valid, mappable
vectors in the Chroma snapshot — not all screenshots.

The mandatory copy is entirely sentinel-triggered. For each vector-space
revision, the absence of
`app_metadata.minilm_auto_migration_done_<revision>` starts one full-scope,
idempotent copy at startup. The worker waits for Windows Hello unlock before
reading encrypted OCR inputs. A crash, process exit, or transient worker
failure leaves the sentinel unset; the next launch/unlock resumes from the
durable cursor and snapshot. Legacy manual/time-bounded run records are never
resumed or allowed to settle the sentinel; the automatic mode starts a fresh
full copy instead.
Terminally quarantined invalid/orphan rows may finish as
`completed_with_errors` and still settle the sentinel; transient orchestration
failures remain unfinished and retry automatically.

The whole run executes under global maintenance mode: a non-dismissable
full-window overlay, `MAINTENANCE_IN_PROGRESS` rejection at MCP and reverse
IPC boundaries (with a small session/crypto/status/dual-write allowlist),
gated monitor start/stop/pause/resume commands, and a paused
retention/delete-queue loop. Capture is paused for the duration; the previous
monitor running/paused state is recorded and restored exactly. The migration
cannot be started from settings and cannot be cancelled; closing the app is
the only interruption.

The full hot-layer ID snapshot is built asynchronously by an internal Python
protocol (`start_task_vectors_export` → status polling → exact-id pages →
`finish_task_vectors_export`), persisted with an atomically renamed manifest
under `data/migrations/minilm/<export_id>/`, and bounded by a 10-minute logical
build deadline plus 24 h idle / 7 d hard / 1 h `.tmp` TTLs. It has no
user-facing time range or cancellation option.
Pages carry vectors as one little-endian float32 blob in Base64
(`embeddings_f32_le_b64`, ~256 KB per 128-row page) instead of tens of
thousands of JSON floats. Run state is durable in
`minilm_migration_runs` / `minilm_migration_subjects` /
`minilm_migration_run_errors`: each page commits ledger rows, embeddings,
counters, and the export cursor in one transaction, so a crash either retries
an uncommitted page or continues after it, and an interrupted run resumes by
re-attaching to the persisted snapshot (a snapshot Python can no longer
restore forces a cursor reset rather than a silently reordered page walk).
Rust accepts only canonical positive screenshot ids and finite, non-zero
384-dimensional vectors (zero vectors are quarantined, never imported), maps
them to active SQLite screenshots, and rehydrates process/title/category/OCR
from SQLite. A valid legacy vector whose current SQLite text is empty is still
copied but marked `legacy_chroma_unverified` instead of being recomputed.
Orphan Chroma ids, corrupt vectors, and rows that disappear after the snapshot
remain persisted diagnostics and yield `completed_with_errors`. After the
copy, a full-scope run deletes Rust `semantic_text` rows outside the snapshot
scope (`reconcile`), and Python's hot-layer expiry now mirrors deletions to
Rust through reverse IPC with a persisted retry queue, so the Rust collection
tracks — rather than outgrows — the Chroma hot layer. A disk preflight
(`estimated peak × 1.25 + 1 GiB`, with a 64 MiB transient allowance) rejects
the run before any vector write; generation publication runs on a blocking
worker with progress phases `publishing_sync` / `publishing_verify` /
`publishing_commit`. The migration never requests cancellation; the final
`sync_all` window is surfaced to the user as an uninterruptible safe write.

The MiniLM source contract remains exactly `process | title | OCR[:200]`; its
versioned fingerprint excludes category and is computed from the final
Rust-rehydrated model input. Legacy Chroma rows do not record their producing
runtime, so imported and newly generated vectors share the reviewed
compatibility contract `minilm-l12-vector-space-v1`. Python capture paths
best-effort dual-write their vectors to Rust through authenticated reverse IPC;
Rust ignores Python metadata and recomputes the fingerprint. The migration
never runs inference to manufacture a missing vector.
Chroma/Python remains the authoritative query backend in this phase: there is
no shadow-query or production-query switch in M2.4.

`task_centroids` and all Chroma operations needed by Python HDBSCAN/PaCMAP stay
unchanged until Milestone 4. The separate CLIP `screenshots` float-copy
migration remains step 7 of M2.5, immediately before CLIP new-capture
dual-write and shadow query. `chromadb` therefore remains a default dependency.

#### M2.5 — Dual-write, shadow-query, then cut over by capability

Recommended sequence:

1. MiniLM Rust inference parity.
2. MiniLM derived-cache/index dual-write and migration. **Done in M2.4.**
3. Rust semantic shadow queries compared locally with Chroma; Python remains authoritative. **Harness done 2026-07-27** — passive per-query shadow, an active built-in query corpus, and a document-encoder re-encode probe, reported through the telemetry-free `get_semantic_shadow_report`. The whole surface is temporary by construction; see the retirement block below.
4. Cut over the **non-reranked** semantic-text retrieval path to `semantic_index = rust`; keep rollback. Calibration is deliberately *not* included — see the scope note below.
5. Rust capture-side MiniLM indexing and retention ownership: new screenshots are encoded and enqueued by Rust, and hot-layer expiry is decided by Rust rather than mirrored from Python. Without this, `semantic_index = rust` means "Rust reads, Python writes".
6. Reranker parity and shadow scoring; then cut over Smart Cluster calibration, which moves as one unit with the reranker. The Python Smart Cluster worker may temporarily call the Rust reranker, but its Python fallback remains until Milestone 3.
7. CLIP vector export/migration.
8. Rust CLIP image encoder dual-write for new captures. **Do not cut over visual search while new screenshots still depend solely on Python image encoding.**
9. Rust CLIP text-query shadow mode, then cut over `search_nl` and MCP capability reporting.
10. Implement BGE in the shared Rust runtime and run it in shadow mode, but do not remove Python BGE inference until classification itself has a Rust path or a deliberately supported Rust inference bridge. The classification consumer remains a Milestone 5 completion item.

Step-4 scope: retrieval only, not calibration (recorded 2026-07-28). The earlier
plan said step 4 cuts over "semantic-text retrieval and Smart Cluster
calibration prefilter". In the shipped code those are not separable at this
step. Calibration mode forces `enableRerank = true` because it needs
`rerank_score` to derive a per-cluster threshold (`NlClusterView.jsx:94-95`),
and Python's `query_by_text` performs retrieval and reranking in one call
(`task_clustering.py:1657-1701`) with no standalone rerank command on the
protocol. Cutting over the calibration prefilter alone would therefore mean
adding a Rust-retrieve → Python-rerank bridge that exists for exactly one
release and is deleted at step 6 — more new surface than it saves, on the one
path where a ranking mistake silently corrupts a saved threshold. So step 4
cuts over `enable_rerank = false` only; `enable_rerank = true` continues to run
entirely on Python and moves at step 6, where the end-to-end reranked ordering
is re-measured anyway.

Step-4 defaults and rollback levers. The cutover flips the *defaults* of both
enums to `rust`, not merely the set of legal values — a "cutover" that left
`semantic_index` defaulting to `chroma` would ship a switch nobody flips. This
is safe on a machine whose M2.4 migration has not run, because an empty Rust
index is treated as a fallback condition and the query is served from Python.
Two independent levers restore the previous behavior for the one required
release: `semantic_index = chroma` (Python owns retrieval) and
`semantic_runtime = python` (no Rust MiniLM inference). The second is honored as
a refusal rather than silently overridden, because serving from the Rust store
necessarily encodes the query with the Rust runtime. Every refusal is recorded
with its reason and read back through `get_ml_semantic_status.backend`, which
the Settings → Advanced "semantic retrieval backend" card renders together with
the `semantic_index` switch, so both the diagnostic and the rollback are
reachable without a registry editor.

Step-4 coverage rule (recorded 2026-07-28, extended 2026-07-29 after review).
An empty Rust index falls back, but a *partially* filled one would not, and that
is the failure this step actually has to defend against: ranking an incomplete
corpus returns a plausible page with screenshots silently missing from it, which
no user can detect and no after-the-fact instrument can reconstruct. Three
things can leave the index short, and each has its own refusal.

*The migration may not have finished.* The M2.4 copy commits page by page
against a persisted cursor, and a migrated row becomes query-visible the moment
its job row reaches `completed` — there is no generation gate in front of the
read path. A run that fails mid-session drops the maintenance guard and does not
retry until the next launch, so the rest of that session would otherwise serve a
prefix of the corpus. The once-per-revision sentinel therefore gates retrieval
too (`last_fallback_reason = migration_incomplete`), cached in-process because it
is a one-way transition.

*A mirror may have been lost.* The Python dual-write is durable — a mirror that
fails is queued to `migrations/minilm/rust-import-retry.json` and retried from
Chroma on the next capture or clustering pass, the same treatment hot-layer
deletions already had. The size of that queue travels with every dual-write, so
the Rust read path knows when its copy is behind and refuses to serve until the
backlog is paid (`last_fallback_reason = index_incomplete`). Because the Rust
counter is process-global and starts at zero, Python also reports the queue it
loads off disk at monitor startup, and again after every retry pass including
the ones that wrote nothing — otherwise a backlog that survived an app restart
would be invisible to Rust until the next capture happened to write, which with
the monitor paused is never.

*The debt must stay payable.* Only mirrors that can still arrive count. Chroma
keeps documents for screenshots the user deleted until they age out (Python
prunes the hot layer on age alone), so a queued id can become one Rust rejects
every time it is offered. Rust therefore classifies each rejected row as
permanent or transient, Python drops the permanent ones instead of queueing
them, and a retry pass no longer stops at the first failing batch. Without this
a single deleted screenshot would hold the debt above zero — and Rust retrieval
switched off — for as long as the queue survived.

This is what replaces the deleted shadow harness's `only_in_chroma` metric as a
*runtime* guard; the release gate below still expects the offline measurement
before the step is called done. One residual window remains, recorded honestly:
while the reverse-IPC pipe itself is down, Python cannot report the debt it is
accumulating, so the Rust index can be behind without knowing it until the pipe
recovers. Step 5 removes the reporting problem altogether by making Rust the
writer.

Step-5 scope: why an extra step exists (recorded 2026-07-28). The numbered
sequence had no Rust capture-side MiniLM indexing step, even though the M2.5
release gate requires that "with Python stopped … new-capture Rust CLIP/MiniLM
indexing continue to work". CLIP got such a step (now step 8); MiniLM was
skipped. In the shipped code the only writers of `semantic_text` rows are the
M2.4 migration and the Python dual-write over reverse IPC
(`reverse_ipc.rs`), and the only reaper is Python's hot-layer expiry mirroring
its deletions to Rust. Rust can already embed (`semantic_runtime::embed_text`),
but nothing calls it for a new screenshot. So step 4 delivers `semantic_index =
rust` in the honest sense of "Rust owns the read path", not "Rust owns the
index". Step 5 closes that: an idle-gated Rust enqueue/drain worker on the
capture path plus Rust-owned retention, after which the "Python stopped" gate
can genuinely be evaluated. Until then the capability is advertised as
Rust-read / Python-written, per the honest-advertisement rule.

Step-3 measurement (2026-07-27, 256-query and 256-document samples): query-encoder
maximum absolute error 6.0e-7; Overlap@10 p50 100%, p05 50%; top-1 agreement
90.7%; document re-encode cosine p50 0.9917 / p05 0.9868 / min 0.9796. Retrieval
divergence is dominated by Rust returning documents the Chroma ANN path does not
surface — the Rust side is an exact scan over the same hot layer, so a strict
superset of recall is expected and was accepted as a difference in retrieval
method rather than a Rust defect. Recorded honestly: this clears the query-encoder
numeric gate but not the *original literal* "top-10 overlap ≥ 99% / top-1
effectively unchanged" wording, which assumed two implementations of the same
retrieval method; that wording is superseded below rather than waived. The
step-4 cutover therefore keeps the `python` fallback and should re-check ranking
on the reranked end-to-end path (step 6), where the bi-encoder recall difference
is re-scored anyway.

- Use enum backends, not ambiguous booleans: `semantic_runtime = python|rust_shadow|rust`, `semantic_index = chroma|dual|rust`, `clip_runtime = python|rust_shadow|rust`, `clip_index = chroma|dual|rust`. Invalid or unavailable Rust configurations fall back observably for one release, with a local diagnostic explaining why.
- Preserve and explicitly test search response schemas, filters, offsets, limits, thresholds, MCP tool availability, and frontend labels. OCR keyword, MiniLM semantic text, CLIP visual/NL image search, and Smart Cluster assignment remain separately labeled; their scores are not compared across models.

##### Shadow-scaffolding retirement — release-blocking, not optional cleanup

Every `rust_shadow` harness in this milestone is a development instrument with a
scheduled death. It proves one gate, and then it goes. A production (non-beta)
release must not contain the shadow toggle or its probes for a capability that
has already cut over. Enforce this per capability, at the cutover PR, rather
than as an end-of-milestone sweep — an unowned diagnostic panel never gets
deleted later.

MiniLM instance of the rule. Shadow-only, delete with (or immediately after)
the step-4 cutover:

- `semantic_shadow.rs` — `spawn_shadow_sample`, `run_semantic_shadow_probe`, `run_semantic_doc_encoder_probe`, the built-in probe corpus, and the single-flight guards; the `spawn_shadow_sample` call site in `monitor.rs::monitor_nl_cluster_query`; the three Tauri commands registered in `lib.rs`.
- `storage/semantic_shadow.rs` and the `semantic_shadow_samples` / `semantic_doc_encoder_runs` tables plus their in-place column upgrades in `storage/schema.rs`. Dropping these tables loses no user data — they hold query hashes and parity/latency numbers only, never query text or screenshot content.
- `SemanticShadowCard` in `settings/advanced/InferenceCards.jsx`, its wiring in `AdvancedSection.jsx`, and the report/probe state in `useAdvancedSectionController.js`.
- `minilm_migration::minilm_sources_for_ids`, whose only caller is the document-encoder probe.
- The `rust_shadow` value of `semantic_runtime` once nothing can enter that mode.

Explicitly **not** shadow scaffolding — this code is the production read path and stays:

- `derived_index::semantic_text_topk`, `ScoredSubject`, `count_query_visible_embeddings`.
- `storage/semantic_cache.rs` (the resident vector matrix), its `StorageState` fields, the idle-eviction ticker in `lib.rs`, and the cache resets in `storage/schema.rs`.
- The `semantic_runtime` / `semantic_index` enums themselves, minus the retired value: they remain the observable rollback switch required above.

What may survive the cutover is the *fallback* diagnostic the enum rule demands
— which backend is active and why a Rust configuration was refused — and it
survives as a field on an existing status command, not as a settings card with
percentiles in it. Apply the same shape to the reranker, CLIP, and BGE shadows
when their turn comes.

Release gate:

- Token IDs match exactly for the golden corpus. CPU embedding cosine is at least 0.99999 with maximum absolute error 0.0001; DirectML embedding cosine is at least 0.999 with maximum absolute error 0.001 unless a model-specific, reviewed tolerance is documented. Raw reranker logits use the same CPU/DirectML absolute-error profiles.
- **Retrieval equivalence, stated as a recall-superset gate (rewritten 2026-07-28).** The original wording — "top-10 overlap at least 99%, top-1 effectively unchanged" — assumed the Rust and Python paths were two implementations of the *same* retrieval method. They are not: Chroma is approximate (HNSW), the Rust path is an exact cosine scan over the same hot layer. An exact scan that agreed with an ANN index 99% of the time at k=10 would be evidence that one of them is wrong, not that both are right. The gate is therefore:
  - **Recall superset.** For each query, every Python top-K result that is present in the Rust store must be reachable by the Rust scan at a score no worse than Python assigns it. A Python-top result that is *absent* from the Rust store is a real coverage defect and blocks the cutover; a Python-top result that is present but ranked differently is the expected ANN-vs-exact difference. The shipped shadow harness reports these separately as `only_in_chroma` (blocking) and `in_both_diff_rank` (accepted) precisely so the two cannot be confused. Since the harness is deleted at step 4, this remains an offline gate measured before the cutover lands; what enforces it afterwards is the runtime coverage rule above (durable dual-write plus a debt-gated read path), not a metric nobody can read anymore.
  - **Query-encoder numerics.** Cosine agreement between the Rust query encoder and the Python one, measured against the same stored document vectors, must meet the CPU tolerance above. This is the part that would catch a genuine Rust inference bug, and it is the part the 2026-07-27 measurement passed at 6.0e-7 maximum absolute error.
  - **Contracts.** Filter, offset, limit, threshold, and JSON response contracts still match 100%. This is unchanged and is not softened by the above.
  - Overlap@10 and top-1 agreement remain **recorded** as descriptive numbers, not pass/fail thresholds, for the bi-encoder retrieval step. They return to being a pass/fail gate at step 6, where reranked end-to-end ordering — which is what the user actually sees on the calibration path — is compared.
- Migrated subject-key sets match exactly per index kind; unmappable/corrupt rows are listable and keep the legacy backend active. Existing CLIP vectors are byte-copied/float-copied rather than re-encoded during normal migration.
- With Python stopped, Rust-owned semantic-text search, migrated CLIP visual search, and new-capture Rust CLIP/MiniLM indexing continue to work for capabilities marked `rust`. Capabilities still marked Python-backed remain advertised honestly and usable through fallback. **MiniLM reaches this gate at step 5, not step 4** — see the step-5 scope note. Step 4 may ship with the capability advertised as Rust-read / Python-written.

- Existing automatic classification and task clustering do not regress: Python BGE remains until its consumer migrates, and `task_vectors` / `task_centroids` remain available to the Milestone 4 path.
- Embedding migration/rebuild is interruptible/resumable; session lock/unlock, process crash, partial ANN generation, model upgrade, rollback to the previous release, and deletion are tested without screenshot loss or silent vector loss.
- Background semantic work cannot trigger a model load during fullscreen/game activity. User-initiated search remains available with a deadline. OCR p95 latency and reliability show no material regression from semantic work; search p95 latency and peak memory stay within an explicitly recorded budget.
- The Python fallback remains available for one released version after each capability becomes Rust-default.
- **No shadow/probe development surface for an already-cut-over capability ships in a production build.** The retirement list above is executed, not deferred: the settings card, the probe commands, and the sample tables are gone, and the only remaining backend diagnostic is the read-only fallback status the enum rule requires. A beta may ship the harness; a production release may not.

Depends on: Milestone 1 semantics/decisions landed and reviewed, including the approximate-count caveats above.

### Milestone 3 — Smart Cluster Worker in Rust (target: post-v0.8.4 Beta)

Purpose: move Smart Cluster scoring entirely into Rust. Note Smart Cluster (user-controllable, NL-anchored, already Rust-persisted in `storage/smart_cluster.rs`) is a **different system** from unsupervised task clustering — do not conflate them.

Current reality: persistence and schema are Rust; the scoring worker `monitor/smart_cluster_worker.py` and `monitor/reranker.py` are still Python (`monitor_smart_cluster_worker_status` forwards to Python).

Recommended changes:

- Move pending-queue drain into Rust using the Milestone 2 reranker.
- Preserve the current good behavior: idle gate before load, idle re-check during batches, manual force-run, reranker unload after idle, per-cluster threshold assignment.
- Remove `monitor/smart_cluster_worker.py` from the default runtime path; keep the SQLite schema unless a migration is clearly needed.
- Add cheap assignment explainability if practical: prefilter score, rerank score, threshold, model id/version.

Release gate:

- Creation, calibration preview, pending drain, assignment, rescan, and summary storage all work without Python.
- Old pending entries are processed or left retryable, never silently dropped.

Depends on: Milestone 2 (Rust reranker).

### Milestone 4 — Task Clustering Decision (target: post-Milestone 3)

Purpose: decide whether HDBSCAN/PaCMAP task clustering deserves to survive Python removal. Default stance: it does not.

Current reality: `monitor/task_clustering.py` (PaCMAP + sklearn HDBSCAN) with a periodic auto-scheduler is fully Python; the scheduler is idle-gated but there is no Rust replacement.

Recommended direction:

- Do not port HDBSCAN/PaCMAP unless user value is proven. Prefer simpler Rust-owned grouping: session windows, process/title/URL continuity, Rust embedding similarity, Smart Cluster assignments, user corrections.
- If unsupervised clustering is still wanted, make it manual/idle-only, cancellable, rebuildable, and off the capture/OCR hot path.
- Remove or hide the periodic automatic Python HDBSCAN scheduler.
- Keep existing saved tasks in SQLite with migration/compat display.

Release gate:

- No dependency on Python HDBSCAN/PaCMAP for capture, OCR, search, or Smart Cluster.
- Task view stays useful; any expensive clustering run is explicitly idle-gated or manual.

Depends on: Milestone 2 (embedding similarity), if the simpler grouping uses it.

### Milestone 5 — Classification & PII Resolution (target: post-Milestone 4)

Purpose: remove the remaining Python-only ML features or make them optional add-ons.

Current reality: classification `monitor/classifier.py` (BGE, ONNX + torch fallback) runs in OCR post-process (`monitor/monitor/worker_process.py`). PII is a **two-tier MCP-output filter**: Rust aho-corasick dictionary masking (tier 1, `sensitive_filter.rs:264`) + Python Presidio NER (tier 2, default-on `presidio_enabled:true`, `mcp_server.rs:503-558`). The Rust rule layer is already the first line, but only on the MCP read path — capture-time PII is untouched. `torch`/`spacy`/`presidio-*` remain in `requirements.txt`.

Recommended changes:

- Replace Python BGE classification with Rust embedding-based scoring (Milestone 2 engine), simple process/title rules, user-defined Smart Clusters, or remove automatic classification from the default experience.
- PII: keep and extend the Rust deterministic rules; decide Presidio/spaCy's fate — add ONNX NER only for a concrete workflow, otherwise make advanced PII optional and not part of the default install. Clarify whether PII also applies at capture-write time, not only MCP read.
- Remove `torch`, `sentence-transformers`, `hdbscan`, `pacmap`, `spacy`, `presidio-*` from default dependencies once no default feature needs them.
- Audit UI for now-backendless controls; demote experimental panels; simplify wizards.

Release gate:

- Default install needs no Python packages for classification, PII, OCR, semantic search, or Smart Cluster.
- Advanced toggles reflect what is installed; no UI path starts Python implicitly.

Depends on: Milestones 2-4.

### Milestone 6 — Python-Free Default Build (target: later 0.8.x)

Purpose: ship the first default build that does not install or start Python.

Current reality: unchanged from the original plan — all of it is still present. `python.rs` provides `request_install_python` / `install_python_venv` / `install_spacy_model` / dep sync; `build.rs` packages and integrity-checks `monitor.pyz`; the release still bundles the Python installer.

Recommended changes:

- Remove Python auto-install from first-run.
- Stop bundling/copying `python-3.12.10-amd64.exe`, `monitor.pyz`, `requirements.txt`, venv freshness checks, pip sync UI, spaCy install UI.
- Keep a temporary `python_legacy_monitor` build flag only if needed, off by default, out of release packaging.
- Update docs/README: core is Rust-native; downloads are ONNX assets; no Python required. (Also fix stale `CLAUDE.md` OCR wording.)
- Upgrade cleanup: detect old venv, offer deletion after successful Rust-native operation, never delete user data.

Release gate:

- Fresh install and upgrade both work without Python; legacy Python files are not required for capture, OCR, search, Smart Cluster, settings, MCP, or extension capture.

Depends on: Milestones 1-5 all default and stable for at least one release.

### Milestone 7 — Infrastructure Deletion & Product Simplification (target: final 0.8.x cleanup)

Purpose: delete the now-unused integration layers and simplify the product surface.

Recommended changes:

- Remove default-build code for the Python launcher, installer/venv manager, monitor named-pipe server, reverse storage IPC, Python worker supervisor, and Python ChromaDB ownership.
- Remove stale docs/naming: no "Python service handles capture/OCR"; no "demo" labels on production Smart Cluster paths; no obsolete PaddleOCR naming (the runtime is RapidOCR/ONNX).
- Collapse setup into: app auth/storage, model assets, optional browser extension. Simplify settings into General / Privacy & Security / Search & Models / Storage / Extension / Advanced. Keep advanced diagnostics but off the main path.

Release gate:

- Removing Python files removes no user data; tests and packaging no longer reference Python monitor files in default mode.
- The product reads as one Rust-native local memory app, not a stack of optional subsystems.

Depends on: Milestone 6.

## Parallel Track: AI Agent Skill And MCP Onboarding

Status: reached the original 0.8.3 target. The standalone package `carbonpaper-memory` (repo `carbonPaperSkill`) is now the committed distribution shape (`components/settings/agent-access/agentAccessConstants.js:1-2`).

**Done:**
- Full Agent-setup area: endpoint + connection state, one-click copy of the setup prompt, separate token copy, "copy diagnostics" (`components/settings/agent-access/`).
- `mcp_get_status` returns `server_version`, `skill.tool_schema_version`, and `capabilities` including `search_nl` availability and the `python_monitor_not_running` disabled reason (`commands/mcp.rs:239-308`).
- 12 MCP tools exposed; `search_nl` is dropped from the tool list when its backend is unavailable (`mcp_server.rs:435`) — capability awareness already works.

**Remaining:**
- Original 0.8.4 items are the gap: a **settings-page MCP smoke test** (authenticated ping, list tools, harmless metadata query, separate auth/port/privacy-filter failure reporting) and per-Agent guided setup variants.
- Capability-drift control: when Milestone 2/3 move embedding/reranker/Smart Cluster to Rust, update the Skill's capability flags and the `search_nl` wording to track its backend (CLIP text→image, moving Python→Rust in Milestone 2) so it never advertises a Python-only path as stable — while keeping it advertised, since the capability is retained, not removed. Prefer generating/validating the Skill's tool table from the Rust MCP command definitions.

Do not defer this track to Milestones 6-7. The Agent story is already stable; those milestones should only remove obsolete Python wording.

## Feature-Specific Migration Notes

### OCR — DONE

Shipped via `rapidocr-core` (pinned crate, not the originally planned local `rapidocr-rs` path), as a standalone worker with thin CarbonPaper integration (RGB bytes in, blocks + timings out). Retain the original gate list as regression fixtures: empty/black, mixed CN/EN browser, code/editor, dense document, tiny edge text, transparent/alpha, EXIF-oriented, and fullscreen/game (no foreground stutter).

### Semantic Search

Keep OCR keyword search as the dependable baseline (Rust `search_text`, shipped). Make semantic text search Rust-owned and rebuildable in Milestone 2. There are **three distinct surfaces, kept separate and clearly labeled**: OCR keyword, semantic text (MiniLM over OCR), and visual/NL image search (Chinese-CLIP text→image). Chinese-CLIP is **retained (2026-07-19 reversal of the earlier demotion)** — it is `search_nl`'s live backend and the only text→image path. Milestone 2 migrates its existing vectors, dual-writes new image embeddings, and cuts over text queries only after parity; it never creates a window where visual search works for old data but new captures stop being indexed. "Prefer one path" applies *within* the text modality (do not ship several redundant text-NL surfaces), not to folding text→image into text→OCR.

### Smart Cluster

Preserve the current product model; it is more user-controllable than unsupervised task clustering. Rust already owns persistence; Milestone 3 moves the queue/prefilter/reranker/assignment/idle-gate. Add explanation fields before adding more algorithms.

### Task Clustering

Treat HDBSCAN/PaCMAP as an experiment that may not survive Python removal (Milestone 4). Prefer simpler grouping; if kept, manual/idle-only and derived from SQLite/embedding data, never a capture dependency.

### PII / Presidio

The Rust deterministic rule layer already exists and runs first on the MCP path. Extend it; add ONNX NER only for a concrete workflow; avoid shipping spaCy transformer models by default; decide whether PII also belongs at capture-write time (Milestone 5).

## Deletion Checklist

Do not delete a Python component until its Rust replacement has shipped as default for at least one release.

- Python OCR recognition — **eligible now**: Rust OCR has been default; remove the dormant Python OCR engine and its runtime handshake once Milestone 1 confirms nothing else depends on it. (The rest of `ocr_service.py` still does post-process — remove only the recognition path.)
- Python Chinese-CLIP inference — **not a demotion; retained** (2026-07-19 reversal). Remove its Python inference only after both Rust CLIP encoders are default for one release, existing vectors are migrated, new captures are Rust-indexed, and `search_nl` parity/rollback gates pass. The `screenshots` collection is migrated, never silently dropped or routinely re-encoded.
- Python MiniLM inference + Rust-replaced Chroma semantic retrieval — only after the relevant Milestone 2 capability is stable for one release. Keep Chroma and any dual-write needed by `task_vectors` / `task_centroids` until the Milestone 4 task-clustering decision is complete.
- Python bge-reranker inference — remove from calibration only after Milestone 2 parity/cutover, and remove from the default Python Smart Cluster path only after Milestone 3.
- Python BGE classification inference — do not remove in Milestone 2 merely because the Rust runtime can load BGE. Remove only when Milestone 5 classification uses Rust directly, or while a deliberately supported Python→Rust inference bridge is active and tested.
- Python Smart Cluster worker — only after Milestone 3 is stable.
- Python HDBSCAN/PaCMAP — only after Milestone 4 is stable.
- Python classification/PII dependencies — only after Milestone 5 is stable.
- Python installer/venv/pyz/reverse IPC — only after Milestone 6 proves fresh install and upgrade without Python.

Migration scaffolding runs on the opposite clock. It is not a Python component
waiting for a replacement; it is temporary instrumentation that must be removed
as soon as the gate it proves is signed off, and at the latest before the
capability's first production release:

- MiniLM shadow toggle, query/document probes, `semantic_shadow_samples` / `semantic_doc_encoder_runs`, and the settings card — delete with the M2.5 step-4 cutover. Full artifact list in the retirement block under M2.5.
- Reranker, CLIP, and BGE shadow harnesses — same rule at their own cutovers.
- The `rust_shadow` enum values — delete once no capability can still enter shadow mode.

## Branching And Release Discipline

- Keep this roadmap inside the `carbonPaper` repository (for example `docs/python-removal-roadmap.md`) before treating milestone status as branch/PR truth. The current parent-directory `roadmap.md` is outside the repository and cannot be reviewed or versioned with implementation branches.
- `m2/contracts-and-baseline` / PR #139 contains the Mini Milestone 1 collection semantics/tests, internal CLIP count diagnostics, and Chroma version pin. Keep the Python oracle in the next stacked PR so the already-small baseline PR remains reviewable. Do not include unrelated release-workflow edits or user-facing warnings.
- Build the v0.8.4 Beta Milestone 2 as short, stacked branches/PRs rather than a big-bang branch: `m2/contracts-and-baseline`, `m2/python-oracle-contracts`, `m2/rust-onnx-runtime`, `m2/derived-vector-store`, `m2/minilm-dual-write-migration`, `m2/minilm-shadow-cutover`, `m2/minilm-query-cutover`, `m2/minilm-capture-indexing`, `m2/reranker-shadow-cutover`, `m2/clip-vector-migration`, `m2/clip-cutover`, `m2/search-ui-capabilities`, and `m2/bge-shadow`. Infrastructure may merge to `main` while disabled; each consumer cutover has its own gate and rollback. (`m2/minilm-query-cutover` and `m2/minilm-capture-indexing` were added on 2026-07-28 for M2.5 steps 4 and 5; calibration is no longer part of the query-cutover PR and rides with `m2/reranker-shadow-cutover`.)
- Do not open a release branch until the intended capabilities have passed shadow mode on `main`. Release branches are stabilization-only; avoid accumulating new migration architecture there.
- Intended backend selection is explicit per capability. Prefer enums (`python|rust_shadow|rust`, `chroma|dual|rust`) over one Boolean per replacement because inference and index ownership cut over at different times. **Reality:** only `rust_ocr_dml_beta` (a DirectML accelerator toggle) exists; OCR shipped default without a `rust_ocr` flag. Milestones 2-5 must not repeat that unobservable big-bang cutover.
- Each flag/replacement should have a telemetry-free local diagnostic command that reports status and last error (the OCR runtime and `mcp_get_status` are the model to follow).
- Each release should include a rollback path for one version.
- Add a short release-note section: what moved from Python to Rust, what remains Python-backed, what data can be rebuilt, whether a model re-download is required.

## Immediate Next Step

M2.5 step 3 is done: the shadow harness (passive per-query comparison, active
query corpus, document re-encode probe) landed on `m2/minilm-shadow-cutover`
together with the resident-vector read path, and its parity/latency numbers are
recorded above. In progress is step 4 on `m2/minilm-query-cutover` — route the
**non-reranked** NL semantic-text query through `semantic_index = rust`, keeping
the `python` enum value as the one-release observable rollback. Reranked queries
(which is every calibration query) stay on Python until step 6; see the step-4
scope note for why splitting them there is cheaper than a one-release rerank
bridge. That same PR must delete the shadow scaffolding per the retirement
block; the harness has now served its purpose and must not reach a production
build. The coverage denominator for the gate is the set of valid, mappable
vectors in the Chroma hot-layer snapshot, not all SQLite screenshots.

Step 5 (`m2/minilm-capture-indexing`) follows immediately and is not optional
bookkeeping: until Rust encodes and enqueues new captures and owns retention,
`semantic_index = rust` means Rust reads an index Python still writes, and the
"with Python stopped" release gate cannot be evaluated for MiniLM at all. Only
after that does the reranker step (6) run, which is also where Smart Cluster
calibration moves. The separate CLIP vector migration/cutover sequence begins
after those.

No Rust backend becomes authoritative until its Python behavior contract, dual-write/migration, shadow-query, lifecycle, performance, and rollback gates pass. The first recommended cutover is MiniLM semantic retrieval; CLIP follows only after both legacy-vector migration and new-capture Rust image indexing work. Chroma remains for Milestone 4 task clustering, and Python BGE remains for Milestone 5 classification. Until subject-level Rust ledgers exist, the internal count-level CLIP comparison is only a migration baseline, not a product health judgment.
