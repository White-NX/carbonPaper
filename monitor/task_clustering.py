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
import sys
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
MAX_TASK_VECTOR_EXPORTS = 4
TASK_VECTOR_EXPORT_LOGICAL_TIMEOUT_SECS = 10 * 60
TASK_VECTOR_EXPORT_IDLE_TTL_SECS = 24 * 60 * 60
TASK_VECTOR_EXPORT_HARD_TTL_SECS = 7 * 24 * 60 * 60
TASK_VECTOR_EXPORT_TMP_TTL_SECS = 60 * 60
HOT_LAYER_DAYS = 30
CENTROID_MATCH_THRESHOLD = 0.55   # cosine similarity threshold for assigning to existing cluster
MIN_CLUSTER_SIZE = 5
MIN_SAMPLES = 3
PACMAP_N_COMPONENTS = 15          # target dims for PaCMAP reduction


# ---------------------------------------------------------------------------
# TaskEmbedder — singleton, loadable / unloadable
# ---------------------------------------------------------------------------

class TaskEmbedder:
    """Singleton for paraphrase-multilingual-MiniLM-L12-v2.

    Designed to be loaded on demand and **unloaded** after clustering to
    reclaim ~200 MB of RAM.
    """

    _instance = None
    _lock = threading.Lock()

    def __new__(cls):
        if cls._instance is None:
            cls._instance = super().__new__(cls)
            cls._instance._model = None
            cls._instance._tokenizer = None
            cls._instance._is_onnx = False
        return cls._instance

    # ---- lifecycle -------------------------------------------------------

    @staticmethod
    def is_model_available() -> bool:
        """Check whether the MiniLM model files exist on disk."""
        model_path = os.environ.get("MINILM_MODEL_PATH")
        if not model_path:
            model_path = os.path.join(
                os.environ.get("LOCALAPPDATA", os.path.expanduser("~")),
                "carbonpaper",
                "models",
                "paraphrase-multilingual-MiniLM-L12-v2",
            )
        from onnx_utils import is_onnx_testing_enabled, get_onnx_model_path
        if is_onnx_testing_enabled():
            primary_onnx_path = os.path.join(
                os.environ.get("LOCALAPPDATA", os.path.expanduser("~")),
                "CarbonPaper",
                "models-onnx",
                "paraphrase-multilingual-MiniLM-L12-v2",
            )
            if not os.environ.get("MINILM_MODEL_PATH") and (
                get_onnx_model_path(primary_onnx_path, "model_int8.onnx")
                or get_onnx_model_path(primary_onnx_path, os.path.join("onnx", "model_quantized.onnx"))
            ):
                model_path = primary_onnx_path
            has_onnx = bool(get_onnx_model_path(model_path, "model_int8.onnx") or
                            get_onnx_model_path(model_path, os.path.join("onnx", "model_quantized.onnx")))
            required_files = ["config.json", "tokenizer.json"]
            return has_onnx and all(os.path.isfile(os.path.join(model_path, f)) for f in required_files)
        else:
            required_files = ["config.json", "pytorch_model.bin", "tokenizer.json"]
            return all(os.path.isfile(os.path.join(model_path, f)) for f in required_files)

    def is_loaded(self) -> bool:
        return self._model is not None

    def load(self):
        """Load model & tokenizer (idempotent)."""
        if self._model is not None:
            return

        with self._lock:
            if self._model is not None:
                return

            model_path = os.environ.get("MINILM_MODEL_PATH")
            if not model_path:
                model_path = os.path.join(
                    os.environ.get("LOCALAPPDATA", os.path.expanduser("~")),
                    "carbonpaper",
                    "models",
                    "paraphrase-multilingual-MiniLM-L12-v2",
                )

            from onnx_utils import is_onnx_testing_enabled, get_onnx_model_path, create_onnx_session

            if is_onnx_testing_enabled():
                primary_onnx_path = os.path.join(
                    os.environ.get("LOCALAPPDATA", os.path.expanduser("~")),
                    "CarbonPaper",
                    "models-onnx",
                    "paraphrase-multilingual-MiniLM-L12-v2",
                )
                if not os.environ.get("MINILM_MODEL_PATH") and (
                    get_onnx_model_path(primary_onnx_path, "model_int8.onnx")
                    or get_onnx_model_path(primary_onnx_path, os.path.join("onnx", "model_quantized.onnx"))
                ):
                    model_path = primary_onnx_path
                onnx_file = get_onnx_model_path(model_path, "model_int8.onnx") or get_onnx_model_path(model_path, os.path.join("onnx", "model_quantized.onnx"))
                if onnx_file:
                    from logging_config import log_model_loading
                    log_model_loading("MiniLM-L12-v2 (ONNX)")
                    logger.info("Loading MiniLM-L12-v2 from ONNX: %s ...", onnx_file)
                    from numpy_tokenizer import NumpyTokenizer
                    self._tokenizer = NumpyTokenizer(model_path)
                    self._model = create_onnx_session(onnx_file)
                    self._is_onnx = True
                    logger.info("MiniLM-L12-v2 loaded successfully via ONNX")
                    return

            from transformers import AutoTokenizer, AutoModel
            from logging_config import log_model_loading
            log_model_loading("MiniLM-L12-v2")
            logger.info("Loading MiniLM-L12-v2 from %s …", model_path)
            self._tokenizer = AutoTokenizer.from_pretrained(model_path, local_files_only=True)
            self._model = AutoModel.from_pretrained(model_path, local_files_only=True)
            self._model.eval()
            self._is_onnx = False
            logger.info("MiniLM-L12-v2 loaded (device=%s)", self._model.device)

    def _acquire_runtime(self, attempts: int = 3):
        """Return a consistent ``(model, tokenizer, is_onnx)`` snapshot.

        The three pieces are read together under ``_lock`` so a concurrent
        :meth:`unload` cannot null one of them between the reads. The caller
        then runs its forward pass against these local references: an unload
        landing mid-pass drops the singleton's handles while the objects
        themselves stay alive until that pass returns.

        ``Reranker.rerank`` has always done exactly this — snapshot the session
        and tokenizer under its lock, then run the forward pass outside it —
        which is why the reranker never had this bug and this class did.

        The snapshot is what lets ``run_clustering`` keep unloading the model in
        its ``finally``, worth ~479 MB resident on the ONNX backend, while no
        longer holding the manager lock across the whole run. A foreground NL
        search encodes outside that lock on purpose, and until now ``encode``
        read ``_is_onnx``, ``_tokenizer`` and ``_model`` as three separate
        unguarded attribute loads, so an interleaved unload turned the third
        one into ``'NoneType' object has no attribute 'get_inputs'``.
        """
        for _ in range(max(1, attempts)):
            self.load()
            with self._lock:
                # `load` assigns the tokenizer before the model on both
                # backends, so a non-None model implies a usable tokenizer.
                # Keep that order if either branch is ever rewritten.
                if self._model is not None:
                    return self._model, self._tokenizer, self._is_onnx
        raise ModelNotAvailableError(
            "MiniLM was unloaded repeatedly while a caller was trying to use it"
        )

    def unload(self):
        """Release model & tokenizer to free memory."""
        with self._lock:
            was_onnx = self._is_onnx
            self._model = None
            self._tokenizer = None
            self._is_onnx = False
        gc.collect()
        # Never import Torch merely to clear its cache. In ONNX mode that
        # would load hundreds of MiB of native DLLs during model teardown.
        if not was_onnx and "torch" in sys.modules:
            try:
                torch = sys.modules["torch"]
                if torch.cuda.is_available():
                    torch.cuda.empty_cache()
            except Exception:
                pass
        logger.info("MiniLM-L12-v2 unloaded — memory released")

    # ---- encoding --------------------------------------------------------

    def encode(self, texts: List[str]) -> np.ndarray:
        """Batch-encode texts → (N, 384) L2-normalised numpy array."""
        # One consistent snapshot, then a forward pass on local references only:
        # a concurrent unload() must not be able to null the model out from
        # under a pass that has already started. See _acquire_runtime.
        model, tokenizer, is_onnx = self._acquire_runtime()

        if is_onnx:
            encoded = tokenizer(
                texts,
                padding=True,
                truncation=True,
                max_length=256,
                return_tensors="np",
            )
            from onnx_utils import build_transformer_inputs
            inputs = build_transformer_inputs(model, encoded)
            outputs = model.run(None, inputs)
            token_embeddings = outputs[0]

            attention_mask = encoded["attention_mask"]
            input_mask_expanded = np.expand_dims(attention_mask, axis=-1).astype(np.float32)
            sum_embeddings = np.sum(token_embeddings * input_mask_expanded, axis=1)
            sum_mask = np.clip(np.sum(input_mask_expanded, axis=1), a_min=1e-9, a_max=None)
            emb = sum_embeddings / sum_mask

            norm = np.linalg.norm(emb, axis=1, keepdims=True)
            emb = emb / np.clip(norm, a_min=1e-9, a_max=None)
            return emb

        import torch

        encoded = tokenizer(
            texts,
            padding=True,
            truncation=True,
            max_length=256,
            return_tensors="pt",
        )
        with torch.no_grad():
            out = model(**encoded)
            # Mean pooling (standard for sentence-transformers)
            attention_mask = encoded["attention_mask"]
            token_embeddings = out.last_hidden_state
            input_mask_expanded = attention_mask.unsqueeze(-1).expand(token_embeddings.size()).float()
            emb = (token_embeddings * input_mask_expanded).sum(1) / input_mask_expanded.sum(1).clamp(min=1e-9)
            emb = torch.nn.functional.normalize(emb, p=2, dim=1)
        return emb.cpu().numpy()

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
        self._task_vector_exports = {}
        self._task_vector_export_executor = ThreadPoolExecutor(
            max_workers=1,
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

    def _decrypt(self, text: str) -> str:
        if self._storage_client and text:
            if text.startswith("ENC2:") or text.startswith("ENC:"):
                dec = self._storage_client.decrypt_from_chromadb(text)
                if dec is not None:
                    return dec
        return text

    # ---- Hot layer operations --------------------------------------------

    @staticmethod
    def _task_vector_sort_key(doc_id: str):
        try:
            parsed = int(doc_id)
            if parsed > 0 and str(parsed) == doc_id:
                return (0, parsed)
        except (TypeError, ValueError):
            pass
        return (1, doc_id)

    @staticmethod
    def _migration_artifact_root() -> str:
        data_dir = os.environ.get("CARBONPAPER_DATA_DIR")
        if not data_dir:
            local_appdata = os.environ.get("LOCALAPPDATA", os.path.expanduser("~"))
            data_dir = os.path.join(local_appdata, "CarbonPaper", "data")
        return os.path.join(data_dir, "migrations", "minilm")

    @classmethod
    def _task_vector_export_dir(cls, export_id: str) -> str:
        return os.path.join(cls._migration_artifact_root(), export_id)

    @staticmethod
    def _validate_export_id(export_id: str) -> str:
        export_id = str(export_id or "")
        if not (16 <= len(export_id) <= 128) or any(
            not (ch.isalnum() or ch in "-_") for ch in export_id
        ):
            raise ValueError("invalid task vector export id")
        return export_id

    def _cleanup_task_vector_exports(self) -> None:
        now_wall = time.time()
        now_mono = time.monotonic()
        root = self._migration_artifact_root()
        try:
            os.makedirs(root, exist_ok=True)
            for name in os.listdir(root):
                path = os.path.join(root, name)
                if not os.path.isdir(path):
                    continue
                try:
                    age = now_wall - os.path.getmtime(path)
                    ttl = (
                        TASK_VECTOR_EXPORT_TMP_TTL_SECS
                        if name.endswith(".tmp")
                        else TASK_VECTOR_EXPORT_HARD_TTL_SECS
                    )
                    if age > ttl:
                        shutil.rmtree(path, ignore_errors=True)
                except OSError:
                    pass
        except OSError:
            logger.debug("[task_clustering] failed to clean export artifacts", exc_info=True)

        with self._lock:
            expired = [
                export_id
                for export_id, state in self._task_vector_exports.items()
                if state.get("status") != "preparing"
                and now_mono - state.get("last_access", now_mono)
                > TASK_VECTOR_EXPORT_IDLE_TTL_SECS
            ]
            for export_id in expired:
                self._task_vector_exports.pop(export_id, None)

    def _build_task_vectors_export(
        self,
        export_id: str,
        stop_event: threading.Event,
    ) -> None:
        final_dir = self._task_vector_export_dir(export_id)
        temp_dir = final_dir + ".tmp"
        try:
            results = self.hot_collection.get(include=[])
            ids = [str(doc_id) for doc_id in (results.get("ids") or [])]
            ids.sort(key=self._task_vector_sort_key)

            with self._lock:
                state = self._task_vector_exports.get(export_id)
                if (
                    state is None
                    or state.get("status") != "preparing"
                    or stop_event.is_set()
                ):
                    return

            os.makedirs(self._migration_artifact_root(), exist_ok=True)
            shutil.rmtree(temp_dir, ignore_errors=True)
            os.makedirs(temp_dir, exist_ok=False)
            ids_path = os.path.join(temp_dir, "ids.json")
            with open(ids_path + ".tmp", "w", encoding="utf-8") as stream:
                json.dump(ids, stream, ensure_ascii=False, separators=(",", ":"))
                stream.flush()
                os.fsync(stream.fileno())
            os.replace(ids_path + ".tmp", ids_path)
            manifest = {
                "export_id": export_id,
                "total": len(ids),
                "created_at": time.time(),
            }
            manifest_path = os.path.join(temp_dir, "manifest.json")
            with open(manifest_path + ".tmp", "w", encoding="utf-8") as stream:
                json.dump(manifest, stream, separators=(",", ":"))
                stream.flush()
                os.fsync(stream.fileno())
            os.replace(manifest_path + ".tmp", manifest_path)

            with self._lock:
                state = self._task_vector_exports.get(export_id)
                if (
                    state is None
                    or state.get("status") != "preparing"
                    or stop_event.is_set()
                ):
                    shutil.rmtree(temp_dir, ignore_errors=True)
                    return
                if os.path.isdir(final_dir):
                    shutil.rmtree(final_dir, ignore_errors=True)
                os.replace(temp_dir, final_dir)
                state.update({
                    "status": "ready",
                    "total": len(ids),
                    "ids": tuple(ids),
                    "last_access": time.monotonic(),
                    "finished_at": time.time(),
                })
        except Exception as exc:
            shutil.rmtree(temp_dir, ignore_errors=True)
            with self._lock:
                state = self._task_vector_exports.get(export_id)
                if state is not None and state.get("status") == "preparing":
                    state.update({
                        "status": "failed",
                        "error": str(exc),
                        "last_access": time.monotonic(),
                    })
            logger.exception("task_vectors export snapshot failed")

    def start_task_vectors_export(
        self,
        export_id: str,
    ) -> Dict[str, Any]:
        """Start a persistent ID snapshot without blocking the IPC worker."""
        self._cleanup_task_vector_exports()
        export_id = self._validate_export_id(export_id or secrets.token_hex(16))
        now_mono = time.monotonic()
        with self._lock:
            existing = self._task_vector_exports.get(export_id)
            if existing is not None:
                return self.get_task_vectors_export_status(export_id)
            if len(self._task_vector_exports) >= MAX_TASK_VECTOR_EXPORTS:
                oldest = min(
                    self._task_vector_exports,
                    key=lambda key: self._task_vector_exports[key].get("created_mono", now_mono),
                )
                self._task_vector_exports.pop(oldest, None)
            stop_event = threading.Event()
            self._task_vector_exports[export_id] = {
                "status": "preparing",
                "total": 0,
                "ids": None,
                "error": None,
                "created_mono": now_mono,
                "created_at": time.time(),
                "last_access": now_mono,
                "stop_event": stop_event,
            }
        self._task_vector_export_executor.submit(
            self._build_task_vectors_export,
            export_id,
            stop_event,
        )
        return {"export_id": export_id, "state": "preparing", "total": 0}

    def _restore_task_vector_export(self, export_id: str) -> Optional[Dict[str, Any]]:
        export_dir = self._task_vector_export_dir(export_id)
        manifest_path = os.path.join(export_dir, "manifest.json")
        ids_path = os.path.join(export_dir, "ids.json")
        if not (os.path.isfile(manifest_path) and os.path.isfile(ids_path)):
            return None
        try:
            with open(manifest_path, "r", encoding="utf-8") as stream:
                manifest = json.load(stream)
            with open(ids_path, "r", encoding="utf-8") as stream:
                ids = tuple(str(value) for value in json.load(stream))
            if manifest.get("export_id") != export_id or int(manifest.get("total", -1)) != len(ids):
                raise ValueError("task vector export manifest does not match ids")
            return {
                "status": "ready",
                "total": len(ids),
                "ids": ids,
                "error": None,
                "created_mono": time.monotonic(),
                "created_at": float(manifest.get("created_at", time.time())),
                "last_access": time.monotonic(),
                "stop_event": threading.Event(),
                "finished_at": os.path.getmtime(manifest_path),
            }
        except Exception:
            logger.exception("failed to restore task_vectors export %s", export_id)
            return None

    def get_task_vectors_export_status(self, export_id: str) -> Dict[str, Any]:
        export_id = self._validate_export_id(export_id)
        self._cleanup_task_vector_exports()
        with self._lock:
            state = self._task_vector_exports.get(export_id)
        if state is None:
            restored = self._restore_task_vector_export(export_id)
            if restored is not None:
                with self._lock:
                    self._task_vector_exports[export_id] = restored
                    state = restored
        if state is None:
            return {"export_id": export_id, "state": "missing", "total": 0}

        with self._lock:
            state = self._task_vector_exports[export_id]
            elapsed = time.monotonic() - state.get("created_mono", time.monotonic())
            if (
                state.get("status") == "preparing"
                and elapsed > TASK_VECTOR_EXPORT_LOGICAL_TIMEOUT_SECS
            ):
                state["status"] = "timed_out"
                state["error"] = "task vector ID snapshot exceeded its 10 minute deadline"
                state["stop_event"].set()
            state["last_access"] = time.monotonic()
            return {
                "export_id": export_id,
                "state": state.get("status", "missing"),
                "total": int(state.get("total", 0) or 0),
                "error": state.get("error"),
                "created_at": state.get("created_at"),
                "finished_at": state.get("finished_at"),
            }

    def export_task_vectors_page(
        self,
        export_id: str,
        cursor: int = 0,
        limit: int = 128,
    ) -> Dict[str, Any]:
        """Export one page from a stable id snapshot, preserving snapshot order.

        Vectors travel as one little-endian float32 blob wrapped in Base64
        instead of tens of thousands of JSON floats, keeping a 128-row page
        around 256 KB of pipe traffic.
        """
        export_id = self._validate_export_id(export_id)
        cursor = max(0, int(cursor))
        limit = max(1, min(500, int(limit)))
        with self._lock:
            snapshot = self._task_vector_exports.get(export_id)
        if snapshot is None:
            snapshot = self._restore_task_vector_export(export_id)
            if snapshot is not None:
                with self._lock:
                    self._task_vector_exports[export_id] = snapshot
        if snapshot is None:
            raise ValueError("unknown or expired task vector export")
        with self._lock:
            snapshot = self._task_vector_exports[export_id]
            if snapshot.get("status") != "ready":
                raise ValueError(f"task vector export is {snapshot.get('status')}")
            snapshot["last_access"] = time.monotonic()
            snapshot_ids = snapshot["ids"]
            page_ids = list(snapshot_ids[cursor:cursor + limit])
            total = len(snapshot_ids)

        if not page_ids:
            return {
                "ids": [],
                "dimensions": EMBEDDING_DIM,
                "embeddings_f32_le_b64": "",
                "missing_ids": [],
                "errors": [],
                "next_cursor": cursor,
                "done": True,
                "total": total,
            }

        results = self.hot_collection.get(ids=page_ids, include=["embeddings"])
        returned_ids = [str(doc_id) for doc_id in (results.get("ids") or [])]
        embeddings = results.get("embeddings")
        if embeddings is None:
            embeddings = []
        vectors_by_id = {}
        errors = []
        for doc_id, vector in zip(returned_ids, embeddings):
            try:
                row = np.asarray(vector, dtype="<f4").reshape(-1)
                if row.shape[0] != EMBEDDING_DIM:
                    raise ValueError(
                        f"expected {EMBEDDING_DIM} dimensions, got {row.shape[0]}"
                    )
                vectors_by_id[doc_id] = row
            except Exception as exc:
                errors.append({"id": doc_id, "error": str(exc)})

        ordered_ids = [doc_id for doc_id in page_ids if doc_id in vectors_by_id]
        if ordered_ids:
            payload = np.concatenate([vectors_by_id[doc_id] for doc_id in ordered_ids])
            embeddings_b64 = base64.b64encode(payload.tobytes()).decode("ascii")
        else:
            embeddings_b64 = ""
        missing_ids = [
            doc_id
            for doc_id in page_ids
            if doc_id not in vectors_by_id
            and not any(entry["id"] == doc_id for entry in errors)
        ]
        next_cursor = cursor + len(page_ids)
        return {
            "ids": ordered_ids,
            "dimensions": EMBEDDING_DIM,
            "embeddings_f32_le_b64": embeddings_b64,
            "missing_ids": missing_ids,
            "errors": errors,
            "next_cursor": next_cursor,
            "done": next_cursor >= total,
            "total": total,
        }

    def finish_task_vectors_export(self, export_id: str) -> bool:
        """Release memory and persistent artifacts after a completed migration."""
        export_id = self._validate_export_id(export_id)
        with self._lock:
            state = self._task_vector_exports.pop(export_id, None)
            if state is not None:
                state["stop_event"].set()
        shutil.rmtree(self._task_vector_export_dir(export_id), ignore_errors=True)
        shutil.rmtree(self._task_vector_export_dir(export_id) + ".tmp", ignore_errors=True)
        return state is not None

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
                    cl_copy["dominant_process"] = self._decrypt(cl_copy.get("dominant_process", ""))
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

    # ---- Natural-language retrieval (demo) -------------------------------

    def query_by_text(
        self,
        query: str,
        n_results: int = 30,
        enable_rerank: bool = False,
        rerank_overfetch: int = 4,
        ocr_snippet_chars: int = 600,
        rerank_variant: str = "uint8",
    ) -> List[Dict[str, Any]]:
        """Retrieve hot-layer snapshots most similar to a natural-language query.

        Reuses the MiniLM embedder + the existing ``task_vectors`` ChromaDB
        collection (cosine space). Returns a list ordered by descending
        similarity; each entry includes the decrypted process/title metadata
        so the caller can render it directly.

        When ``enable_rerank`` is True, fetches ``n_results * rerank_overfetch``
        candidates from the embedding index, pulls OCR text for each via the
        reverse storage IPC, and re-scores with the bge-reranker-v2-m3 cross
        encoder. The reranker sees ``process | title | OCR`` jointly with the
        query, which gives it the context to disambiguate cases the bi-encoder
        collapses (e.g. "神经网络" ML vs neuroscience). Each returned entry
        gains a ``rerank_score`` field; the list is sorted by it.
        """
        if not query or not query.strip():
            return []

        if not TaskEmbedder.is_model_available():
            raise ModelNotAvailableError("MiniLM model not downloaded")

        collection = self.hot_collection
        if collection is None:
            return []

        # Empty collection guard — ChromaDB raises on query against an empty index.
        try:
            if collection.count() == 0:
                return []
        except Exception:
            pass

        # How many to pull from the bi-encoder. Reranker over-fetches.
        fetch_n = max(1, int(n_results))
        if enable_rerank:
            fetch_n = max(fetch_n, fetch_n * max(1, int(rerank_overfetch)))

        # No manager lock around the model load, and none around the forward
        # pass. Both were the wrong place for it: this runs in a named-pipe
        # handler thread, and a cold load takes 1.5 s on the ONNX backend and
        # 22 s on torch — time the manager lock must never spend, since every
        # hot-layer write waits behind it. TaskEmbedder now hands out a
        # consistent (model, tokenizer, backend) snapshot per call, so an
        # unload from a finishing clustering run cannot tear the model down
        # underneath this encode.
        vec = self._embedder.encode_single(query.strip())

        try:
            raw = collection.query(
                query_embeddings=[vec.tolist()],
                n_results=fetch_n,
                include=["metadatas", "distances"],
            )
        except Exception as e:
            logger.warning("[task_clustering] query_by_text failed: %s", e)
            return []

        ids_batch = (raw.get("ids") or [[]])[0]
        metas_batch = (raw.get("metadatas") or [[]])[0]
        dists_batch = (raw.get("distances") or [[]])[0]

        candidates: List[Dict[str, Any]] = []
        for doc_id, meta, dist in zip(ids_batch, metas_batch, dists_batch):
            meta = meta or {}
            similarity = 1.0 - float(dist) if dist is not None else None
            candidates.append({
                "screenshot_id": int(meta.get("screenshot_id", doc_id) or 0),
                "similarity": similarity,
                "distance": float(dist) if dist is not None else None,
                "timestamp": float(meta.get("timestamp", 0.0) or 0.0),
                "process_name": self._decrypt(meta.get("process_name", "")),
                "window_title": self._decrypt(meta.get("window_title", "")),
                "category": meta.get("category", ""),
                "layer": meta.get("layer", "hot"),
            })

        if not enable_rerank or not candidates:
            return candidates[:int(n_results)]

        # ---- rerank path ---------------------------------------------------
        from reranker import Reranker, RerankerNotAvailableError

        try:
            reranker = Reranker()
            reranker.load(rerank_variant)  # raises RerankerNotAvailableError if missing
        except RerankerNotAvailableError:
            # Surface as a tagged exception so the caller can show a friendly hint.
            raise

        # Fetch OCR text for the candidate IDs in one IPC round-trip.
        ocr_by_id: Dict[int, str] = {}
        if self._storage_client:
            try:
                ids_to_fetch = [c["screenshot_id"] for c in candidates if c["screenshot_id"]]
                resp = self._storage_client.get_screenshots_with_ocr_by_ids(ids_to_fetch)
                for row in resp.get("screenshots", []) or []:
                    rid = int(row.get("id", 0) or 0)
                    if rid:
                        ocr_by_id[rid] = row.get("ocr_text", "") or ""
            except Exception as e:
                logger.warning("[task_clustering] OCR fetch for rerank failed: %s", e)

        docs: List[str] = []
        for c in candidates:
            ocr_text = ocr_by_id.get(c["screenshot_id"], "")
            if ocr_text and ocr_snippet_chars > 0:
                ocr_text = ocr_text[:ocr_snippet_chars]
            parts = [p for p in (c["process_name"], c["window_title"], ocr_text) if p]
            docs.append(" | ".join(parts) if parts else "(empty)")

        try:
            rerank_scores = reranker.rerank(query.strip(), docs, variant=rerank_variant)
        except Exception as e:
            logger.warning("[task_clustering] reranker failed, falling back to embedding order: %s", e)
            return candidates[:int(n_results)]

        for c, s in zip(candidates, rerank_scores):
            c["rerank_score"] = float(s)

        candidates.sort(key=lambda c: c.get("rerank_score", float("-inf")), reverse=True)
        return candidates[:int(n_results)]


# ---------------------------------------------------------------------------
# ClusteringScheduler — background timer for periodic re-runs
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
    """Background scheduler that periodically triggers HDBSCAN on the hot layer."""

    def __init__(self, manager: HotColdManager, storage_client=None):
        self._manager = manager
        self._storage_client = storage_client
        self._interval_key = DEFAULT_INTERVAL_KEY
        self._interval_secs = INTERVAL_PRESETS[DEFAULT_INTERVAL_KEY]
        self._stop_event = threading.Event()
        self._thread: Optional[threading.Thread] = None
        self._last_run: float = 0.0
        self._running = False
        self._last_result: Optional[Dict] = None

        # Load persisted config
        self._load_config()

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
        try:
            path = self._config_path()
            os.makedirs(os.path.dirname(path), exist_ok=True)
            with open(path, "w", encoding="utf-8") as f:
                json.dump({
                    "interval": self._interval_key,
                    "last_run": self._last_run,
                }, f)
        except Exception as e:
            logger.warning("Failed to save clustering config: %s", e)

    def set_interval(self, key: str):
        """Set the clustering interval (e.g. '1d', '1w', '1m', '6m')."""
        if key not in INTERVAL_PRESETS:
            raise ValueError(f"Unknown interval key: {key!r}")
        self._interval_key = key
        self._interval_secs = INTERVAL_PRESETS[key]
        self._save_config()
        logger.info("Clustering interval set to %s (%ds)", key, self._interval_secs)

    def get_config(self) -> Dict[str, Any]:
        return {
            "interval": self._interval_key,
            "interval_secs": self._interval_secs,
            "last_run": self._last_run,
            "running": self._running,
        }

    def start(self):
        """Start the scheduler background thread."""
        if self._thread and self._thread.is_alive():
            return
        self._stop_event.clear()
        self._thread = threading.Thread(target=self._loop, daemon=True, name="clustering-scheduler")
        self._thread.start()
        logger.info("Clustering scheduler started (interval=%s)", self._interval_key)

    def stop(self):
        """Stop the scheduler."""
        self._stop_event.set()
        if self._thread:
            self._thread.join(timeout=5)
        logger.info("Clustering scheduler stopped")

    def _loop(self):
        """Scheduler loop — run when due based on (last_run + interval)."""
        while not self._stop_event.is_set():
            now = time.time()
            elapsed = now - self._last_run
            if elapsed >= self._interval_secs:
                did_run = self._do_run()
                if not did_run:
                    # Back off to avoid busy-spin when run is skipped/failed
                    # (e.g. model unavailable, concurrent run, exception path).
                    self._stop_event.wait(timeout=60.0)
                continue

            # Wait until the next due time (bounded to keep stop/config updates responsive).
            remaining = max(1.0, self._interval_secs - elapsed)
            self._stop_event.wait(timeout=min(60.0, remaining))

    def _do_run(self) -> bool:
        """Execute one clustering run. Returns True only on successful completion."""
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
