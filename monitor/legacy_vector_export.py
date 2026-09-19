"""Read-only access to legacy Chroma vector collections.

Legacy collections are retained so interrupted vector migrations can finish.
This module exposes read-only snapshots. A missing collection is an empty export so a
fresh installation does not need to create a Chroma collection merely to
answer a migration request.
"""

from __future__ import annotations

import logging
from typing import Any, Dict

from collection_export import CollectionSnapshotExporter, numeric_first_sort_key

logger = logging.getLogger(__name__)

CLIP_EMBEDDING_DIM = 512
MINILM_EMBEDDING_DIM = 384


class _EmptyCollection:
    """Small Chroma-shaped object used when the legacy collection is absent."""

    def get(self, ids=None, include=None, **_kwargs):
        return {"ids": [], "embeddings": []}


class LegacyVectorExporter:
    """Expose resumable legacy exports for the Rust-owned vector indexes."""

    def __init__(self, chroma_client: Any, *, kind: str):
        if kind not in {"clip", "minilm"}:
            raise ValueError(f"Unknown legacy vector kind: {kind}")
        self.client = chroma_client
        self.collection_name = "screenshots" if kind == "clip" else "task_vectors"
        self._empty = _EmptyCollection()
        self._collection_cache = None
        self._snapshot_exporter = CollectionSnapshotExporter(
            namespace=kind,
            collection_getter=self._collection,
            dimensions=CLIP_EMBEDDING_DIM if kind == "clip" else MINILM_EMBEDDING_DIM,
            sort_key=str if kind == "clip" else numeric_first_sort_key,
            thread_name_prefix=f"{kind}-vector-export",
        )

    def _collection(self):
        if self._collection_cache is not None:
            return self._collection_cache
        if self.client is None:
            raise RuntimeError("Legacy vector store is unavailable")
        try:
            self._collection_cache = self.client.get_collection(self.collection_name)
        except Exception:
            # Confirm absence through the catalog. A read failure in an existing
            # source must remain retryable instead of publishing an empty migration.
            if any(
                getattr(collection, "name", collection) == self.collection_name
                for collection in self.client.list_collections()
            ):
                raise
            logger.debug("Legacy %s collection is absent", self.collection_name)
            return self._empty
        return self._collection_cache

    def start(self, export_id: str) -> Dict[str, Any]:
        return self._snapshot_exporter.start(export_id)

    def status(self, export_id: str) -> Dict[str, Any]:
        return self._snapshot_exporter.status(export_id)

    def page(self, export_id: str, cursor: int = 0, limit: int = 128) -> Dict[str, Any]:
        return self._snapshot_exporter.page(export_id, cursor, limit)

    def finish(self, export_id: str) -> bool:
        return self._snapshot_exporter.finish(export_id)
