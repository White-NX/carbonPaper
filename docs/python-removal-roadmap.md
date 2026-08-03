# Python Removal Roadmap (CarbonPaper 0.8.x)

**Goal.** Remove the Python monitor stack from CarbonPaper without losing user
data, without breaking OCR or search, and without letting background machine
learning interfere with whatever the user has in the foreground.

**Current milestone.** Milestone 2, targeted at v0.8.4 Beta. Steps 1-5 of the
M2.5 cutover sequence are merged; step 6 is implemented on
`m2/reranker-shadow-cutover` and waiting on an on-machine soak and two measured
gate numbers.

---

## 1. How to read this document

The document is organized by **milestone**, not by version. Milestones are
ordered commitments; version numbers next to them are estimates. Only Milestone
2 is tied to a specific release (v0.8.4 Beta). Do not infer a milestone's status
from the app's version number — section 3 explains why the two drifted apart.

Each milestone has the same five parts:

| Part | Answers |
| --- | --- |
| **Goal** | Why this milestone exists |
| **Where the code is** | What is already true, in the shipped or in-flight tree |
| **Work** | What has to be built, and what is already built |
| **Release gate** | What must be provably true before the milestone is called done |
| **Depends on** | Which earlier milestone has to land first |

Status words are used strictly:

- **DONE** — merged and gate-checked.
- **IN PROGRESS** — partially merged; the milestone body says which parts.
- **IMPLEMENTED, SOAKING** — code is written and on a branch, awaiting an
  on-machine run before it counts as done.
- **PLANNED** — not started.

Code references (`file.rs:123`) are pointers, not contracts. Line numbers drift
with every commit; the file and symbol names are the durable part. Where a
reference matters for a decision, the symbol name is given so it can be found
again.

---

## 2. Status at a glance

| Milestone | Target | Status | What it is waiting on |
| --- | --- | --- | --- |
| M1 — Vector semantics and migration baseline | folded into M2 | **DONE** | — |
| M2 — Rust ONNX inference and per-kind index ownership | v0.8.4 Beta | **IN PROGRESS** — 5 of 10 cutover steps merged, step 6 implemented and soaking | step 6's measured latency and reranked-ordering numbers, then CLIP (steps 7-9), then BGE shadow (step 10) |
| M3 — Smart Cluster worker in Rust | post-v0.8.4 Beta | **PLANNED** (scope reduced) | M2 step 6 |
| M4 — Task clustering decision | post-M3 | **PLANNED** | M2 (embedding similarity) |
| M5 — Classification and PII resolution | post-M4 | **PLANNED** | M2-M4 |
| M6 — Python-free default build | later 0.8.x | **PLANNED** | M1-M5 default and stable for a release |
| M7 — Infrastructure deletion and simplification | final 0.8.x | **PLANNED** | M6 |
| Parallel track — Agent skill and MCP | ongoing | **MOSTLY DONE** | settings-page MCP smoke test; per-agent setup variants |

---

## 3. Where the code actually is

### The migration is two hops, not one

The original plan assumed a direct `Python (PyTorch) -> Rust` port, one feature
per release. What the team actually built is two hops:

**Hop 1 — unify everything on ONNX, move model assets to Rust. Done.** All five
models (OCR PP-OCRv5, Chinese-CLIP, BGE, MiniLM, bge-reranker) are ONNX.
`use_onnx` defaults to `true` (`commands/utility.rs:146`, `monitor.rs:1440`), and
when the `CARBONPAPER_USE_ONNX` sentinel is set the Python entry point skips the
`torch` import entirely (`monitor/main.py:6-20`), which saves roughly 250-400 MB
of DLL working set. Model registration, install status, sizing, and OCR
*inference* are Rust-owned. Python is compressed to a thin layer:
`onnxruntime-directml` inference, ChromaDB, and post-process orchestration.

**Hop 2 — move ONNX inference from Python into Rust. OCR and MiniLM retrieval
done.** Everything else still runs its ONNX session inside the Python
`onnxruntime`.

This is why Milestone 2 is organized around a shared runtime rather than one
feature per release: embedding, reranking, and classification already sit on one
Python ONNX runtime, so they are cheaper to move by swapping the inference layer
once than by porting each consumer separately.

### What Rust owns today

Capture, OCR, OCR storage, keyword search, thumbnails, the model asset registry,
task and Smart Cluster persistence, MCP, app lifecycle — and, as of Milestone 2,
MiniLM semantic-text inference, the derived embedding store, non-reranked
semantic retrieval, capture-side semantic indexing, and semantic retention. On
the step-6 branch, not yet merged: cross-encoder reranking, Smart Cluster
calibration, and the Smart Cluster scoring worker.

Specifics worth knowing before touching this area:

- **OCR** runs in a standalone `carbonpaper-ml.exe` worker built on the pinned
  crate `rapidocr-core = "=0.2.2"` with the `directml` feature
  (`src-tauri/Cargo.toml`). The capture hot path calls it directly and records
  `engine="rust"` (`capture.rs`). The runtime has a watchdog that kills a stuck
  worker and a post-process retry loop with a real-failure-only budget
  (`ml_runtime.rs`).
- **Semantic inference** runs in a second worker process, reached over a
  versioned protocol (`ml_protocol.rs`, `semantic_engine.rs`) that exposes
  `embed_text`, `embed_image`, `rerank`, status, and unload. It deliberately does
  not share the OCR queue or failure domain.
- **The trait layer in `ml_contracts.rs` is documentation, not the seam.** The
  traits (`TextEmbedder`, `ImageEmbedder`, `Reranker`, `VectorIndex`,
  `ModelRegistry`, `OcrEngine`) now carry real method surfaces, but there is no
  `impl` of any of them anywhere in the tree — the module is declared
  `#[allow(dead_code)]` in `lib.rs`. Both OCR and semantic inference bypass the
  traits via the worker process plus IPC. Treat `ml_contracts.rs` as a written
  record of the intended boundaries; do not expect to find behavior behind it.
- **There is still no general named-queue job runner in Rust.** There are three
  purpose-built ones instead: the OCR watchdog and retry loop, the idle-gated
  MiniLM index worker (`minilm_index.rs`), and the migration orchestrator
  (`minilm_migration.rs`). Idle gating exists (`idle.rs`, `IdleState`), but each
  consumer wires it up for itself.

### What Python still owns today

- **Chinese-CLIP** image and text inference, and the Chroma `screenshots`
  collection. `search_nl` still forwards to Python (`monitor.rs:600`), and the
  MCP tool list drops `search_nl` when the Python monitor is not running
  (`mcp_server.rs:435`, `commands/mcp.rs`).
- **bge-reranker** inference (`monitor/reranker.py`), which serves both Smart
  Cluster calibration and the Smart Cluster scoring worker. On `main`. The step-6
  branch moves both to Rust and keeps this path as the `rerank_runtime = python`
  rollback.
- **BGE classification** (`monitor/classifier.py`), invoked from OCR
  post-process (`monitor/monitor/worker_process.py`).
- **HDBSCAN / PaCMAP task clustering** (`monitor/task_clustering.py`), including
  the `task_vectors` hot layer and the `task_centroids` cold layer.
- **MiniLM inference in three places that are not the capture path**: the
  reranked NL query encode (`task_clustering.py:1486`), the hot-layer rebuild
  (`_backfill_from_screenshots`, `task_clustering.py:1237`), and the Smart
  Cluster worker's own encode and live-encode fallback
  (`smart_cluster_worker.py:296,479`). The step-6 branch moves the first and the
  third; the hot-layer rebuild stays until Milestone 4.
- **Presidio NER** as tier 2 of the MCP output PII filter, default-on
  (`sensitive_filter.rs:73` sets `presidio_enabled: true`; the call site is
  `mcp_server.rs:825-865`).

### Why the version numbers stopped tracking the plan

Three divergences, all confirmed by reading the shipped tree:

1. **OCR became Rust-default early.** That was the original 0.8.5 target, and it
   shipped while the app was still on 0.8.3. The planned Python-default beta
   phase (original 0.8.4) and its dual-run diagnostics were skipped entirely.
2. **The two-hop reality above** replaced the one-feature-per-release plan.
3. **The Agent/MCP parallel track ran ahead** and reached the original 0.8.3
   target on its own schedule.

One consequence is recorded as debt rather than smoothed over: OCR shipped
default with **no `ocr_engine` config flag and no registry-level Python OCR
fallback**. Python OCR recognition is switched off by a runtime handshake
(`monitor/ocr_service.py:129`, `_rust_ocr_provider_active`), which is not a user-
or operator-reachable rollback. Dual-run diagnostics were never built. Milestones
2-5 must not repeat that pattern; see the flag rule in section 4.

---

## 4. Rules that bind every milestone

**SQLite is the source of truth.** Screenshots, OCR, and metadata live in
SQLite. Embeddings and ANN indexes are derived caches: they may be persisted and
migrated to avoid expensive recomputation, but they must stay versioned,
diagnosable, and rebuildable from SQLite-owned inputs. ChromaDB is the current
operational vector store, never an owner of unrecoverable user data.

**Every path has a deadline.** Every cross-thread, IPC, model, and UI request
path needs a deadline, a defined cancellation behavior, and a user-visible
failure state.

**Loading the same ONNX file is not parity.** Before a Rust replacement becomes
the default it must pass the Python-oracle behavior contract for preprocessing
and tokenization, pooling and normalization, response shape, filtering and
pagination, ranking quality, lifecycle, and rollback.

**The flag rule.** Ship a Rust replacement behind a config flag, then make it
the default, then remove the Python fallback one release later. If a replacement
ships default without a flag — as OCR did — it must still expose an observable
fallback or degrade path and a diagnostic. A skipped flag is tracked as debt, not
pretended into existence.

**Heavy machine learning waits for idle, or for the user to ask.** Background
model loads and inference use the idle policy or an explicit manual-run path. No
background work should surprise a foreground application. This is a hard
requirement, not a preference: a foreground game stutter caused by background
inference is a release blocker.

**Migration diagnostics are scaffolding, not product.** Shadow comparison
toggles, parity probes, and their sample tables exist to prove one cutover gate.
Once the gate is proven and the capability is cut over, they are removed from the
default build before that capability ships in a production (non-beta) release.
Shipping one to end users is a defect: it exposes a meaningless switch, keeps a
second inference path alive on the user's machine, and invites bug reports about
numbers nobody can act on. Whatever must survive for the observable-fallback rule
is a read-only status field on an existing status command, never a settings panel.

**Delete late.** Remove infrastructure only after the previous release ran
without needing it by default.

**Model downloads are explicit,** pinned by expected files and hashes where
practical, and described to the user as local machine-learning assets.

**Do not port a feature just because it exists.** If a Python feature is low
value, hard to explain, or expensive to maintain, demote or remove it instead —
but decide that against the *live* consumers, not against what the code looks
like. The reversed CLIP decision in Milestone 1 is the standing warning here.

---

## 5. Target end state

By the end of the 0.8.x line:

- Rust owns capture, OCR, OCR storage, keyword search, thumbnails, the model
  registry, task and Smart Cluster persistence, MCP, and app lifecycle. *(Already
  true, except for the remaining machine-learning inference and vector-store
  layers.)*
- Python is not installed by default.
- `monitor.pyz`, `requirements.txt`, the bundled Python installer, the pip sync
  UI, Python venv checks, the Python named-pipe server, reverse storage IPC, and
  Python worker supervision are gone from the default build.
- Any surviving legacy or experimental machine-learning feature is an external
  add-on, not part of the core capture pipeline.

---

## 6. Milestones

### Milestone 1 — Vector semantics and migration baseline — DONE

**Goal.** Decide what the vector collections mean and what is in scope, and
establish a stable pre/post-migration baseline. This is an internal engineering
baseline, not a user-facing health feature, so it needed no release of its own
and landed as the first Milestone 2 preparation branch.

**Where the code is.** ChromaDB held **no unrecoverable data** — every vector,
document, and metadata field is derived from SQLite-owned sources (images, OCR
text, window metadata). "Make vector state safe" was therefore already true by
construction. What was actually missing was *verifiability* and *scope
decisions*. The per-row status ledger and the rebuild executor originally planned
here moved to Milestone 2, because a ledger is only trustworthy when the index
writer maintains it first-party, and the writer becomes Rust in Milestone 2.

Audit facts the milestone rests on:

- Three collections, two key schemes: `screenshots` (Chinese-CLIP image vectors,
  keyed `md5("memory://" + image_hash)`), `task_vectors` (MiniLM text vectors,
  keyed `str(screenshot_id)`), and `task_centroids` (cold centroids).
- `postprocess_status = 'completed'` never meant "a vector exists" — text-less
  screenshots skip vector indexing and still complete.
- `task_vectors` ingest was fire-and-forget with no status anywhere. Drops from a
  missing model, a locked session, or a full queue were silent, and screenshot
  deletion did not clean the collection. The 30-day hot-layer expiry was the only
  reaper.
- Restart marks unfinished post-process work `discarded` terminally, and the
  retry button only drains an in-memory backlog of at most 32 entries. **Both are
  confirmed intentional** — bounded failure, no unbounded retry or IO storm. The
  consequence for this plan is that any rebuild must be explicit, budgeted, and
  idempotent, never automatic resurrection.

**Work — delivered.**

- `count_expected_clip_image_rows` (`storage/screenshot.rs`) — distinct
  `image_hash` among non-deleted screenshots that have an active OCR row. A
  stable proxy usable for pre- and post-migration comparison, deliberately
  avoiding a temporary schema field that Milestone 2's ledger would replace
  immediately.
- A count-level CLIP diagnostic inside `storage_get_index_health`:
  `clip_image_index { expected_eligible_images, actual_rows, missing_lower_bound,
  orphaned_lower_bound, assessment }`. Internal only — it feeds JSON, logs, and
  copy-diagnostics, and the ordinary settings UI does not display the gap.
- `chromadb==1.5.1` pinned. It was declared `>=0.4.0` while 1.5.1 was actually
  deployed; the on-disk format is not stable across majors and the cache is keyed
  to this version until Milestone 2 replaces it.
- A storage-backed test proving that multiple OCR rows and duplicate image hashes
  do not inflate the expected CLIP row count.

What this diagnostic does **not** prove: that the vectors are numerically
correct, that nearest-neighbor ranking is good, or that the MiniLM and centroid
indexes are healthy. It detects only that CLIP visual search may cover fewer
eligible images than SQLite implies, or may retain rows for inputs that are gone.

**Binding decisions.** These carry into every later milestone.

1. **Chinese-CLIP text-to-image search is retained as a distinct surface.** This
   reverses an earlier decision to demote it. `search_nl`'s live backend is
   `search_by_text` — the CLIP text encoder scoring against the `screenshots`
   image-vector collection — reachable from `AdvancedSearch.jsx`, `SearchBox.jsx`,
   and the MCP `search_nl` tool. It is cross-modal: a text query matching a
   screenshot's *visual* content. Neither the shipped Rust keyword search
   (`storage/search.rs::search_text`, a blind bigram bitmap index over OCR text)
   nor a future MiniLM-over-OCR semantic search can reproduce it. The withdrawn
   demotion had evaluated two genuinely dead functions (`search_by_image`,
   `search_by_ocr_text`) and missed the live one.
   Its valid parts survive as *scoping* rather than deletion: CLIP indexing is
   gated on `ocr_text.strip()` (`monitor/monitor/worker_process.py:166`), so the
   retained capability is precisely "text-to-image over text-bearing
   screenshots"; and the expensive image re-encode is avoided by keeping the
   already-populated collection, since deletion — not retention — is what would
   force a costly rebuild.
2. **Three labeled search surfaces, not one collapsed natural-language path.**
   OCR keyword (Rust, shipped) · semantic text over OCR (MiniLM, Rust as of
   Milestone 2) · visual and natural-language image search (CLIP text-to-image,
   retained). Different modalities with non-comparable scores. The "two
   natural-language surfaces confuse users" problem is solved by clear labeling,
   not by folding text-to-image into text-over-OCR.
3. **`task_vectors` and `screenshots` are independent collections serving
   different surfaces.** Milestone 2 re-homes each on its own terms; neither
   replaces the other. `task_vectors` deletion-orphan rows stay accepted debt
   until the Milestone 2 ledger covers them.
4. **Discard-on-restart and memory-only retry stay exactly as designed.** The
   Milestone 2 rebuild is the explicit, user-triggered complement that makes
   discarding lossless.

**Depends on.** Nothing.

---

### Milestone 2 — Behavior-equivalent Rust ONNX and per-kind index ownership — IN PROGRESS

**Target: v0.8.4 Beta.**

**Goal.** Complete hop 2 without changing product capability. Introduce Rust
ONNX inference and Rust-owned derived indexes, and cut over **one capability at a
time**, each only after its own parity, dual-write, shadow-query, migration,
foreground-isolation, and rollback gates pass. CLIP text-to-image, MiniLM
semantic-text retrieval, reranking, classification, and unsupervised task
clustering are separate consumers even though they share one ONNX Runtime. A
shared runtime does not justify a big-bang consumer switch.

**Delivery shape.** The complete implementation is developed inside this version
but lands incrementally as short stacked branches. Infrastructure may merge while
disabled. Each of MiniLM, the reranker, and CLIP moves through
`python/chroma -> rust_shadow/dual -> rust with Python fallback`. A capability
counts toward the v0.8.4 Beta completion claim only after its own gate passes.

**Explicitly out of scope for this milestone's production cutover:** BGE
classification and `task_centroids`, even though the shared Rust runtime may
exercise BGE in shadow mode.

**What Milestone 2 cannot do.** Removing Python ONNX and Chroma here would be a
functional regression. Chroma stays available for the still-live task-clustering
collections until Milestone 4, and Python BGE stays available until
classification has a Rust consumer (Milestone 5) or a deliberately supported
Rust inference bridge.

#### Sub-milestone status

| | Sub-milestone | Status |
| --- | --- | --- |
| M2.1 | Freeze the Python behavior contract | **DONE** |
| M2.2 | Separate Rust semantic runtime | **DONE** |
| M2.3 | Rust-owned derived embedding storage and ledger | **DONE** 2026-07-21 |
| M2.4 | Sentinel-triggered MiniLM migration | **DONE** 2026-07-24 |
| M2.5 | Dual-write, shadow-query, then cut over by capability | **IN PROGRESS** — 5 of 10 steps merged, step 6 soaking |

#### M2.1 — Freeze the Python behavior contract — DONE

A local, telemetry-free Python oracle and golden harness over non-sensitive
fixture text and images (`monitor/oracle/golden-v1.json`), recording and testing
the contracts already shipped:

- **Chinese-CLIP** — RGB conversion; direct square BICUBIC resize;
  `preprocessor_config.json` rescale/mean/std; tokenizer padding with no
  truncation; explicit text/image output selection; L2 normalization.
- **MiniLM** — tokenizer max length 256; attention-mask mean pooling; L2
  normalization; combined text format `process | title | OCR[:200]`.
- **BGE** — tokenizer max length 512; CLS pooling; L2 normalization.
- **bge-reranker** — pair tokenization; max length 512; **raw logits, not
  sigmoid**; variant-specific model file and output.
- **CLIP search** — cosine distance; minimum similarity 0.32; the current
  over-fetch, filter, and pagination order; the existing JSON response fields.

Rust token IDs must match exactly. CPU vectors must match to a very tight
cosine and absolute-error tolerance. DirectML may use a slightly wider numeric
tolerance, but ranking and threshold decisions still have to satisfy the M2.5
release gate.

Known current bugs are tracked separately from migration parity. Changing
natural-language time or category filtering, for instance, is an explicit bug fix
with its own tests and release note — not an accidental consequence of switching
backends.

#### M2.2 — Add a separate Rust semantic runtime — DONE

- Batch-capable Rust interfaces for text embedding, image embedding, and
  reranking, reusing the pinned model assets and tokenizer JSON. Input and output
  tensor names, pooling, and preprocessing are explicit model descriptors
  (`semantic_models.rs`) rather than output-name heuristics.
- The versioned ML protocol (`ml_protocol.rs`) carries bounded `embed_text`,
  `embed_image`, `rerank`, status, and unload operations, with maximum batch,
  token, and body limits, deadlines, cancellation behavior, provider/model/version
  diagnostics, and stable error kinds.
- **OCR keeps its own high-priority worker.** Semantic inference does not
  serialize behind the OCR critical worker and does not hold memory inside it. The
  two may share executable and runtime code, never the queue or the failure
  domain.
- **Idle gating applies to background work only** — capture indexing, rebuild,
  and maintenance model loads. A user-initiated search or calibration request is
  foreground work: deadline-bound, but never refused merely because the machine
  is in use.
- ONNX Runtime safety settings match the existing Python ones, or a change to
  graph optimization, allocator, file-versus-buffer loading, thread counts, CPU
  fallback, or DirectML device selection is justified and tested.

**Provider constraint recorded here because it shapes later steps.**
`semantic_engine.rs::provider_supports_model` refuses any non-CPU provider for
MiniLM and for bge-reranker-v2-m3, following the 2026-07-20 audit that rejected
DirectML parity for both. The Rust engine also holds **one** model resident
(`semantic_engine.rs`, `loaded: Option<LoadedModel>`), where the Python reranker
path keeps two.

#### M2.3 — Rust-owned derived embedding storage and ledger — DONE 2026-07-21

Landed on `m2/derived-vector-store`. Two layers, by design:

1. **A SQLite `derived_embeddings` cache** holding the migrated or generated
   float32 vector plus `index_kind`, `subject_key`, dimensions, model id and
   revision, embedding version, source fingerprint, and timestamps.
2. **A generation-versioned ANN sidecar** used purely as a rebuildable
   acceleration layer, written via temporary file, fsync, and atomic replace, and
   validated by header and checksum. It is never authoritative.

Persisting derived vectors is what avoids an expensive CLIP re-encode during
migration, and it gives ledger and vector writes one transactional boundary.

**Generalized subject keys** rather than `(screenshot_id, index_kind)`:
`semantic_text` uses the `screenshot_id` as a string, `clip_image` uses the
`image_hash`.

**The ledger** is `derived_index_jobs(index_kind, subject_key, status,
error_code, error, attempts, next_retry_at, model_id, model_revision,
embedding_version, source_fingerprint, updated_at)` with primary key
`(index_kind, subject_key)`. `discarded` is a legal, visible state; rebuild is
explicit and never automatic resurrection.

**Visibility and safety properties, all first-party tested:**

- Vector plus completed-ledger writes, and vector plus ledger deletion, are
  transactional. Query-visible reads join both tables and require exact
  agreement on model revision, embedding version, and source fingerprint, so
  pending, failed, discarded, invalidated, or partial rows cannot leak into
  search results.
- Workers claim queued subjects with random execution leases. Completion,
  failure, and discard are compare-and-set transitions against the active lease,
  and a commit additionally requires the referenced screenshot or image hash to
  still be active. A late worker therefore cannot resurrect deleted or discarded
  work, nor hide a newer completion.
- Startup requeues any `processing` lease left behind by an interrupted process,
  so a rebuild stays resumable across crashes.
- Runtime publication of a `.cpdvec` generation never deletes a finalized
  sidecar. Unreferenced generations are cleaned during the next startup, before
  readers are exposed.
- Deletion, model-version invalidation, duplicate image hashes, session lock and
  unlock, and interrupted writes each have tests.

The initial sidecar payload is a flat exact-scan snapshot. SQLite remains
authoritative, so the payload can be replaced by an ANN layout later without
changing persistence or generation semantics.

#### M2.4 — Sentinel-triggered MiniLM migration — DONE 2026-07-24

**What the migration is copying.** The Chroma `task_vectors` collection is a
roughly 30-day hot layer whose expired vectors are compressed into
`task_centroids`. So "every SQLite screenshot has a MiniLM vector" is **not** a
completion condition, and the coverage denominator for the later cutover gate is
the set of valid, mappable vectors in the Chroma snapshot — not the set of all
screenshots.

**Triggering.** Entirely sentinel-driven. For each vector-space revision, the
absence of `app_metadata.minilm_auto_migration_done_<revision>` starts one
full-scope, idempotent copy at startup. The worker waits for Windows Hello unlock
before reading encrypted OCR inputs. A crash, process exit, or transient worker
failure leaves the sentinel unset, and the next launch or unlock resumes from the
durable cursor and snapshot. Legacy manual or time-bounded run records are never
resumed and never allowed to settle the sentinel; the automatic mode starts a
fresh full copy instead. Terminally quarantined invalid or orphan rows may finish
as `completed_with_errors` and still settle the sentinel, whereas transient
orchestration failures stay unfinished and retry automatically.

**Maintenance mode, and its cost.** The whole run executes under global
maintenance mode: a non-dismissable full-window overlay,
`MAINTENANCE_IN_PROGRESS` rejection at the MCP and reverse-IPC boundaries (with a
small session, crypto, and status allowlist in
`maintenance.rs::reverse_ipc_command_allowed`), gated monitor
start/stop/pause/resume commands, and a paused retention and delete-queue loop.
The migration cannot be started from settings and cannot be cancelled; closing
the app is the only interruption.

**Capture is paused for the duration.** The previous monitor running or paused
state is recorded and restored exactly, but screenshots that would have been
taken during the run are not taken. This is the first maintenance operation in
the app that stops capture, and it is accepted deliberately: the alternative is
letting new captures race a rewrite of the store they are being written into.
Keeping the run short is therefore a correctness-adjacent concern, not just a
polish one.

**The snapshot.** The full hot-layer ID snapshot is built asynchronously by an
internal Python protocol (`start_task_vectors_export` -> status polling -> exact
ID pages -> `finish_task_vectors_export`), persisted with an atomically renamed
manifest under `data/migrations/minilm/<export_id>/`, and bounded by a 10-minute
logical build deadline plus 24-hour idle, 7-day hard, and 1-hour `.tmp` TTLs.
There is no user-facing time range and no cancellation.

Pages carry vectors as one little-endian float32 blob in Base64
(`embeddings_f32_le_b64`, roughly 256 KB per 128-row page) rather than as tens of
thousands of JSON floats.

**Durability.** Run state lives in `minilm_migration_runs`,
`minilm_migration_subjects`, and `minilm_migration_run_errors`. Each page commits
ledger rows, embeddings, counters, and the export cursor in one transaction, so a
crash either retries an uncommitted page or continues past it, and an interrupted
run resumes by re-attaching to the persisted snapshot. A snapshot Python can no
longer restore forces a cursor reset rather than a silently reordered page walk.

**Validation and mapping.** Rust accepts only canonical positive screenshot IDs
and finite, non-zero 384-dimensional vectors — zero vectors are quarantined,
never imported. It maps them to active SQLite screenshots and rehydrates
process, title, category, and OCR from SQLite. A valid legacy vector whose
current SQLite text is empty is still copied, marked
`legacy_chroma_unverified` rather than recomputed. Orphan Chroma IDs, corrupt
vectors, and rows that disappear after the snapshot remain persisted diagnostics
and yield `completed_with_errors`. **The migration never runs inference to
manufacture a missing vector.**

**Reconciliation.** After the copy, a full-scope run deletes Rust
`semantic_text` rows outside the snapshot scope. During this phase Python's
hot-layer expiry mirrors its deletions to Rust through reverse IPC with a
persisted retry queue, so the Rust collection tracks rather than outgrows the
Chroma hot layer. *(Step 5 later reverses the direction of ownership and removes
this mirror.)*

**Operational bounds.** A disk preflight (estimated peak × 1.25 + 1 GiB, with a
64 MiB transient allowance) rejects the run before any vector write. Generation
publication runs on a blocking worker with progress phases `publishing_sync`,
`publishing_verify`, and `publishing_commit`. The final `sync_all` window is
surfaced to the user as an uninterruptible safe write.

**Contract.** The MiniLM source contract stays exactly `process | title |
OCR[:200]`. Its versioned fingerprint excludes category and is computed from the
final Rust-rehydrated model input. Legacy Chroma rows do not record which runtime
produced them, so imported and newly generated vectors share the reviewed
compatibility contract `minilm-l12-vector-space-v1`.

**What M2.4 does not do.** Chroma and Python remain the authoritative query
backend throughout this phase — there is no shadow-query or production-query
switch here. `task_centroids` and all Chroma operations needed by Python HDBSCAN
and PaCMAP stay unchanged until Milestone 4, so `chromadb` remains a default
dependency.

#### M2.5 — Dual-write, shadow-query, then cut over by capability — IN PROGRESS

The cutover sequence, with status:

| Step | Work | Status |
| --- | --- | --- |
| 1 | MiniLM Rust inference parity | **DONE** |
| 2 | MiniLM derived-cache dual-write and migration | **DONE** (M2.4) |
| 3 | Rust semantic shadow queries against Chroma, Python authoritative | **DONE** 2026-07-27, harness since retired |
| 4 | Cut over the **non-reranked** semantic-text retrieval path | **DONE**, merged |
| 5 | Rust capture-side MiniLM indexing and retention ownership | **DONE** — merged in PR #150 |
| 6 | Reranker parity and shadow scoring, then cut over Smart Cluster calibration **and the scoring worker together** | **IMPLEMENTED, SOAKING** — `m2/reranker-shadow-cutover`; latency measured 2026-08-01 (see below), reranked-ordering numbers outstanding |
| 7 | CLIP vector export and migration | PLANNED |
| 8 | Rust CLIP image-encoder dual-write for new captures | PLANNED |
| 9 | Rust CLIP text-query shadow mode, then cut over `search_nl` and MCP capability reporting | PLANNED |
| 10 | BGE in the shared Rust runtime, shadow mode only | PLANNED |

Two ordering constraints are load-bearing. **Step 8 precedes step 9:** do not cut
over visual search while new screenshots still depend solely on Python image
encoding, or old data will be searchable while new captures silently stop being
indexed. **Step 10 does not remove Python BGE:** the classification consumer is a
Milestone 5 item, and Python BGE inference stays until classification has a Rust
path or a deliberately supported Rust inference bridge.

##### Step 3 — measured result (2026-07-27)

Over 256-query and 256-document samples: query-encoder maximum absolute error
6.0e-7; Overlap@10 p50 100%, p05 50%; top-1 agreement 90.7%; document re-encode
cosine p50 0.9917, p05 0.9868, min 0.9796.

Retrieval divergence is dominated by Rust returning documents the Chroma
approximate-nearest-neighbor path does not surface. The Rust side is an exact
scan over the same hot layer, so a strict superset of recall is the expected
outcome, and it was accepted as a difference in retrieval *method* rather than a
Rust defect.

Recorded honestly: this clears the query-encoder numeric gate but not the
*original literal* wording of "top-10 overlap at least 99%, top-1 effectively
unchanged", which assumed two implementations of the same retrieval method. That
wording is superseded in the release gate below rather than waived.

The 0.83 document-encoder figure seen in an earlier report was a measurement
artifact, not a divergence: the probe read OCR blocks in geometric order
(`ORDER BY box_y1`) while the write path encoded them in engine order. The fix
was to match engine order on the rebuild side. Production clustering vectors were
never affected.

##### Step 6 — measured cross-encoder latency (2026-08-01)

Measured against a real 10,605-vector corpus and the shipped ONNX session
configuration, on a 16-logical-core desktop.

| Quantity | Measured |
| --- | --- |
| One document, 325 tokens, 1 intra-op thread | 1.18–1.25 s |
| Same, 4 threads / 8 threads | 0.55 s / 0.31 s |
| Per-document cost at batch 1 / 2 / 4 / 8 (1 thread) | 1.248 / 1.246 / 1.259 / 1.301 s |
| Cross-encoder load (544 MB uint8, warm page cache) | 1.2 s |
| MiniLM load / query encode | 0.50 s / 2 ms |
| Prefilter pass rate at the shipped 0.40 cutoff | 0.39%–5.04% by anchor |

Three of these changed decisions:

**Batching buys nothing.** Per-document cost is flat to slightly worse from
batch 1 to batch 8 at every thread count, because the session is sequential with
one intra-op thread and has no batch parallelism to exploit. `MAX_COMMIT_PAIRS`
shrinking a commit group to a single snapshot therefore costs no throughput, and
`BACKGROUND_RERANK_CHUNK` could be lowered for free.

**`BACKGROUND_RERANK_CHUNK` was above the foreground budget, not below it.** Its
previous value of 4 held the single request slot for 4.72 s against a 5.0 s
foreground query budget that still had to cover a 0.50 s MiniLM load — a margin
of −0.22 s. Lowered to 1.

**`with_intra_threads(1)` was the most expensive constant in the path.** Now
`min(8, cores / 2)`, which is both roughly four times faster and four times more
responsive when standing down, since a background request cannot be interrupted
once submitted.

Sequence length drives the cost almost linearly: 54 tokens 0.19 s, 84 tokens
0.29 s, 190 tokens 0.66 s, 325 tokens 1.18 s. A 600-character OCR snippet — the
`RERANK_OCR_SNIPPET_CHARS` cap — reaches roughly 325 tokens on mixed
Chinese-English screen text.

Reranked-ordering parity against the retired Python DirectML scorer is still
outstanding; the threshold-provenance mechanism is what makes the cutover safe
without it.

##### Step 6 follow-up — the calibration wait (2026-08-02)

Dividing the measured per-document cost into the shipped constants showed the
calibration query could not finish inside its own budget on ordinary hardware,
which the step-6 measurement pass had not checked because it measured the
scoring path. `intra_threads` is `clamp(cores / 2, 1, 8)` and the picker offered
10, 30, 60, and 120 results at an over-fetch of four, so the shipped
configuration spanned 40 to 480 documents at 0.31 to 1.18 s each against one
120 s deadline:

| Logical cores | Threads | Per document | top 10 | top 30 | top 60 | top 120 |
| --- | --- | --- | --- | --- | --- | --- |
| 16 | 8 | 0.31 s | 12 s | 37 s | 74 s | **149 s** |
| 8 | 4 | 0.55 s | 22 s | 66 s | **132 s** | **264 s** |
| 4 | 2 | ~0.8 s (interpolated) | 32 s | 96 s | **192 s** | **384 s** |
| 2 | 1 | 1.18 s | 47 s | **142 s** | — | — |

The default request was on the line at four cores and over it at two, and the
largest option exceeded the deadline on the sixteen-core machine the latency was
measured on. Exceeding it was not a hard failure — the query fell back to Python
and eventually returned — but it cost the user two minutes of an unlabelled
spinner, discarded every score already computed, and then started over. It
becomes a hard failure when the fallback is removed, which this milestone is in
the middle of doing.

Four changes, none of which touch a score:

**The result picker is capped at 30, enforced in the backend.** A calibration
session needs enough candidates to mark three positives, not 480 documents of
them. `rerank.rs::MAX_RERANK_RESULTS` clamps at the command boundary rather than
inside the Rust path, so the bound holds for a query Python answers too and the
two backends stay comparable. The non-reranked query keeps its old bound of 200:
it costs one encode and a cosine scan.

**The whole-query deadline becomes a per-chunk one.** A total deadline cannot
tell a slow machine from a stuck worker, and on this path the two needed
different answers. Each chunk now gets `RERANK_CHUNK_STALL` (180 s, sized to
cover a cold 544 MB model load, which lands inside the first chunk), under a
`RERANK_QUERY_CEILING` of 15 minutes as a runaway guard for a query nobody is
watching anymore. What makes an open-ended budget acceptable is the same
argument step 5 recorded for dropping the subject cap on the manual indexing
run: presence argues for reporting and interruptibility, not for a fixed
stopping point.

**The foreground chunk drops from 64 to 8.** Batching was already measured to
buy nothing, so this is free in throughput, and the chunk is the resolution of
both the progress bar and the stop button.

What it is not free in is inter-chunk gaps: fourteen where there was one. The
engine keeps a single model resident, so each gap is a place another pass can
take the worker, and the first draft of this change justified the shrink with
"the whole query holds a `foreground_lease` and background callers check it
before submitting" — which was true of the idle loops and false of the two
passes a user starts. A manual index run (`minilm_index.rs`) and a forced Smart
Cluster drain (`smart_cluster_scoring.rs`) both read straight past the lease on
the grounds that somebody had pressed their button. That is a sound argument
about consent and the wrong answer about cost, and the two were costing
different things: the index run wants MiniLM, so every chunk it lands between
two rerank chunks evicts the cross-encoder and buys the next chunk a 570 MB
re-read, a full SHA-256 re-verification, and a fresh ONNX session — about 1.2 s
against roughly 2.5 s of actual scoring. The drain wants the same
cross-encoder, so it evicts nothing and instead simply takes turns at the one
slot, which halves the query rather than sharing anything with it.

So the shrink is paired with closing that gap. **Both user-initiated passes now
stand aside and resume rather than stand down**: they stop submitting the
moment a lease is taken, wait on a 250 ms poll for it to clear, and then claim
a fresh batch, so their button still means something without the collision
being charged to the query. Each carries its own wait budget — five minutes for
the index run against its thirty-minute deadline, three for the drain against
its ten — and ends under a reason that names the actual cause rather than
reporting a deadline it spent waiting. Both also submit under
`RerankPriority::Background` now, forced drain included, so "stops submitting"
takes effect within one document rather than within one whole rerank call: the
drain used to run at `Foreground`, which bought it a commit group's candidates
uninterrupted when scoring — around nineteen seconds on the slowest machine
measured — and a cluster's entire saved example set when re-deriving a
threshold, against the five seconds a plain search has to reach the same
worker.

Reverting either pass to "ignore the lease" puts the foreground chunk back to
64.

**The query reports itself and can be stopped.** `nl-rerank-progress` carries a
phase and a scored/total ratio after every chunk; `nl_rerank_stop_now` ends the
query within one chunk. A stop is *not* a fallback — `RustQueryOutcome::Cancelled`
returns a success with `cancelled: true` rather than handing the query to
Python, because re-running on the other backend the work somebody just asked to
end would take longer than not stopping at all. The three phases are named
separately because only one of them has a denominator: retrieval is
milliseconds, the model load is one opaque 544 MB read, and reranking is the
part that takes minutes.

A query Python answers reports `external_backend` and hides the stop button.
Python fuses retrieval and reranking into one IPC call, so there is no progress
to show and no chunk boundary to stop at, and the alternative to saying so is a
bar that never moves above a button that silently does nothing.

##### Step 4 — non-reranked retrieval cutover — DONE

`semantic_query.rs` serves the non-reranked natural-language query from a Rust
MiniLM query encode plus an exact cosine scan over the migrated derived store,
rehydrating response metadata from SQLite.

**Scope: retrieval only, not calibration.** An earlier version of this plan had
step 4 cutting over "semantic-text retrieval and Smart Cluster calibration
prefilter". In the shipped code those are not separable here. Calibration always
reranks — `NlClusterView.jsx` initializes `enableRerank` from `isCalibrate` and
renders the toggle only in the non-calibrate branch, so a calibration session has
no way to turn it off — because it needs `rerank_score` to derive a per-cluster
threshold. And Python's `query_by_text` (`task_clustering.py:1433`) performs
retrieval and reranking in one call, with no standalone rerank operation on the
protocol. Cutting over the calibration prefilter alone would mean building a
Rust-retrieve-then-Python-rerank bridge that exists for exactly one release and
is deleted at step 6 — more new surface than it saves, on the one path where a
ranking mistake silently corrupts a saved threshold. So step 4 cuts over
`enable_rerank = false` only. `enable_rerank = true` continues to run entirely on
Python and moves at step 6, where the end-to-end reranked ordering is re-measured
anyway.

**Defaults and rollback levers.** The cutover flips the *defaults* of both enums
to `rust`, not merely the set of legal values — a cutover that left
`semantic_index` defaulting to `chroma` would ship a switch nobody flips. This is
safe on a machine whose M2.4 migration has not run, because an empty Rust index
is a fallback condition and the query is served from Python.

Two independent levers restore the previous behavior for the one required
release:

- `semantic_index = chroma` — Python owns retrieval.
- `semantic_runtime = python` — no Rust MiniLM inference at all. This is honored
  as a *refusal* rather than silently overridden, because serving from the Rust
  store necessarily encodes the query with the Rust runtime.

Every refusal is recorded with its reason and read back through
`get_ml_semantic_status.backend`, which the Settings → Advanced card renders
alongside the `semantic_index` switch. Both the diagnostic and the rollback are
reachable without a registry editor.

That card was titled "semantic retrieval backend" and its switch "use Rust
semantic retrieval" until 2026-08-01. Both were wider than the thing they
control, which is MiniLM retrieval over screenshot *text* — the natural-language
grouping view and Smart Cluster calibration — while `search_nl` in the main
search box is Chinese-CLIP over images and does not move until step 9. A user
reading the old label could not tell which of the two searches the switch
governed, or whether turning it off stopped indexing (it does not: capture
indexing is unconditional, and only the read path follows this switch). The
label, the description, and a second line about what the switch does *not* do
now say so.

**Coverage is the failure mode this step defends against.** An empty Rust index
falls back, but a *partially* filled one would not, and ranking an incomplete
corpus returns a plausible page with screenshots silently missing from it —
something no user can detect and no after-the-fact instrument can reconstruct.
Two conditions can leave the local store a prefix of a corpus somebody else holds
in full, and each keeps its refusal:

- **`migration_incomplete`** — the M2.4 copy has not finished. It commits page by
  page and a migrated row becomes query-visible the moment its job row reaches
  `completed`; there is no generation gate in front of the read path. A run that
  fails mid-session drops the maintenance guard and does not retry until the next
  launch, so the rest of that session would otherwise serve a prefix. The
  once-per-revision sentinel gates retrieval too, cached in-process because it is
  a one-way transition.
- **`rust_index_empty`** — the store is empty outright, which is the unmigrated
  machine.

A third refusal, `index_incomplete`, existed in step 4 and was removed by step 5;
the reasoning is in the step-5 section.

**Behavior difference, recorded as a bug fix.** Python reads process, title,
timestamp, and category out of Chroma metadata, so a screenshot deleted after
indexing is still rendered from a stale copy. Rust reads them from SQLite and
drops rows that no longer map to an active screenshot, which means a Rust
response can be shorter than `n_results` where the Python one was not. This
belongs in the release notes.

**Shadow scaffolding was retired with this step, as the rule requires.** Deleted:
`semantic_shadow.rs` and `storage/semantic_shadow.rs`; the
`semantic_shadow_samples` and `semantic_doc_encoder_runs` tables, now dropped in
`storage/schema.rs`; `SemanticShadowCard` and its wiring in `AdvancedSection.jsx`
and `useAdvancedSectionController.js`; the three shadow Tauri commands; the
`rust_shadow` value of `semantic_runtime`, which now normalizes to the shipped
default like any unrecognized string.

Explicitly **not** scaffolding — this is the production read path and stays:
`derived_index::semantic_text_topk`, `ScoredSubject`,
`count_query_visible_embeddings`; `storage/semantic_cache.rs` (the resident
vector matrix), its `StorageState` fields, the idle-eviction ticker in `lib.rs`,
and the cache resets in `storage/schema.rs`; the `semantic_runtime` and
`semantic_index` enums themselves, minus the retired value, since they are the
observable rollback switch. `minilm_index::minilm_sources` was the
document-encoder probe's source builder and is now the capture worker's — it is
production code.

##### Step 5 — Rust capture-side indexing and retention — DONE

**Why this step exists.** The numbered sequence originally had no Rust
capture-side MiniLM indexing step, even though the M2.5 release gate requires
that "with Python stopped, new-capture Rust CLIP and MiniLM indexing continue to
work". CLIP got such a step (now step 8); MiniLM was skipped. Before this step,
the only writers of `semantic_text` rows were the M2.4 migration and the Python
dual-write over reverse IPC, and the only reaper was Python's hot-layer expiry
mirroring its deletions across. Rust could already embed, but nothing called it
for a new screenshot. So step 4 delivered `semantic_index = rust` in the honest
sense of "Rust owns the read path", not "Rust owns the index". Step 5 closes
that, after which the "Python stopped" gate can genuinely be evaluated.

**What landed.**

- `minilm_index.rs` — the capture path enqueues a `semantic_text` ledger job on
  the OCR commit. An idle-gated worker claims, encodes, and commits vector and
  ledger in one transaction, queues the Smart Cluster pending entry, and mirrors
  the finished row into Chroma through the existing `upsert_task_vectors`
  command. The same worker ages rows out at 30 days and re-queues screenshots
  whose enqueue never ran.
- `semantic_query.rs` — the `index_incomplete` refusal is gone. The ledger depth,
  the count of jobs whose retry budget is spent, and the age of the oldest
  waiting screenshot are reported instead, and Settings → Advanced shows them.
  `migration_incomplete` and `rust_index_empty` stay.
- Python — `add_snapshot`, the dual-write, the durable import-retry journal, the
  delete mirror, and the capture-path clustering ingest queue are removed, along
  with the three reverse-IPC handlers that served them. `HotColdManager` keeps
  the hot layer as a *consumer*: clustering reads it, `compress_to_cold` ages it,
  and `_backfill_from_screenshots` rebuilds it when it is found empty.

**Decision 1 — Rust becomes the only MiniLM encoder on the capture path, and the
mirror reverses direction.** The obvious reading of "Rust encodes new captures"
would add a second encoder beside the Python one, embedding every screenshot
twice. Dropping the hot layer instead is not available: Milestone 4 task
clustering still reads `task_vectors`, and `compress_to_cold` still derives
`task_centroids` from it. So Rust takes over the inference and hands the finished
vector to Python for the Chroma write — the M2.4 dual-write with its direction
reversed. The failure modes reverse with it, in the direction that matters: a
lost mirror now degrades unsupervised clustering rather than making a screenshot
unfindable by natural-language search.

Note the precise scope. Python still runs MiniLM for the reranked query encode,
for `_backfill_from_screenshots`, and inside the Smart Cluster worker. Those
consumers move at step 6 and Milestones 3 and 4.

**Decision 2 — indexing is strictly idle-gated, and the coverage rule changes
shape rather than tightening.** MiniLM is a 118 MB model
(`semantic_models.rs`, 118,308,126 bytes), over the line at which background
inference waits for an idle window, so the drain worker is gated on the existing
idle policy instead of running inline on the post-process path the way Python
did.

Three consequences, all deliberate and all recorded rather than smoothed over:

1. **Search freshness regresses.** A screenshot captured during active use is not
   semantically searchable until the next idle window, where previously it was
   searchable within seconds. The backlog depth and the age of the oldest waiting
   screenshot are reported in the backend diagnostic so the cost is visible
   rather than merely felt.
2. **A manual run is the second permitted path, and it now exists.**
   Section 4 permits heavy machine learning to gate on idle *or* on an explicit
   manual run; step 5 shipped only the former. `semantic_index_run_now`
   (`minilm_index.rs`) is the latter: single-flight against the idle worker
   through a shared pass guard, and reachable from the Settings → Advanced card
   next to the backlog number that motivates pressing it. It ignores the idle
   signal and nothing else — maintenance mode, a locked session, and the
   ledger's retry budget and backoff all still apply, because the user
   consenting to spend their own CPU does not make a concurrent rewrite of the
   derived store safe.

   **Correction (2026-08-01): the run drains the queue; the subject budget that
   used to cap it at 128 screenshots is removed.** The budget was reasoned from
   "the user is present, so do not turn one click into half an hour of
   foreground CPU", and it was paired with a 180-second deadline meant to be the
   real ceiling. The step-6 measurements make the arithmetic checkable, and it
   does not hold: MiniLM loads in 0.50 s and encodes a query in 2 ms, so 128
   subjects finish in seconds and the budget fired roughly two orders of
   magnitude before the deadline it was supposed to complement. What the user
   saw was a button that worked for a moment, stopped without saying why, and
   had to be pressed dozens of times to clear a real backlog.

   Presence is an argument for reporting and interruptibility, not for a fixed
   stopping point, so that is what replaced it: the pass emits
   `semantic-index-progress` after each encoded chunk against the queue depth it
   started with, and `semantic_index_stop_now` ends it within one chunk, putting
   everything claimed and unencoded back without charging a retry attempt. The
   deadline stays at a raised 30 minutes as a runaway guard for a run nobody is
   watching anymore, which is the job it can actually do.

   Removing the budget exposed a second defect and fixed it. The drain loop did
   not inspect why an inner `drain_queue` stopped, so a failing encode was
   answered by claiming the next batch and failing identically; with the budget
   gone that would have walked the entire backlog past a broken worker, charging
   a retry attempt against every screenshot on the way. The loop now stops on
   any reason the inner drain reports.
3. **Battery: resolved, and the original analysis of it was wrong.** The
   pre-implementation note said that because `is_idle` requires AC, "on a laptop
   running on battery the worker never runs" and "the backlog is bounded only by
   how long the machine stays unplugged". Both halves were wrong, in opposite
   directions, and the correction is recorded here rather than quietly dropped.

   `idle.rs` did not read AC power. It read `!PowerState.active`
   (`idle.rs`, before this change), and `power.rs` only raises `active` when
   `power_saving_mode_enabled` is on *and* AC drops — at which moment it also
   calls `stop_monitor_impl` and stops capture. So the two configurations behaved
   as follows, neither of them as described:

   - **Power saving on (the default).** Unplugging stops capture, so no new
     screenshots are produced and the backlog does not grow. It is frozen at
     whatever accumulated before unplugging, not unbounded.
   - **Power saving off.** `active` stays false, `ac_connected` therefore reads
     true, and the indexer runs on battery — including the 118 MB model load.
     Nobody decided that; it fell out of a variable named for the condition it
     was standing in for.

   The fix is to make the gate mean what its name says: `idle.rs` now calls
   `power::is_ac_power_connected` directly, so "background machine learning does
   not run on battery" holds in both configurations, and the composite rule lives
   in one tested function (`composite_idle`) rather than inline in the monitor
   loop. `power_saving_active` stays in the emitted event and the log line as a
   separate observable, because it answers a different question — why capture
   stopped, not why indexing did. A battery-only machine indexes on its next AC
   session, or on demand through the manual run above.

**Why the `index_incomplete` refusal did not survive.** Not because idle gating
would trip it nightly. That refusal assumed Python held a complete corpus to fall
back *to*, which was true while Python was the encoder. Once Rust encodes and
Chroma receives its rows from the Rust mirror, both stores are behind by
essentially the same screenshots, so handing the query to Python buys the user
nothing while costing them the faster path. The backlog therefore stops being a
reason to refuse and becomes a reported number.

One caveat on "essentially": `_backfill_from_screenshots` can still write Chroma
rows that never reach Rust, since it encodes from SQLite with Python's own
embedder and there is no longer a reverse dual-write. It only fires when the hot
layer is found entirely empty, so this is an edge case rather than a routine
divergence — but the two stores are not identical by construction, and a future
step that depends on their being identical must not assume it.

**Decision 3 — retention becomes first-party; deletion already was.** The
pre-implementation audit recorded that a user-deleted screenshot leaves its
derived row behind as an unbounded storage leak. That was wrong, and it is
corrected here rather than quietly dropped: the schema has carried
`cleanup_derived_index_on_screenshot_soft_delete` and its hard-delete twin since
the derived layer was introduced, and both remove the embedding and the ledger
row inside the deleting transaction. What was genuinely mirrored from Python was
*expiry on age* — the only deleter of aged Rust `semantic_text` rows was Python's
hot-layer expiry sending its own deletions back over reverse IPC. Step 5 gives
Rust its own 30-day rule against SQLite `created_at` and removes that mirror;
Python keeps expiring its own Chroma hot layer for the clustering path. The two
stores stop tracking each other, which is what ownership means here. The reaper
also sweeps subjects with no live screenshot, which after the triggers is a
safety net for rows written before they existed rather than a live path.

**Decision 4 — the Chroma mirror is best-effort, and the residual gap is named.**
Rust holds the authoritative copy, so a mirror lost while the monitor is down or
clustering is disabled costs that screenshot its place in unsupervised task
clustering, not its findability by search. It is not re-sent:
`_backfill_from_screenshots` rebuilds the hot layer only when it is found
*entirely* empty, so a partial gap stays open until the row ages out. The Smart
Cluster prefilter is unaffected, since it already falls back to a live encode for
any ID the collection lacks. Closing the clustering gap belongs with Milestone 4,
which is where `task_vectors` is actually consumed and where a Rust-to-Python
vector read would have a second user.

**The Smart Cluster pending enqueue moved with the encoder.** Python's
`add_snapshot` called `smart_cluster_enqueue_pending` right after it wrote the
vector, so the Rust writer makes that call in the same position — which also puts
the queue entry point in Rust before step 6 needs it.

##### Step 6 — reranker and Smart Cluster scoring — IMPLEMENTED, SOAKING

**Scope: the scoring worker moves with the reranker.** An earlier version of this
plan allowed the Python Smart Cluster worker to keep scoring while calibration
moved to Rust, with a temporary reverse-IPC rerank bridge as an option. A source
audit rules that arrangement out.

`monitor/reranker.py` builds its own provider list and prefers DirectML
(`reranker.py:186-187`), so today both the calibration scores and the worker's
assignment scores come from a GPU session, while the Rust engine refuses
DirectML for this model outright. Calibration writes its result into
`smart_clusters.threshold` and leaves it there, and the worker compares its own
logits against that stored number. Moving calibration alone would leave a
persisted threshold produced by one scorer being applied by another —
assignments that quietly over- or under-fire, against a number the user never
sees and cannot correct.

So step 6 cuts over calibration and the scoring worker as one unit, pulling the
Milestone 3 scoring path forward. Milestone 3 keeps the rest of that worker's
surface.

**Three implementation consequences.**

1. **Batching and a foreground latency budget.** The Rust cross-encoder is
   CPU-only while Python's is not, and the calibration path over-fetches
   `n_results * rerank_overfetch` — 120 documents at the defaults
   (`task_clustering.py:1433`, `n_results = 30`, `rerank_overfetch = 4`) —
   against a protocol cap of `MAX_RERANK_DOCUMENTS = 64`
   (`ml_protocol.rs:18`). The step needs request batching and a *measured*
   foreground latency budget before it can claim its gate. The shipped Python
   path also keeps two models resident, where the Rust engine holds one and
   re-verifies a 570 MB file (`semantic_models.rs`, 570,727,094 bytes) on every
   swap.
2. **Thresholds already on disk were produced by the retired scorer.** Each
   cluster has to record the scorer that produced its threshold — model,
   revision, variant, provider — so that a threshold from a scorer that no longer
   exists is recognizable rather than silently reused.
3. **The `rerank_variant` selector is a loose end to resolve, not carry across.**
   Rust pins `model_uint8.onnx`, which is also the only variant
   `model_management.rs` installs, so the multi-variant dropdown already offers no
   real choice. There is a live inconsistency to clean up at the same time:
   `NlClusterView.jsx` and `query_by_text` default to `uint8`, while
   `task_api.js` and `monitor.rs` default to `q4f16` — a variant that is never
   installed. Do not carry the dropdown across the cutover as a live switch.

**What landed.**

- `rerank.rs` — the consumer layer the cross-encoder never had. It holds the
  document contract both callers share (`process | title | OCR[:600]`, joined on
  the non-empty parts, `"(empty)"` when nothing survives), the chunking that a
  64-document protocol cap forces on a path that over-fetches 120, one deadline
  spanning every chunk, the `rerank_runtime` rollback switch, and
  `ScorerIdentity` — model, revision, variant, provider — which is the thing a
  stored threshold is measured against.
- `semantic_query.rs` — `try_rust_reranked_nl_query` retrieves with the
  bi-encoder and re-scores with the Rust cross-encoder, in the same two stages,
  the same over-fetch factor, and the same descending raw-logit sort Python's
  `query_by_text` performed in one call. The reranked response now names the
  variant that produced its scores instead of leaving it null.
- `smart_cluster_scoring.rs` — the Rust drainer of `smart_cluster_pending`.
  Same three stages as `smart_cluster_worker.py` and the same constants, because
  those constants produced the thresholds already on disk: MiniLM cosine
  prefilter at 0.40, cross-encoder rerank of the survivors, assignment above the
  cluster's threshold; 32 snapshots per idle pass and 128 per forced one, shrunk
  so `(snapshots × clusters)` stays inside a 4096-pair budget. Idle-gated like
  the indexer, so it inherits step 5's AC-power rule. A pass that leaves the
  570 MB cross-encoder resident releases it when it finishes
  (`semantic_runtime.rs::unload_model`), which is where Python's worker
  unloaded its own reranker; the unload is conditioned on the reranker actually
  being the resident model, so it cannot evict the MiniLM session the capture
  indexer is about to reuse.
- Storage — `smart_clusters` grows five `threshold_*` columns, written in the
  same statement as the threshold itself, plus a sixth recording the scorer for
  which re-derivation has been ruled out. `list_smart_cluster_scoring_targets`
  reads what the scorer needs in one query, and
  `get_query_visible_embeddings_by_subjects` reads a whole peeked batch of
  vectors in one statement rather than taking the database mutex once per
  screenshot while a foreground query waits behind it.
- Python — `monitor/monitor/__init__.py` leaves the Smart Cluster worker
  unstarted unless `CARBONPAPER_RERANK_RUNTIME` says `python`. Two drainers on
  one queue would score the same snapshots twice, against different logits.
- Frontend — the ONNX variant dropdown is gone from `NlClusterView`, and the
  `rerankVariant` argument is gone from `nlClusterQuery`. The loaded variant is
  still displayed, because "which variant produced this score" stays a real
  question even when there is only one answer.

**Decision 1 — every threshold on disk records its scorer, and a threshold from
a retired scorer is re-derived rather than trusted or discarded.** Both other
options are bad: trusting it applies Python's DirectML logits to Rust's CPU ones
on a number the user never sees, and discarding it asks every user to redo
calibration work they already did. So each cluster stores the model, revision,
variant, and provider that produced its threshold, and a cluster whose recorded
scorer is absent or different has its threshold recomputed from the calibration
examples stored beside it — the same positives and negatives the user picked,
re-scored with the current scorer, through the same formula the calibration UI
applies. Nothing is invented.

A cluster whose examples can no longer support a threshold — every positive
screenshot deleted, typically — is **given up on, and said so**: the worker
counts those clusters, the status payload carries the count, and the Smart
Cluster screen shows how many stopped being scored and that recalibration is
what resumes them. A cluster that quietly stops matching is indistinguishable
from one that has nothing to match.

Giving up is recorded rather than re-decided. Re-derivation loads a 570 MB
cross-encoder, and a cluster with no positives left fails it identically every
time, so the verdict — together with the scorer it was reached under — is
written to the row, and the cluster is skipped from then on without touching the
model. Re-saving the examples clears it, which is what recalibration does. Only
a *transient* failure, such as a rerank that errored or timed out, is retried,
and then only once per pass. When every enabled cluster has been given up on,
the queued snapshots are dropped for the same reason the no-enabled-clusters
branch drops them: nothing will ever score them, and holding them would leave a
queue that grows with every capture and a status line claiming work is pending.

**Decision 1a — the threshold is stamped with the backend that answered the
calibration query, not with the one the switch selects.** Those are not the same
thing. `monitor_nl_cluster_query` hands a reranked query to Python whenever the
Rust path stands down for a reason of its own — `semantic_index` or
`semantic_runtime` pointing elsewhere, maintenance, an unfinished M2.4
migration, an empty Rust index, an error mid-query — none of which
`rerank_runtime` knows about. Reading the switch would therefore stamp Rust's
CPU identity onto a threshold derived from Python's DirectML logits, and the
worker would trust it, which is the one outcome the provenance columns exist to
prevent. The response already reports which engine served it; the frontend
carries that value into `smart_cluster_create`, and a caller that cannot say
leaves the columns NULL — indistinguishable from a pre-provenance threshold, and
repaired the same way.

**Decision 2 — status and rollback move as one surface.** Rather than three new
Tauri commands, the three that already existed —
`monitor_smart_cluster_worker_status`, `_drain_now`, `_stop_drain` — branch on
`rerank_runtime`, and `monitor_nl_cluster_reranker_status` branches with them.
The frontend contract does not move, and the rollback lever switches status,
force-run, cancel, and availability reporting together instead of leaving the UI
talking to one backend about another one's work. The reranker status in
particular had to move: answered from Python while Rust reranks, it warns
"unavailable" on a calibration screen that works whenever Python is stopped, and
stays silent when the file Rust actually loads is missing.

**Decision 3 — the calibration threshold formula is ported exactly, including
the part that looks like a bug.** `base = min(positive) × 0.85`, then
`max(base, max(negative) × 1.05)` when negatives exist. Reranker outputs are raw
logits and are routinely negative, so multiplying a negative ceiling by 1.05
moves it *down*, and the outer `max` is what keeps `base` standing in that case.
Ported rather than corrected: changing it is a behavior change to calibration
against thresholds already on disk, not a porting decision, and it belongs in
its own change with its own release note.

**What is not yet true.** The code is written, compiles, and passes its unit
tests; two gate items need a real machine and are the reason this step is
**SOAKING** rather than DONE:

1. **The foreground latency figure is unmeasured.** The Rust cross-encoder is
   CPU-only where Python's prefers DirectML, and a calibration query reranks 120
   documents in two chunks after a possible cold 570 MB model load. The step's
   own gate asks for a *measured* number, and no such number exists yet.
   `RERANK_QUERY_TIMEOUT` is set to 120 s to cover the cold-load worst case,
   which is a deadline, not a measurement.
2. **Reranked end-to-end ordering has not been compared against Python.** The
   release gate returns overlap@10 and top-1 agreement to pass/fail status at
   this step. The shadow harness was deleted at step 4, so this is an offline
   comparison to run before the cutover ships, not something the runtime
   reports.

**Decision 4 — the two background passes are serialized, and both give the
worker up to the user.** Review of the soaking build found the scoring worker
sharing nothing with the capture indexer it was modeled on. Both poll every
60 seconds, both gate on the same idle signal, and both were spawned seconds
apart, so they wake in the same window; but they want different models from an
engine that keeps exactly one resident, and every swap re-reads the model file,
re-hashes it in full (`semantic_models.rs::verify_model_files` — 570 MB for the
cross-encoder), and rebuilds the ONNX session. Running them at once does not
double throughput, it makes each pass evict the other's session. The guard that
already existed for this was private to `minilm_index.rs`; it is now
`semantic_runtime::BACKGROUND_PASS_GUARD` and both loops claim it. An idle tick
that loses it skips, and a forced drain waits.

The same review found the foreground path exposed to the background one.
`acquire_request_slot` is a single slot held for a whole request, a background
rerank could hold it for the length of a 64-document CPU batch, and an NL query
has five seconds — which covers the wait, not just the encode. The idle signal
cannot solve this: `idle.rs` polls `GetLastInputInfo` every ten seconds, so it
learns the user is back well after the search they typed has already given up.
So a foreground query now announces itself directly
(`SemanticRuntimeState::foreground_lease`), background passes check that between
clusters and between rerank chunks, and the background chunk is four documents
rather than sixty-four, because a request in flight cannot be interrupted and
the chunk size therefore *is* the worst-case foreground wait. A forced drain
keeps the full chunk and does not yield: it is a button the same user pressed.

Chunk size cannot move a score — a cross-encoder evaluates each pair
independently, which is what made the existing batching legitimate — so this is
a scheduling change and not a scoring one. The value of four is conservative
pending the measurement item above; it is the constant to revisit once there is
a real per-document CPU latency figure to divide the foreground budget by.

##### Configuration surface

Enum backends, not ambiguous booleans, because inference and index ownership cut
over at different times:

| Setting | Values | Default today |
| --- | --- | --- |
| `semantic_runtime` | `python` \| `rust` | `rust` |
| `semantic_index` | `chroma` \| `dual` \| `rust` | `rust` |
| `rerank_runtime` | `python` \| `rust` | `rust` (step 6) |
| `clip_runtime` | `python` \| `rust_shadow` \| `rust` | not yet introduced |
| `clip_index` | `chroma` \| `dual` \| `rust` | not yet introduced |

`rerank_runtime` is one lever for two consumers on purpose. Calibration and the
background scorer must not be split across backends, so the switch moves the
reranked query, the scoring worker, the worker status command, and the reranker
availability report together — and it is passed to Python as
`CARBONPAPER_RERANK_RUNTIME`, which is what keeps the Python worker from
starting a second drainer on the same queue.

**Changing `rerank_runtime` takes effect on the Smart Cluster queue only after
the monitor process restarts.** The two sides read it at different times: Rust
reads the registry on every pass, Python reads the environment variable once, at
startup. So the value the running monitor was spawned with is what decides who
drains `smart_cluster_pending`, and Rust honors that rather than the key —
otherwise setting the key back to `rust` under a monitor started with `python`
would wake a second drainer beside a live one, with each deleting queue rows the
other is still working through and the two scoring the same snapshots on
providers the 2026-07-20 audit measured as disagreeing on 20.5% of top-1
results. `MonitorState::python_owns_smart_cluster_queue` is that arbitration;
the worker writes the resulting arrangement to the log whenever it changes,
including the interval where the switch says `python` but the monitor predates
it and nothing is draining at all.

Invalid or unavailable Rust configurations fall back observably for one release,
with a local diagnostic explaining why. `rust_shadow` is retired from
`semantic_runtime` now that nothing can enter shadow mode for MiniLM; CLIP will
introduce and then retire its own.

Search response schemas, filters, offsets, limits, thresholds, MCP tool
availability, and frontend labels are preserved and explicitly tested. OCR
keyword, MiniLM semantic text, CLIP visual and natural-language image search, and
Smart Cluster assignment stay separately labeled, and their scores are never
compared across models.

##### Milestone 2 release gate

**Numeric parity.** Token IDs match exactly for the golden corpus. CPU embedding
cosine is at least 0.99999 with maximum absolute error 0.0001. DirectML embedding
cosine is at least 0.999 with maximum absolute error 0.001, unless a
model-specific reviewed tolerance is documented. Raw reranker logits use the same
CPU and DirectML absolute-error profiles.

**Retrieval equivalence, stated as a recall-superset gate.** The original wording
— "top-10 overlap at least 99%, top-1 effectively unchanged" — assumed the Rust
and Python paths were two implementations of the *same* retrieval method. They
are not: Chroma is approximate (HNSW), the Rust path is an exact cosine scan over
the same hot layer. An exact scan that agreed with an approximate index 99% of
the time at k=10 would be evidence that one of them is wrong, not that both are
right. So:

- **Recall superset.** For each query, every Python top-K result that is present
  in the Rust store must be reachable by the Rust scan at a score no worse than
  Python assigns it. A Python-top result that is *absent* from the Rust store is
  a real coverage defect and blocks the cutover. A Python-top result that is
  present but ranked differently is the expected approximate-versus-exact
  difference. The retired shadow harness reported these separately as
  `only_in_chroma` (blocking) and `in_both_diff_rank` (accepted) precisely so the
  two could not be confused. Since the harness was deleted at step 4, this is an
  **offline gate measured before a cutover lands**; what enforces coverage
  afterwards is the runtime refusal set, not a metric nobody can read anymore.
- **Query-encoder numerics.** Cosine agreement between the Rust and Python query
  encoders, measured against the same stored document vectors, must meet the CPU
  tolerance above. This is the part that catches a genuine Rust inference bug,
  and the part the 2026-07-27 measurement passed at 6.0e-7.
- **Contracts.** Filter, offset, limit, threshold, and JSON response contracts
  still match 100%. Not softened by anything above.
- Overlap@10 and top-1 agreement stay **recorded** as descriptive numbers for the
  bi-encoder step, not pass/fail thresholds. They return to being a pass/fail
  gate at step 6, where reranked end-to-end ordering — what the user actually
  sees on the calibration path — is compared.

**Migration.** Migrated subject-key sets match exactly per index kind.
Unmappable and corrupt rows are listable and keep the legacy backend active.
Existing CLIP vectors are float-copied, not re-encoded, during normal migration.

**Python stopped.** With Python stopped, Rust-owned semantic-text search,
migrated CLIP visual search, and new-capture Rust CLIP and MiniLM indexing
continue to work for every capability marked `rust`. Capabilities still marked
Python-backed are advertised honestly and remain usable through fallback.
**MiniLM reaches this gate at step 5, not step 4** — step 4 may ship with the
capability advertised as Rust-read and Python-written.

**No regressions elsewhere.** Existing automatic classification and task
clustering do not regress: Python BGE remains until its consumer migrates, and
`task_vectors` and `task_centroids` remain available to the Milestone 4 path.

**Lifecycle.** Embedding migration and rebuild are interruptible and resumable.
Session lock and unlock, process crash, partial ANN generation, model upgrade,
rollback to the previous release, and deletion are all tested without screenshot
loss or silent vector loss.

**Foreground isolation.** Background semantic work cannot trigger a model load
during fullscreen or game activity. User-initiated search stays available with a
deadline. OCR p95 latency and reliability show no material regression from
semantic work. Search p95 latency and peak memory stay inside an explicitly
recorded budget.

**Rollback.** The Python fallback remains available for one released version
after each capability becomes Rust-default.

**Scaffolding is gone.** No shadow or probe development surface for an
already-cut-over capability ships in a production build. The settings card, the
probe commands, and the sample tables are deleted, and the only remaining backend
diagnostic is the read-only fallback status the flag rule requires. A beta may
ship a harness; a production release may not.

**Depends on.** Milestone 1's semantics and decisions, landed and reviewed,
including the count-level caveats.

---

### Milestone 3 — Smart Cluster worker in Rust — PLANNED

**Target: post-v0.8.4 Beta.**

**Goal.** Move the remainder of Smart Cluster scoring into Rust.

Smart Cluster — user-controllable, natural-language-anchored, already
Rust-persisted in `storage/smart_cluster.rs` — is a **different system** from
unsupervised task clustering. Do not conflate the two.

**Where the code is.** Persistence and schema are Rust. On `main`, the scoring
worker `monitor/smart_cluster_worker.py` and `monitor/reranker.py` are still
Python and `monitor_smart_cluster_worker_status` forwards to Python; M2.5 step 6
moves the drain, the scoring, the status command, and the force-run and cancel
levers to Rust and leaves the Python path reachable as the `rerank_runtime`
rollback.

**Scope reduced 2026-07-29.** The pending-queue drain and the reranker scoring
move earlier, in M2.5 step 6, because the calibration threshold and the
assignment score have to come from the same scorer and calibration cuts over
there. What remains here is the surface around that scorer.

**Work.**

- Keep the current good behavior intact: idle gate before load, idle re-check
  during batches, manual force-run, reranker unload after a pass, per-cluster
  threshold assignment. *(All carried across in M2.5 step 6; keep them intact.)*
- Port the status command, force-run, and queue plumbing. *(Done in M2.5
  step 6.)*
- Add cheap assignment explainability if practical: prefilter score, rerank
  score, threshold, model id and version. The threshold's scorer identity is
  already stored per cluster as of step 6, so this is now mostly a read.
- Delete `monitor/smart_cluster_worker.py` and the `rerank_runtime = python`
  rollback once the Rust drain has run for a release. Keep the SQLite schema
  unless a migration is clearly needed.

**Release gate.**

- Creation, calibration preview, pending drain, assignment, rescan, and summary
  storage all work without Python.
- Old pending entries are processed or left retryable, never silently dropped.

**Depends on.** Milestone 2 step 6 (Rust reranker and scoring).

---

### Milestone 4 — Task clustering decision — PLANNED

**Target: post-Milestone 3.**

**Goal.** Decide whether HDBSCAN and PaCMAP task clustering deserves to survive
Python removal. **Default stance: it does not.**

**Where the code is.** `monitor/task_clustering.py` (PaCMAP plus scikit-learn
HDBSCAN) with a periodic auto-scheduler, fully Python. The scheduler is
idle-gated (`task_clustering.py:1696`), but there is no Rust replacement.

**Work.**

- Do not port HDBSCAN and PaCMAP unless user value is proven. Prefer simpler
  Rust-owned grouping: session windows, process/title/URL continuity, Rust
  embedding similarity, Smart Cluster assignments, user corrections.
- If unsupervised clustering is still wanted, make it manual or idle-only,
  cancellable, rebuildable, and off the capture and OCR hot path.
- Remove or hide the periodic automatic Python HDBSCAN scheduler.
- Keep existing saved tasks in SQLite with migration and compatibility display.
- Decide the fate of the `task_vectors` hot layer, which after M2.5 step 5 is fed
  only by a best-effort Rust mirror and is not repaired when partially behind.
  This is where a Rust-to-Python vector read would get its second user, and where
  the clustering coverage gap opened by step 5 is closed or accepted.

**Release gate.**

- No dependency on Python HDBSCAN or PaCMAP for capture, OCR, search, or Smart
  Cluster.
- The task view stays useful, and any expensive clustering run is explicitly
  idle-gated or manual.

**Depends on.** Milestone 2, if the simpler grouping uses embedding similarity.

---

### Milestone 5 — Classification and PII resolution — PLANNED

**Target: post-Milestone 4.**

**Goal.** Remove the remaining Python-only machine-learning features, or make
them optional add-ons.

**Where the code is.** Classification (`monitor/classifier.py`, BGE via ONNX with
a torch fallback) runs inside OCR post-process
(`monitor/monitor/worker_process.py`). PII is a **two-tier MCP-output filter**:
tier 1 is Rust aho-corasick dictionary masking (`sensitive_filter.rs`), tier 2 is
Python Presidio NER, default-on (`presidio_enabled: true` at
`sensitive_filter.rs:73`, applied at `mcp_server.rs:825-865`). The Rust rule
layer is already the first line, but only on the MCP read path — capture-time PII
is untouched. `torch`, `spacy`, and `presidio-*` remain in `requirements.txt`.

**Work.**

- Replace Python BGE classification with Rust embedding-based scoring using the
  Milestone 2 engine, or with simple process and title rules, or with
  user-defined Smart Clusters — or remove automatic classification from the
  default experience.
- PII: keep and extend the Rust deterministic rules. Decide Presidio and spaCy's
  fate — add ONNX NER only for a concrete workflow, otherwise make advanced PII
  optional and not part of the default install. Clarify whether PII also applies
  at capture-write time, not only on the MCP read path.
- Remove `torch`, `sentence-transformers`, `hdbscan`, `pacmap`, `spacy`, and
  `presidio-*` from default dependencies once no default feature needs them.
- Audit the UI for now-backendless controls, demote experimental panels, simplify
  wizards.

**Release gate.**

- A default install needs no Python packages for classification, PII, OCR,
  semantic search, or Smart Cluster.
- Advanced toggles reflect what is actually installed, and no UI path starts
  Python implicitly.

**Depends on.** Milestones 2-4.

---

### Milestone 6 — Python-free default build — PLANNED

**Target: later 0.8.x.**

**Goal.** Ship the first default build that neither installs nor starts Python.

**Where the code is.** Unchanged from the original plan — all of it is still
present. `python.rs` provides `request_install_python`, `install_python_venv`,
`install_spacy_model`, and dependency sync. `build.rs` packages and
integrity-checks `monitor.pyz`. The release still bundles the Python installer.

**Work.**

- Remove Python auto-install from first run.
- Stop bundling and copying `python-3.12.10-amd64.exe`, `monitor.pyz`,
  `requirements.txt`, venv freshness checks, the pip sync UI, and the spaCy
  install UI.
- Keep a temporary `python_legacy_monitor` build flag only if needed — off by
  default, out of release packaging.
- Update docs and README: the core is Rust-native, downloads are ONNX assets, no
  Python required. This includes fixing `CLAUDE.md`, which still describes OCR as
  PaddleOCR and still requires a `torch`-before-cv2 import order that the ONNX
  sentinel path no longer performs.
- Upgrade cleanup: detect an old venv, offer deletion after successful
  Rust-native operation, never delete user data.

**Release gate.**

- Fresh install and upgrade both work without Python. Legacy Python files are not
  required for capture, OCR, search, Smart Cluster, settings, MCP, or extension
  capture.

**Depends on.** Milestones 1-5, all default and stable for at least one release.

---

### Milestone 7 — Infrastructure deletion and product simplification — PLANNED

**Target: final 0.8.x cleanup.**

**Goal.** Delete the now-unused integration layers and simplify the product
surface.

**Work.**

- Remove default-build code for the Python launcher, the installer and venv
  manager, the monitor named-pipe server, reverse storage IPC, the Python worker
  supervisor, and Python ChromaDB ownership.
- Remove stale docs and naming: no "Python service handles capture/OCR"; no
  "demo" labels on production Smart Cluster paths (`task_clustering.py` still
  labels the natural-language retrieval section "demo"); no obsolete PaddleOCR
  naming, since the runtime is RapidOCR on ONNX.
- Collapse setup into three things: app auth and storage, model assets, and the
  optional browser extension. Simplify settings into General, Privacy & Security,
  Search & Models, Storage, Extension, and Advanced. Keep advanced diagnostics,
  but off the main path.

**Release gate.**

- Removing Python files removes no user data. Tests and packaging no longer
  reference Python monitor files in default mode.
- The product reads as one Rust-native local memory application, not a stack of
  optional subsystems.

**Depends on.** Milestone 6.

---

## 7. Parallel track: AI agent skill and MCP onboarding

**Status.** Reached the original 0.8.3 target. The standalone package
`carbonpaper-memory` (repo `carbonPaperSkill`) is the committed distribution
shape (`components/settings/agent-access/agentAccessConstants.js`).

**Done.**

- A full agent-setup area: endpoint and connection state, one-click copy of the
  setup prompt, separate token copy, and copy-diagnostics
  (`components/settings/agent-access/`).
- `mcp_get_status` returns `server_version`, `skill.tool_schema_version`, and
  `capabilities`, including `search_nl` availability and the
  `python_monitor_not_running` disabled reason (`commands/mcp.rs`).
- 12 MCP tools exposed. `search_nl` is dropped from the tool list when its
  backend is unavailable (`mcp_server.rs:435`), so capability awareness already
  works.

**Remaining.**

- The original 0.8.4 items: a **settings-page MCP smoke test** (authenticated
  ping, list tools, harmless metadata query, with auth, port, and privacy-filter
  failures reported separately) and per-agent guided setup variants. No such
  command exists today.
- Capability-drift control. As Milestones 2 and 3 move embedding, reranker, and
  Smart Cluster work to Rust, update the skill's capability flags and the
  `search_nl` wording to track its backend — CLIP text-to-image, moving from
  Python to Rust at M2.5 steps 7-9 — so it never advertises a Python-only path as
  stable, while keeping it advertised, since the capability is retained rather
  than removed. Prefer generating and validating the skill's tool table from the
  Rust MCP command definitions.

Do not defer this track to Milestones 6-7. The agent story is already stable;
those milestones should only remove obsolete Python wording.

---

## 8. Per-feature notes

### OCR — done

Shipped via `rapidocr-core` (a pinned crate, not the originally planned local
`rapidocr-rs` path), as a standalone worker with thin CarbonPaper integration:
RGB bytes in, blocks plus timings out.

Retain the original gate list as regression fixtures: empty and black frames,
mixed Chinese/English browser content, code and editor windows, dense documents,
tiny edge text, transparent and alpha content, EXIF-oriented images, and
fullscreen or game capture with no foreground stutter.

### Semantic search

Keep OCR keyword search as the dependable baseline (Rust `search_text`, shipped).
Semantic text search is Rust-owned and rebuildable as of Milestone 2.

There are **three distinct surfaces, kept separate and clearly labeled**: OCR
keyword; semantic text (MiniLM over OCR); and visual or natural-language image
search (Chinese-CLIP text-to-image).

Chinese-CLIP is **retained** — it is `search_nl`'s live backend and the only
text-to-image path. Milestone 2 migrates its existing vectors, dual-writes new
image embeddings, and cuts over text queries only after parity. It must never
create a window where visual search works for old data while new captures stop
being indexed.

"Prefer one path" applies *within* the text modality — do not ship several
redundant text-based natural-language surfaces — not to folding text-to-image
into text-over-OCR.

### Smart Cluster

Preserve the current product model; it is more user-controllable than
unsupervised task clustering. Rust already owns persistence. M2.5 step 6 moves
the reranker and scoring; Milestone 3 moves the surrounding surface. Add
explanation fields before adding more algorithms.

### Task clustering

Treat HDBSCAN and PaCMAP as an experiment that may not survive Python removal
(Milestone 4). Prefer simpler grouping. If kept, make it manual or idle-only and
derive it from SQLite and embedding data, never as a capture dependency.

### PII and Presidio

The Rust deterministic rule layer already exists and runs first on the MCP path.
Extend it. Add ONNX NER only for a concrete workflow, avoid shipping spaCy
transformer models by default, and decide whether PII also belongs at
capture-write time (Milestone 5).

---

## 9. Deletion checklist

### Python components — wait for a shipped replacement

Do not delete a Python component until its Rust replacement has been the default
for at least one release.

| Component | Condition |
| --- | --- |
| Python OCR recognition | **Eligible now.** Rust OCR has been default and Milestone 1 confirmed nothing else depends on it. Remove the dormant engine and its runtime handshake. The rest of `ocr_service.py` still does post-process — remove only the recognition path. |
| Python Chinese-CLIP inference | **Retained capability, not a demotion.** Remove its Python inference only after both Rust CLIP encoders are default for one release, existing vectors are migrated, new captures are Rust-indexed, and the `search_nl` parity and rollback gates pass. The `screenshots` collection is migrated, never silently dropped or routinely re-encoded. |
| Python MiniLM inference and Chroma semantic retrieval | Only after the relevant Milestone 2 capability is stable for one release. Note the three surviving Python MiniLM call sites listed in section 3 — the reranked query encode, the hot-layer rebuild, and the Smart Cluster worker — of which the first and third move at step 6 and the second at Milestone 4. Keep Chroma and any dual-write needed by `task_vectors` and `task_centroids` until the Milestone 4 decision is complete. |
| Python bge-reranker inference | Remove from calibration and from the Smart Cluster scoring path together, after the M2.5 step 6 parity and cutover gates. Step 6 leaves it in place as the `rerank_runtime = python` rollback for one release. Remove the remaining worker surface after Milestone 3. |
| `monitor_smart_cluster_calibrate_preview` | **DELETED.** It was an auth-guarded Tauri command with no caller outside `api_contracts.test.js`: the calibration screen goes through `nlClusterQuery(..., enableRerank = true)`. The command, its Python handler, its security-guard entry, and its test reference were removed together. |
| Python BGE classification inference | Do not remove in Milestone 2 merely because the Rust runtime can load BGE. Remove only when Milestone 5 classification uses Rust directly, or while a deliberately supported Python-to-Rust inference bridge is active and tested. |
| Python Smart Cluster worker | Only after Milestone 3 is stable. |
| Python HDBSCAN and PaCMAP | Only after Milestone 4 is stable. |
| Python classification and PII dependencies | Only after Milestone 5 is stable. |
| Python installer, venv, pyz, reverse IPC | Only after Milestone 6 proves fresh install and upgrade without Python. |

### Migration scaffolding — the opposite clock

Scaffolding is not a component waiting for a replacement. It is temporary
instrumentation, removed as soon as the gate it proves is signed off, and at the
latest before the capability's first production release.

| Scaffolding | Status |
| --- | --- |
| MiniLM shadow toggle, query and document probes, `semantic_shadow_samples`, `semantic_doc_encoder_runs`, the settings card | **DELETED** with the M2.5 step-4 cutover. Tables are dropped in `storage/schema.rs`. |
| The `rust_shadow` value of `semantic_runtime` | **DELETED** — normalizes to the shipped default. |
| The reranker ONNX variant selector (`rerankVariant` argument, `available_variants` dropdown) | **DELETED** with M2.5 step 6. Only `model_uint8.onnx` is installed and the Rust engine pins it, so the selector offered choices that could not load and the `q4f16` default named a file that is never on disk. The loaded variant is still reported. |
| Reranker, CLIP, and BGE shadow harnesses | Not yet built. Same rule applies at their own cutovers: enforce deletion **in the cutover PR**, not as an end-of-milestone sweep. An unowned diagnostic panel never gets deleted later. |

When deleting a shadow harness, check for functions the production path has since
adopted. `minilm_sources` began life as the document-encoder probe's source
builder and is now the capture worker's; deleting it as scaffolding would have
removed production code.

---

## 10. Branching and release discipline

- **This roadmap lives in the repository** (`docs/python-removal-roadmap.md`) so
  milestone status can be reviewed and versioned alongside implementation
  branches. The former parent-directory `roadmap.md` was outside the repository
  and could not be.
- **Build Milestone 2 as short stacked branches**, not one big-bang branch:
  `m2/contracts-and-baseline`, `m2/python-oracle-contracts`,
  `m2/rust-onnx-runtime`, `m2/derived-vector-store`,
  `m2/minilm-dual-write-migration`, `m2/minilm-shadow-cutover`,
  `m2/minilm-query-cutover`, `m2/minilm-capture-indexing`,
  `m2/reranker-shadow-cutover`, `m2/clip-vector-migration`, `m2/clip-cutover`,
  `m2/search-ui-capabilities`, `m2/bge-shadow`. Infrastructure may merge to
  `main` while disabled; each consumer cutover carries its own gate and rollback.
- **Do not open a release branch** until the intended capabilities have passed
  their gates on `main`. Release branches are stabilization-only; do not
  accumulate new migration architecture there.
- **Backend selection is explicit per capability.** Prefer enums
  (`python|rust_shadow|rust`, `chroma|dual|rust`) over one boolean per
  replacement, because inference and index ownership cut over at different times.
  The cautionary precedent: only `rust_ocr_dml_beta` (a DirectML accelerator
  toggle) ever existed for OCR, which shipped default with no `rust_ocr` flag.
  Milestones 2-5 must not repeat that unobservable big-bang cutover.
- **Every flag and replacement gets a telemetry-free local diagnostic** command
  reporting status and last error. The OCR runtime and `mcp_get_status` are the
  model to follow.
- **Every release includes a rollback path for one version.**
- **Every release note says** what moved from Python to Rust, what remains
  Python-backed, what data can be rebuilt, and whether a model re-download is
  required.

---

## 11. Immediate next step

**M2.5 step 5 is merged** (PR #150), together with its three follow-up items:
the battery case is decided and the idle gate now reads AC power directly, a
manual "index now" path exists, and the freshness cost is reported rather than
hidden. That manual path was revised on 2026-08-01 — it drains the queue with a
progress report and a stop button instead of stopping after 128 screenshots, and
the settings card it lives on now names the scope it actually covers rather than
"semantic retrieval" in general. What remains from that step is an on-machine
soak — confirm the backlog figures behave as described across a session lock and
an app restart, which only a real machine can show.

**Step 6 is implemented on `m2/reranker-shadow-cutover`,** carrying the step-5
follow-ups with it. It cuts over Smart Cluster calibration *and* the scoring
worker together, because a persisted threshold cannot be produced by one scorer
and applied by another, and it stores the scorer identity next to every
threshold so a number from a retired scorer is re-derived rather than trusted.

Three things stand between it and DONE, and none of them can be settled by
reading code:

1. **Measure the foreground latency of a reranked calibration query** on a real
   machine, cold and warm, and record it here. The step's gate asks for a
   measured number, and the CPU-only cross-encoder is the reason it might not
   be acceptable.
2. **Compare reranked end-to-end ordering against Python** offline —
   overlap@10 and top-1 agreement return to pass/fail status at this step.
3. **Soak the scoring worker**: confirm that a threshold written by the Python
   scorer is re-derived on the first pass, that a cluster with no usable
   examples is reported rather than silently skipped and is not re-attempted on
   the next pass, and that queue depth and the "run now" and "stop" controls
   behave through an idle window and an app restart.

**Then the CLIP sequence** — steps 7 through 9, in that order, with the
new-capture dual-write (step 8) landing before the text-query cutover (step 9).

**Standing constraints.** No Rust backend becomes authoritative until its Python
behavior contract, dual-write and migration, shadow-query, lifecycle,
performance, and rollback gates pass. Chroma remains for Milestone 4 task
clustering, and Python BGE remains for Milestone 5 classification.

---

## Appendix A — Decision log

Only decisions that changed the plan's direction. The reasoning lives in the
milestone bodies; this is an index so a reversal is not silently re-reversed.

| Date | Decision |
| --- | --- |
| 2026-07-18 | Rebaselined the plan on a source audit of shipped `0.8.3`, superseding the 0.8.1-baseline plan (kept as `roadmap.0.8.1-original.md`). Version numbers stop tracking milestones. |
| 2026-07-19 | Milestone 1 reduced to an internal migration baseline; its per-row ledger and rebuild executor moved to Milestone 2. |
| 2026-07-19 | **Reversed the "demote Chinese-CLIP" decision.** It rested on a dead-code rationale that missed `search_by_text`, the live backend serving `search_nl` from two frontend surfaces and the MCP. CLIP is retained as a first-class search surface. |
| 2026-07-19 | Milestone 2 reframed around behavioral equivalence rather than moving an ONNX call site. ChromaDB cannot be removed while `task_vectors` and `task_centroids` serve Milestone 4; Python BGE cannot be removed before classification has a Rust path. |
| 2026-07-19 | Milestone 2 committed as the scope for v0.8.4 Beta, delivered as short stacked PRs, with disabled or shadow infrastructure allowed to land before individual cutovers. |
| 2026-07-20 | DirectML parity rejected for MiniLM and the uint8 reranker after a five-gate audit failed (top-1 changed on 20.5% of queries). Both are CPU-only in Rust. |
| 2026-07-27 | Step-3 shadow measurement accepted: the query encoder passes numerically, and the retrieval divergence is a recall superset rather than a defect. |
| 2026-07-28 | Step 4 scoped to non-reranked retrieval only; calibration moves to step 6 rather than through a throwaway Rust-retrieve/Python-rerank bridge. |
| 2026-07-28 | The "top-10 overlap ≥ 99%" release gate rewritten as a recall-superset gate, because the two paths use different retrieval methods. |
| 2026-07-28 | Step 5 added: Rust capture-side MiniLM indexing, which the numbered sequence never contained even though the "with Python stopped" gate silently required it. |
| 2026-07-29 | Step 6 enlarged to move the Smart Cluster scoring worker together with the reranker, and Milestone 3 reduced accordingly. |
| 2026-07-29 | Step 5 decisions: Rust owns capture-path encoding with the Chroma mirror reversed; indexing is idle-gated and search freshness regresses; retention becomes first-party; the `index_incomplete` refusal is retired. |
| 2026-07-30 | Document restructured around milestones. Stale "current reality" claims corrected against the tree: the `ml_contracts` traits have method surfaces but no implementations, the MiniLM shadow harness is already deleted, and Python still runs MiniLM outside the capture path. |
| 2026-07-31 | **Step 5's battery analysis reversed on a source read.** `idle.rs` was gating on `!PowerState.active`, not on AC power, so the claim "the worker never runs on battery" held only with power saving enabled — where capture stops too, bounding the backlog — and was false with it disabled, where background inference ran on battery unnoticed. The gate now reads `power::is_ac_power_connected` directly. Battery-only machines index on the next AC session or through the new bounded manual run. |
| 2026-07-31 | Steps 5 and 6 combined onto one branch, `m2/reranker-shadow-cutover`, rather than soaking step 5 separately: step 5 is merged to `main` already, so its follow-ups are ordinary changes rather than a gated cutover. |
| 2026-07-31 | A Smart Cluster threshold from a retired scorer is **re-derived from its stored calibration examples**, not trusted and not discarded. Every threshold now records the model, revision, variant, and provider that produced it; a cluster whose examples can no longer support one is skipped and counted, and the count is shown to the user. |
| 2026-08-01 | A calibration threshold records **the backend that answered the query**, taken from the response, not the one `rerank_runtime` selects. The reranked query falls back to Python for reasons that switch does not know about — an unfinished M2.4 migration, an empty Rust index, an index backend pointing at Chroma — so reading the switch stamped Rust's identity onto Python's logits and made the number trusted instead of repaired. Unknown backend records no provenance at all. |
| 2026-08-01 | A cluster whose threshold **cannot** be re-derived is given up on durably, not retried. The verdict and the scorer it was reached under are stored on the row, so an idle machine stops reloading the 570 MB cross-encoder once a minute to re-reach it; re-saving the examples clears it, and a queue no remaining cluster can score is drained rather than held. |
| 2026-07-31 | The reranker's availability report and the Smart Cluster worker's status, force-run, and cancel commands branch on `rerank_runtime` together with the scorer itself, instead of each moving on its own schedule. One lever, one backend, no UI talking to two workers at once. |
| 2026-07-31 | The ONNX `rerank_variant` selector deleted rather than carried across, since only `model_uint8.onnx` is ever installed and the old `q4f16` default named a file that is never on disk. |
| 2026-08-01 | The scoring pass **commits in small groups instead of at the end of a batch**. A queue entry may only be deleted once every enabled cluster has scored it, and the pass stands down mid-batch whenever the idle gate closes, a foreground query arrives, or a forced drain is cancelled — so deleting at the end put the next pass back at the same queue head, which `peek_smart_cluster_pending_batch` reads `ORDER BY queued_at ASC`. A machine whose idle windows were shorter than a batch takes to score would have repeated the same work indefinitely and never reached a newly captured screenshot. A group is bounded in `(snapshot × cluster)` pairs, which is what costs cross-encoder time, and grouping cannot move a score. |
| 2026-08-01 | **The Python worker that is actually running outranks the `rerank_runtime` key.** Rust reads the registry every pass, Python reads its environment once at spawn, so setting the key back to `rust` under a monitor started with `python` would have started a second drainer beside a live one — both writing assignments and deleting each other's queue rows, on providers that disagree on 20.5% of top-1 results. Ownership is now decided by the value the live monitor was handed, the status, force-run, and cancel commands route by the same rule, and the arrangement is logged whenever it changes — including the interval where a rollback is set but no monitor has restarted to enact it. |
| 2026-08-01 | `semantic_runtime` deliberately does **not** gate the Smart Cluster scoring pass. It selects which runtime answers an NL query; the scoring pass has no Python side to fall back to, since its prefilter vectors live in the Rust derived store and the Python worker only starts when `rerank_runtime` handed it the queue at spawn. Honoring it there would stop the queue being drained by anyone rather than move the work. |
| 2026-08-02 | **The calibration query is bounded per chunk, not per query.** Dividing the step-6 latency figures into the shipped constants showed a 120 s whole-query deadline that the default request already brushed on a four-core machine and exceeded on a two-core one, while the largest picker option exceeded it on the sixteen-core machine the latency was measured on. A total deadline cannot distinguish a slow machine from a stuck worker; each chunk now gets its own allowance under a runaway ceiling, and the result picker is capped at 30 so the document count stops varying twelvefold. |
| 2026-08-02 | **A reranked query reports its phase and can be stopped**, and a stop is not a fallback. Every other reason the Rust path stops serving hands the query to Python; this one must not, because re-running the work somebody just asked to end takes longer than not stopping. The foreground chunk drops from 64 to 8 to give the progress bar and the stop button a usable resolution, which is free because batching was already measured to buy nothing. A query Python answers reports itself as such and hides the button rather than offering one its backend cannot honour. |
| 2026-08-02 | **The foreground lease binds the two passes a user starts, and they wait rather than end.** Reverses the rule that a manual index run and a forced Smart Cluster drain may ignore it because somebody pressed their button. Consent to run is not consent to the cost: with the foreground chunk at 8 there are fourteen inter-chunk gaps per query, and a MiniLM pass landing in one evicts the cross-encoder for a 570 MB reload, while a drain landing in one takes turns at the single slot and halves the query instead of sharing it. Both now stop submitting while a lease is held and resume when it clears, each under its own wait budget, and both run their cross-encoder calls at `Background` so standing down takes one document rather than one whole rerank call. The forced drain's old `Foreground` priority is what let a plain search wait a commit group — about nineteen seconds — for a five-second budget. |
| 2026-08-02 | A rerank that **yielded** during Smart Cluster threshold re-derivation is no longer recorded as a retryable failure. It is not a verdict about the cluster — nothing was asked — and treating it as one left that cluster out of `usable` while the batch went on to score against the rest and then delete the queue entries, so the cluster it never reached would never see those screenshots again. The whole resolution now stops instead, which costs the batch it had not started and nothing else. |
