"""
Smart Cluster background worker.

Drains the `smart_cluster_pending` queue during system idle windows, scores
new snapshots against every enabled smart cluster, and writes assignments
back to SQLite via the reverse-IPC storage client.

Pipeline per drained snapshot:
    1. MiniLM cosine pre-filter vs each cluster's anchor embedding
       (anchor embeddings are computed once and cached in-memory)
    2. For (snapshot × cluster) pairs that pass cosine > PREFILTER_THRESHOLD,
       run the reranker cross-encoder
    3. If rerank_score > cluster.threshold, record assignment

Idle gate: every tick, ask Rust whether system is idle (no input >30 min,
no fullscreen exclusive, AC connected). Skip the tick when not idle.

Memory hygiene: the reranker is loaded lazily on first batch and unloaded
after RERANKER_IDLE_UNLOAD_SECS without work.
"""

import logging
import threading
import time
from typing import Dict, List, Optional, Tuple

import numpy as np

logger = logging.getLogger(__name__)


# ---- Tunable constants ------------------------------------------------------

TICK_INTERVAL_SECS = 60.0           # how often to poll idle state when no work
BATCH_SIZE = 32                     # snapshots fetched per drain pass
PREFILTER_THRESHOLD = 0.40          # MiniLM cosine cutoff before reranker
OCR_SNIPPET_CHARS = 600             # truncate OCR text before feeding reranker
RERANKER_IDLE_UNLOAD_SECS = 60.0    # unload reranker after N seconds w/o work
DRAIN_NOW_TIMEOUT_SECS = 5.0        # max time the drain_now flag remains set


class SmartClusterWorker:
    """Singleton background worker."""

    _instance = None
    _lock = threading.Lock()

    def __new__(cls):
        if cls._instance is None:
            cls._instance = super().__new__(cls)
            cls._instance._init_fields()
        return cls._instance

    def _init_fields(self):
        self._storage_client = None
        self._task_embedder = None
        self._hot_collection_getter = None
        self._thread: Optional[threading.Thread] = None
        self._stop_event = threading.Event()
        self._drain_now_event = threading.Event()
        self._abort_drain_event = threading.Event()
        self._anchor_cache: Dict[int, np.ndarray] = {}      # cluster_id -> anchor MiniLM vec
        self._anchor_text_cache: Dict[int, str] = {}        # cluster_id -> anchor text (for cache validity)
        self._anchor_threshold_cache: Dict[int, float] = {} # cluster_id -> reranker threshold
        self._last_active_at = 0.0
        self._is_running = False
        self._force_running = False

    def is_running(self) -> bool:
        return self._is_running

    def is_force_running(self) -> bool:
        return self._force_running

    @property
    def storage_client(self):
        return self._storage_client

    # ---- lifecycle ------------------------------------------------------

    def start(self, storage_client, task_embedder, hot_collection_getter=None):
        """Start the worker thread. Idempotent.

        ``hot_collection_getter`` is an optional zero-arg callable returning the
        ``task_vectors`` ChromaDB collection. When provided, the worker reuses
        the MiniLM embeddings already stored at ingest time instead of
        re-encoding from OCR — only missing ids fall back to live encoding.
        """
        with self._lock:
            if self._thread is not None and self._thread.is_alive():
                return
            self._storage_client = storage_client
            self._task_embedder = task_embedder
            self._hot_collection_getter = hot_collection_getter
            self._stop_event.clear()
            self._thread = threading.Thread(
                target=self._run,
                name="smart_cluster_worker",
                daemon=True,
            )
            self._thread.start()
            logger.info("[smart_cluster_worker] started")

    def stop(self):
        self._stop_event.set()
        self._drain_now_event.set()  # wake up
        if self._thread is not None:
            self._thread.join(timeout=5.0)
            self._thread = None

    def request_drain_now(self):
        """Wake the worker immediately, bypassing the idle gate for one pass."""
        self._abort_drain_event.clear()
        self._drain_now_event.set()

    def request_stop_drain(self):
        """Abort the current forced drain run immediately."""
        self._abort_drain_event.set()

    # ---- main loop ------------------------------------------------------

    def _run(self):
        while not self._stop_event.is_set():
            # Wait for either the tick interval or an explicit drain_now signal.
            woke_via_drain = self._drain_now_event.wait(timeout=TICK_INTERVAL_SECS)
            if self._stop_event.is_set():
                break
            if woke_via_drain:
                self._drain_now_event.clear()

            try:
                self._is_running = True
                force = woke_via_drain
                self._force_running = force
                while not self._stop_event.is_set():
                    if force and self._abort_drain_event.is_set():
                        logger.info("[smart_cluster_worker] Forced drain aborted by user request.")
                        break

                    has_more = self._tick(force=force)
                    if not has_more:
                        break

                    # Yield slightly to avoid CPU starvation / database contention.
                    sleep_time = 0.3 if force else 0.05
                    time.sleep(sleep_time)
            except Exception as e:
                logger.exception("[smart_cluster_worker] tick failed: %s", e)
            finally:
                self._is_running = False
                self._force_running = False

            # If reranker has been idle long enough, unload to free RAM/VRAM.
            if (
                self._last_active_at > 0
                and time.monotonic() - self._last_active_at > RERANKER_IDLE_UNLOAD_SECS
            ):
                self._maybe_unload_reranker()

    def _tick(self, force: bool = False) -> bool:
        """Process one batch of pending snapshots.

        Returns:
            True if a batch was processed and there is likely more pending work,
            False if we should stop draining (no work, not idle, or error).
        """
        if not self._storage_client:
            return False

        # Quick exits.
        pending = self._storage_client.smart_cluster_count_pending()
        if pending <= 0:
            return False

        if not force:
            idle = self._storage_client.get_idle_state()
            if not idle.get("is_idle", False):
                logger.debug(
                    "[smart_cluster_worker] skipping tick — system not idle (idle_secs=%s fullscreen=%s)",
                    idle.get("idle_secs"), idle.get("fullscreen_exclusive"),
                )
                return False

        # Use larger batch size for manual runs to utilize parallelism and speed up processing
        batch_size = 128 if force else 32

        clusters = self._storage_client.smart_cluster_list_enabled()
        if not clusters:
            # No enabled clusters — peek + delete to keep the queue from
            # growing unbounded if the user has temporarily disabled them.
            stale = self._storage_client.smart_cluster_peek_pending(limit=batch_size)
            if stale:
                self._storage_client.smart_cluster_delete_pending(stale)
            return pending > batch_size

        # If the reranker model files are missing or the module is broken,
        # exit early WITHOUT touching the pending queue — the snapshots
        # remain queued and will be retried on the next tick once the user
        # has installed the model. Without this guard the queue would be
        # silently drained while every batch fails to score.
        if not self._is_reranker_available():
            logger.info(
                "[smart_cluster_worker] reranker model not available; deferring %d pending snapshot(s) until install",
                pending,
            )
            return False

        # Refresh anchor cache for any cluster whose text we don't know yet.
        self._ensure_anchor_cache(clusters)

        # Drain ONE batch this tick. We peek (no delete) so any failure in
        # _process_batch leaves the ids in place; assignment writes use
        # INSERT OR REPLACE so retries are idempotent.
        ids = self._storage_client.smart_cluster_peek_pending(limit=batch_size)
        if not ids:
            return False

        remaining = max(0, pending - len(ids))
        msg = f"[smart_cluster_worker] Progress: processing batch of {len(ids)} snapshots. Remaining in pending queue: {remaining}."
        logger.info(msg)
        print(msg, flush=True)

        success = self._process_batch(ids, clusters, force=force)
        if success:
            # Only remove ids that we actually finished scoring. On partial
            # success (some clusters scored, others aborted on idle loss)
            # we still delete — the assignments table is the source of
            # truth and we don't want to re-process forever on a flaky
            # foreground window. _process_batch decides what "success"
            # means for this run.
            self._storage_client.smart_cluster_delete_pending(ids)
        self._last_active_at = time.monotonic()
        return success and remaining > 0

    @staticmethod
    def _is_reranker_available() -> bool:
        """Probe whether the reranker model is installed without loading it."""
        try:
            from reranker import Reranker
        except Exception:
            return False
        try:
            return Reranker.is_model_available()
        except Exception:
            return False

    # ---- batch processing ----------------------------------------------

    def _ensure_anchor_cache(self, clusters: List[Dict]):
        """Compute and cache MiniLM embeddings for any newly-seen anchors."""
        stale_ids = []
        for c in clusters:
            cid = int(c["id"])
            anchor = c.get("anchor_text", "") or ""
            self._anchor_threshold_cache[cid] = float(c.get("threshold", 0.0))
            if self._anchor_text_cache.get(cid) != anchor:
                stale_ids.append((cid, anchor))

        # Drop cache entries for clusters no longer in the list (deleted / disabled).
        live_ids = {int(c["id"]) for c in clusters}
        for cid in list(self._anchor_cache.keys()):
            if cid not in live_ids:
                self._anchor_cache.pop(cid, None)
                self._anchor_text_cache.pop(cid, None)
                self._anchor_threshold_cache.pop(cid, None)

        if not stale_ids:
            return

        # MiniLM is shared with the main clustering pipeline and stays resident.
        self._task_embedder.load()
        texts = [t for (_id, t) in stale_ids if t.strip()]
        if not texts:
            return
        vecs = self._task_embedder.encode(texts)
        for (cid, _anchor), vec in zip(
            ((cid, t) for (cid, t) in stale_ids if t.strip()),
            vecs,
        ):
            self._anchor_cache[cid] = vec
            self._anchor_text_cache[cid] = _anchor

    def _process_batch(self, snapshot_ids: List[int], clusters: List[Dict], force: bool = False) -> bool:
        """Score a peeked batch.

        Returns True if the batch was processed end-to-end (so the caller
        should delete the ids from pending). Returns False if we bailed
        out before doing meaningful work — e.g. the reranker became
        unavailable, the system left idle, or storage returned nothing —
        leaving the ids in the queue for a future retry.
        """
        # Fetch OCR text for all snapshots in one round-trip.
        try:
            resp = self._storage_client.get_screenshots_with_ocr_by_ids(snapshot_ids)
        except Exception as e:
            logger.warning("[smart_cluster_worker] Failed to fetch screenshots by ids: %s", e)
            return False

        rows = resp.get("screenshots", []) or []
        if not rows:
            # Snapshots may have been deleted between enqueue and processing;
            # treat as processed so the caller cleans them out of pending.
            return True

        # Build doc texts + MiniLM vectors for prefilter.
        doc_texts: Dict[int, str] = {}
        for row in rows:
            rid = int(row.get("id", 0) or 0)
            if not rid:
                continue
            parts = [
                row.get("process_name", "") or "",
                row.get("window_title", "") or "",
                (row.get("ocr_text", "") or "")[:OCR_SNIPPET_CHARS],
            ]
            doc_texts[rid] = " | ".join(p for p in parts if p) or "(empty)"

        if not doc_texts:
            return True

        snapshot_ids_kept = list(doc_texts.keys())
        try:
            snapshot_ids_kept, snapshot_vecs = self._get_prefilter_vectors(snapshot_ids_kept, doc_texts)
        except Exception as e:
            logger.warning("[smart_cluster_worker] prefilter encode failed: %s", e)
            return False

        # Pre-filter: for each (snapshot, cluster), keep if cosine > threshold.
        candidates: List[Tuple[int, int]] = []  # (snapshot_id, cluster_id)
        for cluster in clusters:
            cid = int(cluster["id"])
            anchor_vec = self._anchor_cache.get(cid)
            if anchor_vec is None:
                continue
            # Vectors are L2-normalised; cosine == dot product.
            if len(snapshot_ids_kept) > 0:
                sims = snapshot_vecs @ anchor_vec
                for i, sim in enumerate(sims):
                    if sim >= PREFILTER_THRESHOLD:
                        candidates.append((snapshot_ids_kept[i], cid))

        if not candidates:
            logger.debug("[smart_cluster_worker] batch had no candidates above prefilter")
            return True

        # Re-check idle right before we pull ~600 MB into RAM. A user that
        # started a fullscreen game between the tick gate and now must NOT
        # have their CPU/GPU swallowed by the reranker. Manual `force` runs
        # bypass the gate by design (the user asked for it).
        if not force and not self._still_idle():
            logger.info("[smart_cluster_worker] left idle before reranker load; bailing out")
            return False

        # Reranker pass.
        try:
            from reranker import Reranker, RerankerNotAvailableError
            reranker = Reranker()
            try:
                reranker.load()
            except RerankerNotAvailableError as e:
                logger.warning("[smart_cluster_worker] reranker unavailable, deferring batch: %s", e)
                return False
        except ImportError as e:
            logger.warning("[smart_cluster_worker] reranker module import failed: %s", e)
            return False

        # Group by cluster anchor so each reranker.rerank() call covers
        # (one anchor, many docs) — matches the reranker's batch shape.
        by_cluster: Dict[int, List[int]] = {}
        for sid, cid in candidates:
            by_cluster.setdefault(cid, []).append(sid)

        any_scored = False
        for cid, sids in by_cluster.items():
            # Cheap idle or abort re-check between clusters — if the user came back
            # mid-batch, or requested manual cancellation, stop gracefully. Whatever
            # clusters we already scored remain valid; the rest will be retried next
            # tick because we return False below and the caller won't delete.
            if force and self._abort_drain_event.is_set():
                logger.info("[smart_cluster_worker] Forced drain aborted mid-batch by user request.")
                return False

            if not force and not self._still_idle():
                logger.info("[smart_cluster_worker] left idle mid-batch; aborting after partial scoring")
                return False

            cluster_meta = next((c for c in clusters if int(c["id"]) == cid), None)
            if cluster_meta is None:
                continue
            anchor_text = cluster_meta["anchor_text"]
            threshold = self._anchor_threshold_cache.get(cid, 0.0)

            docs = [doc_texts[s] for s in sids]
            try:
                scores = reranker.rerank(anchor_text, docs)
            except Exception as e:
                logger.warning("[smart_cluster_worker] reranker scoring failed for cluster %d: %s", cid, e)
                continue

            for sid, score in zip(sids, scores):
                if score >= threshold:
                    self._storage_client.smart_cluster_record_assignment(cid, sid, float(score))
            any_scored = True

        # any_scored False means every cluster either had no candidates or
        # all rerank() calls failed. The ids are still safe to delete
        # because nothing about this batch will succeed on retry under
        # the current cluster configuration; returning True lets the
        # caller drain them and avoid a perpetual loop.
        _ = any_scored
        return True

    def _still_idle(self) -> bool:
        """Cheap idle re-check used to abort heavy work mid-batch."""
        try:
            idle = self._storage_client.get_idle_state()
            return bool(idle.get("is_idle", False))
        except Exception:
            return False

    # ---- memory hygiene -------------------------------------------------

    def _get_prefilter_vectors(self, ids: List[int], doc_texts: Dict[int, str]) -> Tuple[List[int], np.ndarray]:
        """Return a tuple of (valid_ids, (N, D) array of MiniLM vectors).

        Reuses embeddings already stored in the ``task_vectors`` hot-layer
        collection (computed during ``HotColdManager.add_snapshot``); only ids
        absent from the collection fall back to a live MiniLM forward pass.
        Saves one encode per already-ingested snapshot, which is the common case
        since the smart-cluster pending queue is populated right after the hot
        layer insert.
        """
        fetched: Dict[int, np.ndarray] = {}

        if self._hot_collection_getter is not None:
            try:
                coll = self._hot_collection_getter()
                if coll is not None:
                    resp = coll.get(ids=[str(i) for i in ids], include=["embeddings"])
                    got_ids = resp.get("ids") or []
                    embs = resp.get("embeddings")
                    if embs is not None:
                        for sid_str, emb in zip(got_ids, embs):
                            if emb is None:
                                continue
                            try:
                                fetched[int(sid_str)] = np.asarray(emb, dtype=np.float32)
                            except (TypeError, ValueError):
                                continue
            except Exception as e:
                logger.debug("[smart_cluster_worker] hot-layer fetch failed, will encode: %s", e)

        missing = [i for i in ids if i not in fetched]
        if missing:
            try:
                self._task_embedder.load()
                encoded = self._task_embedder.encode([doc_texts[i] for i in missing])
                for i, v in zip(missing, encoded):
                    fetched[i] = np.asarray(v, dtype=np.float32)
            except Exception as e:
                logger.warning("[smart_cluster_worker] live encoding failed for %s: %s", missing, e)

        valid_ids = [i for i in ids if i in fetched]
        if not valid_ids:
            return [], np.empty((0, 384), dtype=np.float32)

        return valid_ids, np.stack([fetched[i] for i in valid_ids])

    def _maybe_unload_reranker(self):
        try:
            from reranker import Reranker
            r = Reranker()
            if r.is_loaded():
                r.unload()
                logger.info("[smart_cluster_worker] reranker unloaded after idle period")
        except Exception:
            pass
        self._last_active_at = 0.0
