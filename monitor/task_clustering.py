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
import logging
import threading
import hashlib
import numpy as np
from typing import List, Dict, Any, Optional, Tuple

logger = logging.getLogger(__name__)


class ModelNotAvailableError(Exception):
    """Raised when the MiniLM model files are not downloaded yet."""
    pass


# ---------------------------------------------------------------------------
# Constants
# ---------------------------------------------------------------------------
EMBEDDING_DIM = 384
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

            from transformers import AutoTokenizer, AutoModel

            model_path = os.environ.get("MINILM_MODEL_PATH")
            if not model_path:
                model_path = os.path.join(
                    os.environ.get("LOCALAPPDATA", os.path.expanduser("~")),
                    "carbonpaper",
                    "models",
                    "paraphrase-multilingual-MiniLM-L12-v2",
                )

            logger.info("Loading MiniLM-L12-v2 from %s …", model_path)
            self._tokenizer = AutoTokenizer.from_pretrained(model_path, local_files_only=True)
            self._model = AutoModel.from_pretrained(model_path, local_files_only=True)
            self._model.eval()
            logger.info("MiniLM-L12-v2 loaded (device=%s)", self._model.device)

    def unload(self):
        """Release model & tokenizer to free memory."""
        with self._lock:
            self._model = None
            self._tokenizer = None
        gc.collect()
        try:
            import torch
            if torch.cuda.is_available():
                torch.cuda.empty_cache()
        except Exception:
            pass
        logger.info("MiniLM-L12-v2 unloaded — memory released")

    # ---- encoding --------------------------------------------------------

    def encode(self, texts: List[str]) -> np.ndarray:
        """Batch-encode texts → (N, 384) L2-normalised numpy array."""
        self.load()
        import torch

        encoded = self._tokenizer(
            texts,
            padding=True,
            truncation=True,
            max_length=256,
            return_tensors="pt",
        )
        with torch.no_grad():
            out = self._model(**encoded)
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

        # ---- Aggregate per-cluster statistics ----
        clusters = []
        noise_ids = []

        for i, (sid, lbl) in enumerate(zip(ids, labels)):
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

            # dominant process / category by frequency
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
        self._lock = threading.Lock()

        logger.info("[task_clustering] HotColdManager ready (lazy loading collections)")

    @property
    def hot_collection(self):
        with self._lock:
            if not hasattr(self, "_hot_collection"):
                self._hot_collection = self._client.get_or_create_collection(
                    name="task_vectors",
                    metadata={"hnsw:space": "cosine"},
                )
            return self._hot_collection

    @property
    def cold_collection(self):
        with self._lock:
            if not hasattr(self, "_cold_collection"):
                self._cold_collection = self._client.get_or_create_collection(
                    name="task_centroids",
                    metadata={"hnsw:space": "cosine"},
                )
            return self._cold_collection

    def unload_collections(self):
        """Unload collections from memory to save HNSW overhead."""
        with self._lock:
            if hasattr(self, "_hot_collection"):
                delattr(self, "_hot_collection")
            if hasattr(self, "_cold_collection"):
                delattr(self, "_cold_collection")
            
            # Try to drop from Chroma's internal cache
            try:
                if hasattr(self._client, "_collections"):
                    self._client._collections.pop("task_vectors", None)
                    self._client._collections.pop("task_centroids", None)
            except Exception:
                pass

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

    def add_snapshot(
        self,
        screenshot_id: int,
        process_name: str,
        window_title: str,
        ocr_text: str,
        timestamp: float,
        category: str = "",
    ):
        """Encode and store a single snapshot in the hot layer.

        Silently skips if the MiniLM model is not yet downloaded.
        The timestamp is normalised to seconds (Unix epoch).
        """
        if not TaskEmbedder.is_model_available():
            return  # model not downloaded yet, skip silently

        combined = build_task_text(process_name, window_title, ocr_text)
        if not combined.strip():
            return

        # Normalise timestamp to seconds — callers may pass milliseconds
        if timestamp > 1e12:
            timestamp = timestamp / 1000.0

        doc_id = str(screenshot_id)
        # Check for duplicate
        try:
            existing = self.hot_collection.get(ids=[doc_id])
            if existing and existing["ids"]:
                return
        except Exception:
            pass

        vector = self._embedder.encode_single(combined)

        metadata = {
            "screenshot_id": screenshot_id,
            "timestamp": timestamp,
            "process_name": self._encrypt(process_name) if process_name else "",
            "window_title": self._encrypt(window_title) if window_title else "",
            "category": category or "",
            "layer": "hot",
        }

        self.hot_collection.add(
            ids=[doc_id],
            embeddings=[vector.tolist()],
            metadatas=[metadata],
            documents=[self._encrypt(combined)],
        )

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

        # Remove expired hot vectors
        try:
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
                    self.hot_collection.add(
                        ids=batch_ids,
                        embeddings=vectors.tolist(),
                        metadatas=batch_metas,
                    )
                    added += len(batch_ids)
                    logger.info("Backfilled %d/%d (page offset %d)", added, total, offset)
                except Exception as e:
                    logger.warning("Backfill encode/add failed at offset %d: %s", offset, e)

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
    ) -> Dict[str, Any]:
        """Execute HDBSCAN clustering on hot-layer vectors.

        Args:
            start_time / end_time: optional range override (for manual runs).
            min_cluster_size / min_samples: HDBSCAN params.
            auto_compress: if True, compress old clusters to cold layer.

        Returns clustering results dict.
        """
        with self._lock:
            logger.info("Starting clustering run (range=%s–%s) …",
                        start_time or "auto", end_time or "auto")

            # Load embedder
            self._embedder.load()

            try:
                # Fetch vectors
                if start_time is not None and end_time is not None:
                    vectors, ids, metas = self.get_hot_vectors_in_range(start_time, end_time)
                else:
                    vectors, ids, metas = self.get_hot_vectors()

                # If hot layer is empty, try backfilling from screenshot_embeddings
                if len(ids) == 0:
                    logger.warning("Hot layer empty — attempting backfill from SQLite")
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

                # Run engine
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
                    "n_total": len(ids),
                    "status": "success",
                }
            finally:
                # Always unload the model after clustering to free memory
                self._embedder.unload()
                # Unload collections to save HNSW memory overhead when idle
                self.unload_collections()

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
        """Scheduler loop — check every 60s whether it's time to re-run."""
        while not self._stop_event.is_set():
            now = time.time()
            elapsed = now - self._last_run
            if elapsed >= self._interval_secs:
                self._do_run()
            # Sleep in small increments so stop is responsive
            self._stop_event.wait(timeout=60)

    def _do_run(self):
        """Execute one clustering run."""
        if self._running:
            return
        if not TaskEmbedder.is_model_available():
            logger.debug("Skipping scheduled clustering: MiniLM model not downloaded")
            return
        self._running = True
        try:
            logger.info("Scheduled clustering run starting …")
            result = self._manager.run_clustering(auto_compress=True)
            self._last_result = result
            self._last_run = time.time()
            self._save_config()
            logger.info("Scheduled clustering run complete: %s", {
                k: v for k, v in result.items() if k != "clusters"
            })
        except Exception as e:
            logger.error("Scheduled clustering run failed: %s", e)
        finally:
            self._running = False

    def run_now(self, start_time: Optional[float] = None, end_time: Optional[float] = None) -> Dict[str, Any]:
        """Manually trigger a clustering run (blocking)."""
        if self._running:
            return {"status": "already_running"}
        self._running = True
        try:
            result = self._manager.run_clustering(
                start_time=start_time,
                end_time=end_time,
                auto_compress=(start_time is None),
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
