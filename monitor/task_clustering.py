"""
Long-term task clustering module.

Uses paraphrase-multilingual-MiniLM-L12-v2 to encode OCR text + process metadata,
PaCMAP for dimensionality reduction, and HDBSCAN for density-based clustering.

Architecture:
    Hot Layer (recent 30 days)  — participates in HDBSCAN, re-run periodically.
    Cold Layer (older than 30d) — compressed to cluster centroids, never re-run.

New snapshots are first matched against Hot clusters, then Cold centroids,
and finally marked as unclustered noise awaiting the next HDBSCAN run.
"""

import os
import gc
import json
import time
import base64
import logging
import secrets
import shutil
import threading
import hashlib
import numpy as np
from concurrent.futures import ThreadPoolExecutor
from collection_export import CollectionSnapshotExporter, numeric_first_sort_key
from typing import List, Dict, Any, Optional, Tuple

from clustering_resources import (
    CLUSTERING_ASSIGNMENT_BATCH_SIZE,
    CLUSTERING_SAMPLE_SIZE,
    LOW_MEMORY_CLUSTERING_THRESHOLD,
    MANUAL_CLUSTERING_PROMPT_THRESHOLD,
    memory_status_for_clustering,
)

logger = logging.getLogger(__name__)


class ModelNotAvailableError(Exception):
    """Raised when the MiniLM model files are not downloaded yet."""
    pass


class ManagerBusyError(Exception):
    """Raised when the hot-layer manager lock could not be taken in time.

    Now that ``run_clustering`` no longer holds the manager lock across its
    whole body, that lock is only ever held for a single Chroma call or a lazy
    handle creation, so this should be unreachable. It exists to assert the
    invariant rather than to be recovered from: the Rust mirror reads an
    ``error`` field in the response body as a best-effort miss and moves on,
    which beats parking a named-pipe handler thread for the length of a
    clustering run.
    """
    pass


# ---------------------------------------------------------------------------
# Constants
# ---------------------------------------------------------------------------
EMBEDDING_DIM = 384
# Longest a hot-layer write will wait for the manager lock before reporting
# itself busy. Generous next to the milliseconds a Chroma upsert needs, and
# short next to the minutes a clustering run takes. The longest legitimate
# holds left are in the task-vector export path, which cannot overlap a mirror:
# the migration that drives it holds the Rust maintenance guard, and the index
# worker refuses to run while that guard is held.
MANAGER_LOCK_BUSY_TIMEOUT_SECS = 5.0
HOT_LAYER_DAYS = 30
CENTROID_MATCH_THRESHOLD = 0.55   # cosine similarity threshold for assigning to existing cluster
MIN_CLUSTER_SIZE = 5
MIN_SAMPLES = 3
PACMAP_N_COMPONENTS = 15          # target dims for PaCMAP reduction


# ---------------------------------------------------------------------------
# TaskEmbedder — singleton, loadable / unloadable
# ---------------------------------------------------------------------------

class TaskEmbedder:
    """Singleton for the ONNX MiniLM task-clustering encoder.

    The model is loaded only for a clustering pass and unloaded afterwards.
    Task clustering is still Python-owned, but its model format is fixed to the
    reviewed ONNX artifact; there is no PyTorch runtime fallback.
    """

    _instance = None
    _lock = threading.Lock()

    def __new__(cls):
        if cls._instance is None:
            cls._instance = super().__new__(cls)
            cls._instance._model = None
            cls._instance._tokenizer = None
        return cls._instance

    # ---- lifecycle -------------------------------------------------------

    @staticmethod
    def _model_path() -> str:
        """Return the first configured MiniLM directory with an ONNX file."""
        explicit = os.environ.get("MINILM_MODEL_PATH")
        local_appdata = os.environ.get("LOCALAPPDATA", os.path.expanduser("~"))
        candidates = [
            explicit,
            os.path.join(
                local_appdata,
                "CarbonPaper",
                "models-onnx",
                "paraphrase-multilingual-MiniLM-L12-v2",
            ),
            os.path.join(
                local_appdata,
                "carbonpaper",
                "models-onnx",
                "paraphrase-multilingual-MiniLM-L12-v2",
            ),
            os.path.join(
                local_appdata,
                "CarbonPaper",
                "models",
                "paraphrase-multilingual-MiniLM-L12-v2",
            ),
        ]
        from onnx_utils import get_onnx_model_path

        for candidate in candidates:
            if candidate and (
                get_onnx_model_path(candidate, "model_int8.onnx")
                or get_onnx_model_path(candidate, os.path.join("onnx", "model_quantized.onnx"))
            ):
                return candidate
        return next((candidate for candidate in candidates if candidate), candidates[-1])

    @staticmethod
    def is_model_available() -> bool:
        """Check whether the MiniLM model files exist on disk."""
        model_path = TaskEmbedder._model_path()
        from onnx_utils import get_onnx_model_path

        onnx_file = get_onnx_model_path(model_path, "model_int8.onnx") or get_onnx_model_path(
            model_path, os.path.join("onnx", "model_quantized.onnx")
        )
        required_files = ["config.json", "tokenizer.json"]
        return bool(onnx_file) and all(
            os.path.isfile(os.path.join(model_path, filename)) for filename in required_files
        )

    def is_loaded(self) -> bool:
        return self._model is not None

    def load(self):
        """Load model & tokenizer (idempotent)."""
        if self._model is not None:
            return

        with self._lock:
            if self._model is not None:
                return

            model_path = self._model_path()
            from onnx_utils import get_onnx_model_path, create_onnx_session
            from logging_config import log_model_loading
            onnx_file = get_onnx_model_path(model_path, "model_int8.onnx") or get_onnx_model_path(
                model_path, os.path.join("onnx", "model_quantized.onnx")
            )
            if not onnx_file:
                raise ModelNotAvailableError(
                    f"MiniLM ONNX model is missing from {model_path}"
                )
            log_model_loading("MiniLM-L12-v2 (ONNX)")
            logger.info("Loading MiniLM-L12-v2 from ONNX: %s ...", onnx_file)
            from numpy_tokenizer import NumpyTokenizer

            self._tokenizer = NumpyTokenizer(model_path)
            self._model = create_onnx_session(onnx_file)
            logger.info("MiniLM-L12-v2 loaded successfully via ONNX")

    def _acquire_runtime(self, attempts: int = 3):
        """Return a consistent ``(model, tokenizer)`` snapshot.

        The three pieces are read together under ``_lock`` so a concurrent
        :meth:`unload` cannot null one of them between the reads. The caller
        then runs its forward pass against these local references: an unload
        landing mid-pass drops the singleton's handles while the objects
        themselves stay alive until that pass returns.

        The snapshot is what lets ``run_clustering`` keep unloading the model in
        its ``finally``, worth ~479 MB resident on the ONNX backend, while no
        longer holding the manager lock across the whole run. Without this
        snapshot, an interleaved unload could turn the third attribute read
        into ``'NoneType' object has no attribute 'get_inputs'``.
        """
        for _ in range(max(1, attempts)):
            self.load()
            with self._lock:
                # `load` assigns the tokenizer before the model, so a non-None
                # model implies a usable tokenizer.
                if self._model is not None:
                    return self._model, self._tokenizer
        raise ModelNotAvailableError(
            "MiniLM was unloaded repeatedly while a caller was trying to use it"
        )

    def unload(self):
        """Release model & tokenizer to free memory."""
        with self._lock:
            self._model = None
            self._tokenizer = None
        gc.collect()
        logger.info("MiniLM-L12-v2 unloaded — memory released")

    # ---- encoding --------------------------------------------------------

    def encode(self, texts: List[str]) -> np.ndarray:
        """Batch-encode texts → (N, 384) L2-normalised numpy array."""
        # One consistent snapshot, then a forward pass on local references only:
        # a concurrent unload() must not be able to null the model out from
        # under a pass that has already started. See _acquire_runtime.
        model, tokenizer = self._acquire_runtime()
        encoded = tokenizer(
            texts,
            padding=True,
            truncation=True,
            max_length=256,
            return_tensors="np",
        )
        from onnx_utils import build_transformer_inputs

        inputs = build_transformer_inputs(model, encoded)
        token_embeddings = model.run(None, inputs)[0]
        attention_mask = encoded["attention_mask"]
        input_mask_expanded = np.expand_dims(attention_mask, axis=-1).astype(np.float32)
        sum_embeddings = np.sum(token_embeddings * input_mask_expanded, axis=1)
        sum_mask = np.clip(np.sum(input_mask_expanded, axis=1), a_min=1e-9, a_max=None)
        emb = sum_embeddings / sum_mask
        norm = np.linalg.norm(emb, axis=1, keepdims=True)
        return emb / np.clip(norm, a_min=1e-9, a_max=None)

    def encode_single(self, text: str) -> np.ndarray:
        """Encode one text → (384,) vector."""
        return self.encode([text])[0]


# ---------------------------------------------------------------------------
# Helper: build combined text for embedding
# ---------------------------------------------------------------------------

def build_task_text(process_name: str, window_title: str, ocr_text: str, max_ocr_len: int = 200) -> str:
    """Combine process + title + OCR snippet into a single embedding input."""
    parts = []
    if process_name:
        parts.append(process_name)
    if window_title:
        parts.append(window_title)
    if ocr_text:
        snippet = ocr_text[:max_ocr_len].strip()
        if snippet:
            parts.append(snippet)
    return " | ".join(parts) if parts else ""


# ---------------------------------------------------------------------------
# ClusteringEngine
# ---------------------------------------------------------------------------

class ClusteringEngine:
    """Run PaCMAP + HDBSCAN on a set of embedding vectors."""

    def _build_result(
        self,
        vectors: np.ndarray,
        ids: List[str],
        metadatas: List[Dict[str, Any]],
        labels: np.ndarray,
    ) -> Dict[str, Any]:
        N = len(vectors)
        unique_labels = set(labels)
        clusters = []
        noise_ids = []

        for sid, lbl in zip(ids, labels):
            if lbl == -1:
                noise_ids.append(sid)

        for cid in sorted(unique_labels - {-1}):
            mask = labels == cid
            cluster_vectors = vectors[mask]
            cluster_ids = [ids[i] for i in range(N) if mask[i]]
            cluster_metas = [metadatas[i] for i in range(N) if mask[i]]

            centroid = cluster_vectors.mean(axis=0)
            centroid = centroid / (np.linalg.norm(centroid) + 1e-9)

            timestamps = [m.get("timestamp", 0) for m in cluster_metas]
            processes = [m.get("process_name", "") for m in cluster_metas]
            categories = [m.get("category", "") for m in cluster_metas]

            def _dominant(items):
                from collections import Counter
                filtered = [x for x in items if x]
                if not filtered:
                    return ""
                return Counter(filtered).most_common(1)[0][0]

            clusters.append({
                "cluster_id": int(cid),
                "centroid": centroid,
                "snapshot_ids": cluster_ids,
                "start_time": float(min(timestamps)) if timestamps else 0.0,
                "end_time": float(max(timestamps)) if timestamps else 0.0,
                "snapshot_count": len(cluster_ids),
                "dominant_process": _dominant(processes),
                "dominant_category": _dominant(categories),
            })

        return {
            "clusters": clusters,
            "noise_ids": noise_ids,
            "labels": labels,
        }

    def run(
        self,
        vectors: np.ndarray,
        ids: List[str],
        metadatas: List[Dict[str, Any]],
        min_cluster_size: int = MIN_CLUSTER_SIZE,
        min_samples: int = MIN_SAMPLES,
    ) -> Dict[str, Any]:
        """Execute the clustering pipeline.

        Args:
            vectors: (N, D) array of L2-normalised embeddings.
            ids: snapshot ids corresponding to each row.
            metadatas: per-row metadata dicts (must contain 'timestamp').
            min_cluster_size: HDBSCAN param.
            min_samples: HDBSCAN min_samples param.

        Returns:
            {
                "clusters": [
                    {
                        "cluster_id": int,
                        "centroid": ndarray(D,),
                        "snapshot_ids": [...],
                        "start_time": float,
                        "end_time": float,
                        "snapshot_count": int,
                        "dominant_process": str,
                        "dominant_category": str,
                    },
                    ...
                ],
                "noise_ids": [...],
                "labels": ndarray(N,),
            }
        """
        N = len(vectors)
        if N < min_cluster_size:
            logger.info("Too few vectors (%d) for clustering (min_cluster_size=%d)", N, min_cluster_size)
            return {"clusters": [], "noise_ids": list(ids), "labels": np.full(N, -1)}

        logger.info("Clustering %d vectors: PaCMAP(%d→%d) + HDBSCAN(min_cluster=%d, min_samples=%d)",
                     N, vectors.shape[1], min(PACMAP_N_COMPONENTS, N - 1), min_cluster_size, min_samples)

        t0 = time.perf_counter()

        # ---- PaCMAP dimensionality reduction ----
        n_components = min(PACMAP_N_COMPONENTS, N - 1)
        try:
            import pacmap
            reducer = pacmap.PaCMAP(n_components=n_components, n_neighbors=None, MN_ratio=0.5, FP_ratio=2.0)
            reduced = reducer.fit_transform(vectors)
        except Exception as e:
            logger.warning("PaCMAP failed (%s), falling back to raw vectors", e)
            reduced = vectors

        # ---- HDBSCAN clustering ----
        from sklearn.cluster import HDBSCAN as SklearnHDBSCAN

        clusterer = SklearnHDBSCAN(
            min_cluster_size=min_cluster_size,
            min_samples=min_samples,
            metric="euclidean",
            cluster_selection_method="eom",
        )
        labels = clusterer.fit_predict(reduced)

        elapsed = time.perf_counter() - t0
        unique_labels = set(labels)
        n_clusters = len(unique_labels - {-1})
        n_noise = int((labels == -1).sum())
        logger.info("Clustering done in %.2fs: %d clusters, %d noise points", elapsed, n_clusters, n_noise)

        return self._build_result(vectors, ids, metadatas, labels)

    def run_sampled_assignment(
        self,
        vectors: np.ndarray,
        ids: List[str],
        metadatas: List[Dict[str, Any]],
        min_cluster_size: int = MIN_CLUSTER_SIZE,
        min_samples: int = MIN_SAMPLES,
        sample_size: int = CLUSTERING_SAMPLE_SIZE,
        assignment_batch_size: int = CLUSTERING_ASSIGNMENT_BATCH_SIZE,
        assignment_threshold: float = CENTROID_MATCH_THRESHOLD,
    ) -> Dict[str, Any]:
        """Approximate low-memory clustering.

        Runs the expensive global reducer/clusterer on a deterministic
        time-stratified sample, then assigns the remaining vectors to the
        nearest sampled centroid in bounded batches.
        """
        N = len(vectors)
        if N <= sample_size:
            result = self.run(vectors, ids, metadatas, min_cluster_size, min_samples)
            result["degraded"] = False
            return result

        sample_size = max(min_cluster_size, min(int(sample_size), N))
        assignment_batch_size = max(1, int(assignment_batch_size))
        order = sorted(range(N), key=lambda i: (float(metadatas[i].get("timestamp", 0) or 0), ids[i]))
        positions = np.linspace(0, N - 1, sample_size, dtype=np.int64)
        sample_indices = sorted({order[int(pos)] for pos in positions})
        sample_set = set(sample_indices)

        logger.info(
            "Approximate clustering: sample=%d of %d, assignment_batch=%d",
            len(sample_indices),
            N,
            assignment_batch_size,
        )

        sample_vectors = vectors[sample_indices]
        sample_ids = [ids[i] for i in sample_indices]
        sample_metas = [metadatas[i] for i in sample_indices]
        sample_result = self.run(sample_vectors, sample_ids, sample_metas, min_cluster_size, min_samples)

        labels = np.full(N, -1, dtype=np.int64)
        for local_i, global_i in enumerate(sample_indices):
            labels[global_i] = int(sample_result["labels"][local_i])

        clusters = sample_result.get("clusters", [])
        if not clusters:
            result = self._build_result(vectors, ids, metadatas, labels)
            result.update({
                "degraded": True,
                "degrade_mode": "sample_assign",
                "sample_size": len(sample_indices),
                "assigned_count": 0,
            })
            return result

        cluster_ids = np.array([int(cl["cluster_id"]) for cl in clusters], dtype=np.int64)
        centroids = np.vstack([cl["centroid"] for cl in clusters]).astype(np.float32)
        centroids = centroids / (np.linalg.norm(centroids, axis=1, keepdims=True) + 1e-9)
        unsampled = [i for i in range(N) if i not in sample_set]
        assigned = 0

        for start in range(0, len(unsampled), assignment_batch_size):
            batch_indices = unsampled[start:start + assignment_batch_size]
            batch = vectors[batch_indices]
            sims = batch @ centroids.T
            best_pos = np.argmax(sims, axis=1)
            best_scores = sims[np.arange(len(batch_indices)), best_pos]
            for row_i, score in enumerate(best_scores):
                if float(score) >= assignment_threshold:
                    labels[batch_indices[row_i]] = int(cluster_ids[int(best_pos[row_i])])
                    assigned += 1

        result = self._build_result(vectors, ids, metadatas, labels)
        result.update({
            "degraded": True,
            "degrade_mode": "sample_assign",
            "sample_size": len(sample_indices),
            "assigned_count": assigned,
            "assignment_threshold": assignment_threshold,
        })
        return result


# ---------------------------------------------------------------------------
# HotColdManager — orchestrates the two-layer vector store
# ---------------------------------------------------------------------------

class HotColdManager:
    """Manages Hot / Cold layer lifecycle in ChromaDB.

    Hot layer: ``task_vectors`` collection  (recent snapshots, full vectors).
    Cold layer: ``task_centroids`` collection (archived cluster centroids).
    """

    def __init__(self, chroma_client, storage_client=None):
        self._client = chroma_client
        self._storage_client = storage_client
        self._embedder = TaskEmbedder()
        self._engine = ClusteringEngine()
        # The snapshot mechanics live in `collection_export`, shared with the
        # CLIP image collection's own migration. The hot layer is keyed by
        # `str(screenshot_id)`, so it orders numerically.
        self._task_vector_exporter = CollectionSnapshotExporter(
            namespace="minilm",
            collection_getter=lambda: self.hot_collection,
            dimensions=EMBEDDING_DIM,
            sort_key=numeric_first_sort_key,
            thread_name_prefix="task-vector-export",
        )
        # Guards Chroma collection access and the lazy collection handles, and
        # nothing else. Held only for the length of one Chroma call, because
        # every caller that reaches it may be a named-pipe handler thread and a
        # parked handler costs a slot out of the IPC server's pool of 8.
        # Still an RLock: the collection properties re-acquire it beneath
        # callers that hold it.
        self._lock = threading.RLock()
        # Separate, and deliberately not the manager lock: mutual exclusion
        # between clustering runs only. Two concurrent HDBSCAN runs would
        # double peak memory on a machine already checked for low memory, and
        # the scheduler's plain `_running` bool has a check-then-set race
        # between a scheduled run and a manual one.
        self._clustering_lock = threading.Lock()

        logger.info("[task_clustering] HotColdManager ready (lazy loading collections)")

    @property
    def embedder(self):
        return self._embedder

    @property
    def hot_collection(self):
        if self._client is None:
            return None
        with self._lock:
            if not hasattr(self, "_hot_collection"):
                self._hot_collection = self._client.get_or_create_collection(
                    name="task_vectors",
                    metadata={"hnsw:space": "cosine"},
                )
            return self._hot_collection

    @property
    def cold_collection(self):
        if self._client is None:
            return None
        with self._lock:
            if not hasattr(self, "_cold_collection"):
                self._cold_collection = self._client.get_or_create_collection(
                    name="task_centroids",
                    metadata={"hnsw:space": "cosine"},
                )
            return self._cold_collection

    def unload_collections(self):
        """Drop the cached collection handles.

        This does **not** free the HNSW indexes, despite what its previous name
        and comment claimed. Measured on chromadb 1.5.1 against this project's
        own 10,605-vector hot layer (2026-07-30): dropping the handles releases
        0.0 MB, and the next query still answers in 1.7 ms against a 24 ms cold
        load, so the index never left memory.

        Two reasons. The index lives in the Rust core's cache
        (``chroma_api_impl`` defaults to ``chromadb.api.rust.RustBindingsAPI``),
        sized ``_getmaxstdio() // 5`` = 102 collections, which this database's
        two collections never come close to filling, so nothing is ever
        evicted. And the ``_client._collections`` pop this method used to
        attempt was dead code: 1.5.1's client has no such attribute, so the
        ``hasattr`` guard was always False. Only dropping the whole client
        returns the memory (223 MB for both collections, ``_server.stop()``),
        and that client is shared with the CLIP ``screenshots`` collection, so
        a clustering run has no business dropping it.

        What remains is still worth doing and cheap: the next access rebuilds a
        fresh handle. Keep the lock, which makes the delete atomic against the
        lazy initialisation in the collection properties.
        """
        with self._lock:
            if hasattr(self, "_hot_collection"):
                delattr(self, "_hot_collection")
            if hasattr(self, "_cold_collection"):
                delattr(self, "_cold_collection")

    # ---- encrypt / decrypt helpers (mirror VectorStore pattern) ----------

    def _encrypt(self, text: str) -> str:
        if self._storage_client and text:
            enc = self._storage_client.encrypt_for_chromadb(text)
            if enc:
                return enc
        return text

    def _decrypt(self, text: str, *, background: bool = False) -> str:
        if self._storage_client and text:
            if text.startswith("ENC2:") or text.startswith("ENC:"):
                dec = (
                    self._storage_client.decrypt_from_chromadb_silent(text)
                    if background
                    else self._storage_client.decrypt_from_chromadb(text)
                )
                if dec is not None:
                    return dec
        return text

    # ---- Hot layer operations --------------------------------------------

    # ---- task_vectors snapshot export (M2.4 migration) -------------------
    #
    # Four thin forwards to the shared exporter. They stay methods on this
    # class because the IPC dispatcher addresses the manager, and because the
    # collection handle they snapshot is this class's to resolve.

    def start_task_vectors_export(self, export_id: str) -> Dict[str, Any]:
        """Start a persistent ID snapshot without blocking the IPC worker."""
        return self._task_vector_exporter.start(export_id)

    def get_task_vectors_export_status(self, export_id: str) -> Dict[str, Any]:
        return self._task_vector_exporter.status(export_id)

    def export_task_vectors_page(
        self,
        export_id: str,
        cursor: int = 0,
        limit: int = 128,
    ) -> Dict[str, Any]:
        return self._task_vector_exporter.page(export_id, cursor, limit)

    def finish_task_vectors_export(self, export_id: str) -> bool:
        """Release memory and persistent artifacts after a completed migration."""
        return self._task_vector_exporter.finish(export_id)


    def upsert_task_vectors(self, records: List[Dict[str, Any]]) -> int:
        """Write Rust-generated MiniLM vectors to the authoritative hot layer."""
        if not isinstance(records, list) or not records:
            return 0
        if len(records) > 128:
            raise ValueError("task vector upsert batch exceeds 128 records")

        ids, embeddings, metadatas, documents = [], [], [], []
        for record in records:
            doc_id = str(record.get("id", ""))
            if not doc_id.isdigit() or int(doc_id) <= 0 or str(int(doc_id)) != doc_id:
                raise ValueError(f"invalid task vector id: {doc_id!r}")
            vector = np.asarray(record.get("embedding", []), dtype=np.float32)
            if vector.shape != (EMBEDDING_DIM,) or not np.isfinite(vector).all():
                raise ValueError(f"invalid task vector for id {doc_id}")
            timestamp = float(record.get("timestamp", 0) or 0)
            if timestamp > 1e12:
                timestamp /= 1000.0
            process_name = str(record.get("process_name", "") or "")
            window_title = str(record.get("window_title", "") or "")
            category = str(record.get("category", "") or "")
            document = str(record.get("document", "") or "")
            ids.append(doc_id)
            embeddings.append(vector.tolist())
            metadatas.append({
                "screenshot_id": int(doc_id),
                "timestamp": timestamp,
                "process_name": self._encrypt(process_name) if process_name else "",
                "window_title": self._encrypt(window_title) if window_title else "",
                "category": category,
                "layer": "hot",
            })
            documents.append(self._encrypt(document))

        # Bounded acquisition, on purpose. This method runs inside a named-pipe
        # handler thread, and the pool has 8 slots: parking here for the length
        # of a clustering run drains the pool and takes `status`, `search_nl`
        # and the OCR post-process enqueue down with it. Now that the manager
        # lock is only held for single Chroma calls this can never fire, which
        # is exactly what makes it a useful assertion. Note the encryption
        # above stays outside the lock — it is a reverse-IPC round trip per
        # field, and the lock must never wait on the pipe.
        if not self._lock.acquire(timeout=MANAGER_LOCK_BUSY_TIMEOUT_SECS):
            raise ManagerBusyError(
                "hot layer busy: manager lock held for more than "
                f"{MANAGER_LOCK_BUSY_TIMEOUT_SECS:g}s"
            )
        try:
            self.hot_collection.upsert(
                ids=ids,
                embeddings=embeddings,
                metadatas=metadatas,
                documents=documents,
            )
        finally:
            self._lock.release()
        return len(ids)

    # M2.5 step 5 removed `_dual_write_rust` and `add_snapshot` from this class.
    # Python no longer runs MiniLM on the capture path: Rust encodes each new
    # screenshot on its idle-gated worker, commits the vector to its own store,
    # and hands the finished row back through `upsert_task_vectors` above. The
    # Smart Cluster pending enqueue moved with the encoder for the same reason —
    # it belongs next to whoever wrote the vector the prefilter reads.
    #
    # What is left here is the hot layer as a *consumer*: clustering reads it,
    # `compress_to_cold` ages it, and `_backfill_from_screenshots` still rebuilds
    # it from SQLite when it is found empty.

    def get_hot_vectors(self, days: int = HOT_LAYER_DAYS) -> Tuple[np.ndarray, List[str], List[Dict]]:
        """Retrieve hot-layer vectors within the time window.

        Returns (vectors, ids, metadatas).
        """
        cutoff = time.time() - days * 86400
        # ChromaDB where filter
        results = self.hot_collection.get(
            where={"timestamp": {"$gte": cutoff}},
            include=["embeddings", "metadatas"],
        )

        if not results["ids"]:
            return np.empty((0, EMBEDDING_DIM)), [], []

        vectors = np.array(results["embeddings"], dtype=np.float32)
        ids = results["ids"]
        metas = results["metadatas"]
        return vectors, ids, metas

    def get_all_hot_vectors(self) -> Tuple[np.ndarray, List[str], List[Dict]]:
        """Retrieve ALL hot-layer vectors (for manual range clustering)."""
        results = self.hot_collection.get(
            include=["embeddings", "metadatas"],
        )
        if not results["ids"]:
            return np.empty((0, EMBEDDING_DIM)), [], []
        vectors = np.array(results["embeddings"], dtype=np.float32)
        return vectors, results["ids"], results["metadatas"]

    def get_hot_vectors_in_range(self, start_time: float, end_time: float) -> Tuple[np.ndarray, List[str], List[Dict]]:
        """Retrieve hot-layer vectors within a specific time range."""
        results = self.hot_collection.get(
            where={
                "$and": [
                    {"timestamp": {"$gte": start_time}},
                    {"timestamp": {"$lte": end_time}},
                ]
            },
            include=["embeddings", "metadatas"],
        )
        if not results["ids"]:
            return np.empty((0, EMBEDDING_DIM)), [], []
        vectors = np.array(results["embeddings"], dtype=np.float32)
        return vectors, results["ids"], results["metadatas"]

    def estimate_clustering_inputs(
        self,
        start_time: Optional[float] = None,
        end_time: Optional[float] = None,
    ) -> Dict[str, Any]:
        """Estimate input count and memory pressure before expensive work."""
        count: Optional[int] = None
        source = "unknown"

        if self._storage_client:
            start_s = start_time if start_time is not None else time.time() - HOT_LAYER_DAYS * 86400
            end_s = end_time if end_time is not None else time.time()
            try:
                first_page = self._storage_client.list_screenshots_for_clustering(
                    start_ts=start_s,
                    end_ts=end_s,
                    offset=0,
                    limit=1,
                )
                payload = first_page.get("data", first_page)
                if not (first_page.get("status") == "error" or first_page.get("error")):
                    count = int(payload.get("total", 0) or 0)
                    source = "storage"
            except Exception as e:
                logger.debug("[task_clustering] input estimate via storage failed: %s", e)

        if count is None:
            try:
                if start_time is not None and end_time is not None:
                    raw = self.hot_collection.get(
                        where={
                            "$and": [
                                {"timestamp": {"$gte": start_time}},
                                {"timestamp": {"$lte": end_time}},
                            ]
                        },
                        include=[],
                    )
                    count = len(raw.get("ids", []) or [])
                    source = "hot_collection_range"
                else:
                    count = int(self.hot_collection.count())
                    source = "hot_collection"
            except Exception as e:
                logger.debug("[task_clustering] input estimate via hot collection failed: %s", e)
                count = 0

        memory = memory_status_for_clustering(count)
        return {
            "count": count,
            "source": source,
            "memory": memory,
            "manual_prompt_threshold": MANUAL_CLUSTERING_PROMPT_THRESHOLD,
            "low_memory_threshold": LOW_MEMORY_CLUSTERING_THRESHOLD,
        }

    # ---- Cold layer operations -------------------------------------------

    def compress_to_cold(self, clusters: List[Dict[str, Any]]):
        """Archive cluster centroids to cold layer; remove old hot vectors."""
        cutoff = time.time() - HOT_LAYER_DAYS * 86400

        for cl in clusters:
            centroid = cl["centroid"]
            cid = f"cold_cluster_{cl['cluster_id']}_{int(cl['start_time'])}"

            meta = {
                "cluster_id": cl["cluster_id"],
                "start_time": cl["start_time"],
                "end_time": cl["end_time"],
                "snapshot_count": cl["snapshot_count"],
                "dominant_process": self._encrypt(cl.get("dominant_process", "")),
                "dominant_category": cl.get("dominant_category", ""),
                "layer": "cold",
            }

            # Only archive clusters whose end_time is before cutoff
            if cl["end_time"] < cutoff:
                try:
                    self.cold_collection.upsert(
                        ids=[cid],
                        embeddings=[centroid.tolist()],
                        metadatas=[meta],
                    )
                except Exception as e:
                    logger.warning("Failed to archive cluster %s to cold: %s", cid, e)

        # Remove expired hot vectors. Only this collection's own rows: since
        # M2.5 step 5 the Rust semantic store keeps its own 30-day window
        # against SQLite timestamps, so the deletions no longer have to be
        # mirrored across. The two stores stop tracking each other.
        #
        # The read and the delete need the manager lock as a pair. They used to
        # get it for free from the lock `run_clustering` held across its whole
        # body; that lock is gone, and without this an id that a Rust mirror
        # re-upserts between the two calls would be deleted right back out
        # again. Both calls are single Chroma operations, so the hold is short
        # enough not to reintroduce the pipe-handler stall.
        try:
            with self._lock:
                expired = self.hot_collection.get(
                    where={"timestamp": {"$lt": cutoff}},
                )
                if expired["ids"]:
                    self.hot_collection.delete(ids=expired["ids"])
                    logger.info("Removed %d expired vectors from hot layer", len(expired["ids"]))
        except Exception as e:
            logger.warning("Failed to clean expired hot vectors: %s", e)

    def match_to_existing(self, vector: np.ndarray) -> Optional[Dict[str, Any]]:
        """Try to match a new vector against hot clusters then cold centroids.

        Returns best match metadata or None.
        """
        # Try cold centroids first (broader scope)
        cold_count = self.cold_collection.count()
        if cold_count > 0:
            try:
                results = self.cold_collection.query(
                    query_embeddings=[vector.tolist()],
                    n_results=1,
                    include=["metadatas", "distances"],
                )
                if results["distances"][0]:
                    # ChromaDB cosine distance = 1 - cosine_sim
                    cosine_sim = 1.0 - results["distances"][0][0]
                    if cosine_sim >= CENTROID_MATCH_THRESHOLD:
                        meta = results["metadatas"][0][0]
                        return {
                            "matched_layer": "cold",
                            "cosine_similarity": cosine_sim,
                            **meta,
                        }
            except Exception as e:
                logger.debug("Cold centroid match failed: %s", e)

        return None

    # ---- Backfill from screenshot_embeddings ------------------------------

    def _backfill_from_screenshots(self, start_time: Optional[float] = None, end_time: Optional[float] = None) -> int:
        """Read historical screenshots from SQLite (via Rust reverse IPC) and
        encode them into the hot layer so that old data participates in clustering.

        Returns the number of snapshots added.
        """
        if not self._storage_client:
            logger.warning("Backfill skipped: no storage client available")
            return 0

        PAGE = 500
        added = 0
        offset = 0

        # start_time / end_time are in seconds (Unix epoch), same as Rust expects
        start_s = start_time if start_time else 0.0
        end_s = end_time if end_time else 0.0

        # First call to get total count
        try:
            first_page = self._storage_client.list_screenshots_for_clustering(
                start_ts=start_s, end_ts=end_s, offset=0, limit=1,
            )
            # storage_client returns errors as {'status': 'error', 'error': '...'} (no exception)
            if first_page.get("status") == "error" or first_page.get("error"):
                logger.warning("Backfill query failed: %s", first_page.get("error", first_page))
                return 0
            # Rust wraps response in {"status": "success", "data": {...}}
            payload = first_page.get("data", first_page)
            total = payload.get("total", 0)
        except Exception as e:
            logger.warning("Cannot query SQLite for backfill: %s", e)
            return 0

        if total == 0:
            logger.warning("Backfill: no screenshots found in SQLite (start=%.0f end=%.0f)", start_s, end_s)
            return 0

        logger.warning("Backfilling hot layer from SQLite (%d screenshots) …", total)

        while offset < total:
            try:
                page = self._storage_client.list_screenshots_for_clustering(
                    start_ts=start_s, end_ts=end_s, offset=offset, limit=PAGE,
                )
                if page.get("status") == "error" or page.get("error"):
                    logger.warning("Backfill page fetch error at offset %d: %s", offset, page.get("error"))
                    break
                # Unwrap 'data' envelope
                page = page.get("data", page)
            except Exception as e:
                logger.warning("Backfill page fetch failed at offset %d: %s", offset, e)
                break

            screenshots = page.get("screenshots", [])
            if not screenshots:
                break

            # Build string IDs and deduplicate
            str_ids = [str(s["id"]) for s in screenshots]
            try:
                existing = self.hot_collection.get(ids=str_ids)
                existing_set = set(existing["ids"]) if existing and existing.get("ids") else set()
            except Exception:
                existing_set = set()

            texts_to_encode = []
            entries = []

            for s in screenshots:
                doc_id = str(s["id"])
                if doc_id in existing_set:
                    continue

                process_name = s.get("process_name", "")
                window_title = s.get("window_title", "")
                ocr_text = s.get("ocr_text", "")
                timestamp = s.get("timestamp", 0)
                category = s.get("category", "")

                combined = build_task_text(process_name, window_title, ocr_text)
                if not combined.strip():
                    continue

                texts_to_encode.append(combined)
                entries.append((doc_id, {
                    "screenshot_id": int(s["id"]),
                    "timestamp": float(timestamp) if timestamp else 0.0,
                    "process_name": self._encrypt(process_name) if process_name else "",
                    "window_title": self._encrypt(window_title) if window_title else "",
                    "category": category,
                    "layer": "hot",
                }))

            if texts_to_encode:
                try:
                    vectors = self._embedder.encode(texts_to_encode)
                    batch_ids = [e[0] for e in entries]
                    batch_metas = [e[1] for e in entries]
                    # upsert, not add: a Rust mirror for one of these ids can now
                    # interleave with the backfill, and `add` would fail the whole
                    # page on the duplicate. That also demotes the dedupe `get`
                    # above from a correctness requirement to an optimisation.
                    self.hot_collection.upsert(
                        ids=batch_ids,
                        embeddings=vectors.tolist(),
                        metadatas=batch_metas,
                    )
                    added += len(batch_ids)
                    logger.info("Backfilled %d/%d (page offset %d)", added, total, offset)
                except Exception as e:
                    logger.warning("Backfill encode/upsert failed at offset %d: %s", offset, e)

            offset += PAGE

        logger.info("Backfill complete: %d snapshots added to hot layer", added)
        return added

    # ---- Full clustering run ---------------------------------------------

    def run_clustering(
        self,
        start_time: Optional[float] = None,
        end_time: Optional[float] = None,
        min_cluster_size: int = MIN_CLUSTER_SIZE,
        min_samples: int = MIN_SAMPLES,
        auto_compress: bool = True,
        clustering_mode: str = "auto",
        manual: bool = False,
        allow_full_low_memory: bool = False,
        background: bool = False,
    ) -> Dict[str, Any]:
        """Execute HDBSCAN clustering on hot-layer vectors.

        Args:
            start_time / end_time: optional range override (for manual runs).
            min_cluster_size / min_samples: HDBSCAN params.
            auto_compress: if True, compress old clusters to cold layer.
            clustering_mode: 'auto' | 'full' | 'batched'. 'batched' means
                approximate sample+assign mode.
            manual: True when triggered from a user action.
            allow_full_low_memory: for automatic runs, bypass low-memory
                downgrade when the advanced setting is enabled.

        Returns clustering results dict.
        """
        # Deliberately NOT the manager lock. That lock guards Chroma access and
        # the lazy collection handles; holding it across this whole body — the
        # input estimate, the vector fetch, the HDBSCAN/PaCMAP engine run, the
        # per-cluster decrypt round trips and the backfill's re-encode — parked
        # every incoming `upsert_task_vectors` mirror inside a named-pipe
        # handler thread for minutes at a time, one slot per idle pass, until
        # the pool of 8 was gone and the whole forward pipe answered "IPC server
        # busy".
        #
        # The invariant restored here used to be spelled out in
        # `_flush_pending_rust_deletes`, which M2.5 step 5 deleted along with
        # the rest of the Python capture path: "IPC happens outside the lock;
        # the manager lock also guards Chroma operations and must not wait on
        # pipe round-trips." It reads the same in both directions — a lock that
        # guards Chroma must not be held across IPC, and IPC must not wait on it.
        #
        # What we give up is that a run clusters the snapshot taken at its start
        # rather than a corpus frozen for its duration. For an unsupervised
        # background job over a 30-day window that is not a loss: a screenshot
        # arriving mid-run is picked up by the next run.
        if not self._clustering_lock.acquire(blocking=False):
            logger.info("Clustering run refused: another run is in progress")
            return {
                "clusters": [],
                "noise_ids": [],
                "n_clusters": 0,
                "n_noise": 0,
                "n_total": 0,
                "status": "already_running",
            }
        try:
            logger.info("Starting clustering run (range=%s–%s) …",
                        start_time or "auto", end_time or "auto")

            clustering_mode = (clustering_mode or "auto").strip().lower()
            if clustering_mode not in {"auto", "full", "batched"}:
                clustering_mode = "auto"

            estimate = self.estimate_clustering_inputs(start_time, end_time)
            estimated_count = int(estimate.get("count", 0) or 0)
            memory_status = estimate.get("memory") or {}

            def _needs_user_choice_response(count: int, memory: Dict[str, Any], source_estimate: Dict[str, Any]) -> Dict[str, Any]:
                reason = "low_memory" if memory.get("low_memory") else "large_range"
                choice_estimate = dict(source_estimate or {})
                choice_estimate["count"] = count
                choice_estimate["memory"] = memory
                return {
                    "clusters": [],
                    "noise_ids": [],
                    "n_clusters": 0,
                    "n_noise": 0,
                    "n_total": count,
                    "status": "needs_user_choice",
                    "reason": reason,
                    "degrade_mode": "sample_assign",
                    "estimate": choice_estimate,
                }

            if (
                manual
                and clustering_mode == "auto"
                and estimated_count >= MANUAL_CLUSTERING_PROMPT_THRESHOLD
            ):
                return _needs_user_choice_response(estimated_count, memory_status, estimate)

            try:
                # Fetch vectors
                if start_time is not None and end_time is not None:
                    vectors, ids, metas = self.get_hot_vectors_in_range(start_time, end_time)
                else:
                    vectors, ids, metas = self.get_hot_vectors()

                # If hot layer is empty, try backfilling from screenshot_embeddings
                if len(ids) == 0:
                    # Automatic clustering must never become a second MiniLM
                    # worker. Rust owns semantic indexing and mirrors completed
                    # vectors into this collection; while that queue is still
                    # catching up, leave the scheduled task for a later pass
                    # instead of loading Python's legacy encoder concurrently.
                    if background:
                        logger.info(
                            "Hot layer empty during scheduled clustering; waiting for Rust semantic indexing"
                        )
                        return {
                            "clusters": [],
                            "noise_ids": [],
                            "n_clusters": 0,
                            "n_noise": 0,
                            "n_total": 0,
                            "status": "waiting_for_index",
                        }
                    logger.warning("Hot layer empty — attempting backfill from SQLite")
                    self._embedder.load()
                    backfilled = self._backfill_from_screenshots(start_time, end_time)
                    if backfilled > 0:
                        # Re-fetch after backfill — use get_all to avoid 30-day cutoff
                        # filtering out old backfilled data
                        if start_time is not None and end_time is not None:
                            vectors, ids, metas = self.get_hot_vectors_in_range(start_time, end_time)
                        else:
                            vectors, ids, metas = self.get_all_hot_vectors()
                    else:
                        logger.warning("Backfill returned 0 snapshots")

                if len(ids) == 0:
                    logger.warning("No vectors in hot layer for clustering (even after backfill)")
                    return {"clusters": [], "noise_ids": [], "status": "empty"}

                n_total = len(ids)
                memory_status = memory_status_for_clustering(n_total)
                if (
                    manual
                    and clustering_mode == "auto"
                    and n_total >= MANUAL_CLUSTERING_PROMPT_THRESHOLD
                ):
                    return _needs_user_choice_response(n_total, memory_status, estimate)

                use_approximate = clustering_mode == "batched"
                if (
                    clustering_mode == "auto"
                    and not manual
                    and n_total >= LOW_MEMORY_CLUSTERING_THRESHOLD
                    and memory_status.get("low_memory")
                    and not allow_full_low_memory
                ):
                    use_approximate = True
                    logger.info(
                        "Scheduled clustering using approximate mode due to low memory: n=%d memory=%s",
                        n_total,
                        memory_status,
                    )

                # Run engine
                if use_approximate:
                    result = self._engine.run_sampled_assignment(
                        vectors=vectors,
                        ids=ids,
                        metadatas=metas,
                        min_cluster_size=min_cluster_size,
                        min_samples=min_samples,
                    )
                else:
                    result = self._engine.run(
                        vectors=vectors,
                        ids=ids,
                        metadatas=metas,
                        min_cluster_size=min_cluster_size,
                        min_samples=min_samples,
                    )

                # Optionally compress to cold
                if auto_compress and result["clusters"]:
                    self.compress_to_cold(result["clusters"])

                # Serialise centroids for JSON/IPC transport
                clusters_serialisable = []
                for cl in result["clusters"]:
                    cl_copy = dict(cl)
                    cl_copy["centroid"] = cl["centroid"].tolist()
                    # Decrypt display fields
                    cl_copy["dominant_process"] = self._decrypt(
                        cl_copy.get("dominant_process", ""),
                        background=background,
                    )
                    clusters_serialisable.append(cl_copy)

                return {
                    "clusters": clusters_serialisable,
                    "noise_ids": result["noise_ids"],
                    "n_clusters": len(result["clusters"]),
                    "n_noise": len(result["noise_ids"]),
                    "n_total": n_total,
                    "status": "success",
                    "degraded": bool(result.get("degraded")),
                    "degrade_mode": result.get("degrade_mode"),
                    "sample_size": result.get("sample_size"),
                    "assigned_count": result.get("assigned_count"),
                    "memory_status": memory_status,
                    "clustering_mode": "batched" if use_approximate else "full",
                }
            finally:
                # Reclaims ~479 MB on the ONNX backend, measured 2026-07-30, so
                # this stays even though the run no longer holds a lock that
                # would keep other users away. TaskEmbedder._acquire_runtime is
                # what makes it safe: an in-flight encode holds its own
                # references and finishes on them.
                self._embedder.unload()
                # Frees no memory — see unload_collections. Kept because the
                # next access should start from a fresh handle.
                self.unload_collections()
        finally:
            self._clustering_lock.release()

    # ---- Scheduled re-run helper -----------------------------------------

    def get_cold_clusters(self) -> List[Dict[str, Any]]:
        """Return all cold-layer cluster summaries (for UI display)."""
        results = self.cold_collection.get(include=["metadatas"])
        out = []
        for meta in results.get("metadatas", []):
            m = dict(meta)
            m["dominant_process"] = self._decrypt(m.get("dominant_process", ""))
            out.append(m)
        return out

# ---------------------------------------------------------------------------
# ClusteringScheduler — compatibility facade for explicit runs
# ---------------------------------------------------------------------------

# Interval presets (seconds)
INTERVAL_PRESETS = {
    "1d": 86400,
    "1w": 604800,
    "1m": 2592000,
    "6m": 15552000,
}
DEFAULT_INTERVAL_KEY = "1w"


class ClusteringScheduler:
    """Compatibility facade for explicit clustering requests.

    Periodic scheduling moved to Rust. This object intentionally creates no
    timer or authentication-monitor thread.
    """

    def __init__(self, manager: HotColdManager, storage_client=None):
        self._manager = manager
        self._storage_client = storage_client
        self._interval_key = DEFAULT_INTERVAL_KEY
        self._interval_secs = INTERVAL_PRESETS[DEFAULT_INTERVAL_KEY]
        self._last_run: float = 0.0
        self._running = False
        self._last_result: Optional[Dict] = None

    def _config_path(self) -> str:
        data_dir = os.environ.get("CARBONPAPER_DATA_DIR")
        if not data_dir:
            local_appdata = os.environ.get("LOCALAPPDATA", os.path.expanduser("~"))
            data_dir = os.path.join(local_appdata, "CarbonPaper", "data")
        return os.path.join(data_dir, "clustering_config.json")

    def _load_config(self):
        try:
            path = self._config_path()
            if os.path.exists(path):
                with open(path, "r", encoding="utf-8") as f:
                    cfg = json.load(f)
                key = cfg.get("interval", DEFAULT_INTERVAL_KEY)
                if key in INTERVAL_PRESETS:
                    self._interval_key = key
                    self._interval_secs = INTERVAL_PRESETS[key]
                self._last_run = cfg.get("last_run", 0.0)
        except Exception as e:
            logger.warning("Failed to load clustering config: %s", e)

    def _save_config(self):
        # Rust persists scheduler timing in SQLite. Keep the hook as a
        # compatibility seam for callers/tests that observe an explicit run,
        # but never recreate the legacy JSON file.
        return None

    def set_interval(self, key: str):
        """Set the clustering interval (e.g. '1d', '1w', '1m', '6m')."""
        if key not in INTERVAL_PRESETS:
            raise ValueError(f"Unknown interval key: {key!r}")
        self._interval_key = key
        self._interval_secs = INTERVAL_PRESETS[key]
        logger.info("Clustering interval set to %s (%ds)", key, self._interval_secs)

    def get_config(self) -> Dict[str, Any]:
        return {
            "interval": self._interval_key,
            "interval_secs": self._interval_secs,
            "last_run": self._last_run,
            "running": self._running,
        }

    def start(self):
        """Retained for compatibility; Rust owns automatic scheduling."""
        logger.debug("Ignoring Python clustering scheduler start; Rust owns scheduling")

    def stop(self):
        """Retained for compatibility; there is no Python timer to stop."""

    def run_scheduled(self) -> Dict[str, Any]:
        """Run one Rust-admitted automatic clustering slice.

        Rust owns the idle gate, retry policy, and interval. Keeping the
        result on this compatibility facade is still important because the
        existing ``get_tasks`` IPC response reads the most recent hot-cluster
        result from here.
        """
        from monitor.config import CLUSTERING_ALLOW_FULL_LOW_MEMORY, CLUSTERING_ENABLED

        if not CLUSTERING_ENABLED:
            result = {"status": "disabled", "error": "Clustering is disabled"}
            self._last_result = result
            return result
        if self._running:
            return {"status": "already_running"}

        self._running = True
        try:
            result = self._manager.run_clustering(
                auto_compress=True,
                clustering_mode="auto",
                manual=False,
                allow_full_low_memory=CLUSTERING_ALLOW_FULL_LOW_MEMORY,
                background=True,
            )
            if result.get("status") in {"already_running", "waiting_for_index"}:
                # Neither outcome completed a clustering interval. In
                # particular, waiting_for_index means Rust has not mirrored
                # semantic vectors yet; recording a run here would postpone
                # the next attempt for the full configured interval.
                return result
            self._last_result = result
            self._last_run = time.time()
            self._save_config()
            return result
        finally:
            self._running = False

    def _do_run(self) -> bool:
        """Compatibility probe for callers from pre-Rust scheduler releases.

        Production automatic work enters through :meth:`run_scheduled`, which
        is admitted by Rust after the unified idle/auth/retry checks. This
        method remains synchronous for older integrations and tests that called
        the former worker directly; it is never started by :meth:`start`.
        """
        from monitor.config import CLUSTERING_ALLOW_FULL_LOW_MEMORY, CLUSTERING_ENABLED
        if not CLUSTERING_ENABLED:
            logger.debug("Skipping scheduled clustering: feature disabled")
            return False
        if self._running:
            return False
        storage_client = self._storage_client or getattr(self._manager, "_storage_client", None)
        if storage_client:
            try:
                idle = storage_client.get_idle_state()
            except Exception as e:
                logger.warning("Skipping scheduled clustering: idle state unavailable: %s", e)
                return False
            if not isinstance(idle, dict):
                logger.warning("Skipping scheduled clustering: idle state malformed: %r", idle)
                return False
            if not idle.get("is_idle", False):
                logger.debug(
                    "Skipping scheduled clustering: system not idle (idle_secs=%s fullscreen=%s)",
                    idle.get("idle_secs"),
                    idle.get("fullscreen_exclusive"),
                )
                return False
        if not TaskEmbedder.is_model_available():
            logger.debug("Skipping scheduled clustering: MiniLM model not downloaded")
            return False
        self._running = True
        success = False
        try:
            logger.info("Scheduled clustering run starting …")
            result = self._manager.run_clustering(
                auto_compress=True,
                clustering_mode="auto",
                manual=False,
                allow_full_low_memory=CLUSTERING_ALLOW_FULL_LOW_MEMORY,
            )
            if result.get("status") == "already_running":
                # A manual run beat us to the clustering guard. Do not touch
                # `_last_run`: treating this as a completed run would push the
                # next scheduled attempt out by a whole interval. Returning
                # False sends the loop into its 60 s backoff instead.
                logger.info("Scheduled clustering yielded to a run already in progress")
                return False
            self._last_result = result
            self._last_run = time.time()
            self._save_config()
            logger.info("Scheduled clustering run complete: %s", {
                k: v for k, v in result.items() if k != "clusters"
            })
            success = True
        except Exception as e:
            logger.error("Scheduled clustering run failed: %s", e)
        finally:
            self._running = False
        return success

    def run_now(
        self,
        start_time: Optional[float] = None,
        end_time: Optional[float] = None,
        clustering_mode: str = "auto",
        manual: bool = False,
    ) -> Dict[str, Any]:
        """Manually trigger a clustering run (blocking)."""
        from monitor.config import CLUSTERING_ALLOW_FULL_LOW_MEMORY, CLUSTERING_ENABLED
        if not CLUSTERING_ENABLED:
            return {"status": "disabled", "error": "Clustering is disabled"}
        if self._running:
            return {"status": "already_running"}
        self._running = True
        try:
            result = self._manager.run_clustering(
                start_time=start_time,
                end_time=end_time,
                auto_compress=(start_time is None),
                clustering_mode=clustering_mode,
                manual=bool(manual or (start_time is not None and end_time is not None)),
                allow_full_low_memory=CLUSTERING_ALLOW_FULL_LOW_MEMORY,
            )
            self._last_result = result
            if start_time is None:
                self._last_run = time.time()
                self._save_config()
            return result
        finally:
            self._running = False

    def get_last_result(self) -> Optional[Dict[str, Any]]:
        return self._last_result
