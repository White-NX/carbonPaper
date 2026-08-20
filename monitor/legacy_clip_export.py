"""Read-only access to the pre-v0.8.5 Chroma screenshot collection.

The collection is retained only long enough to finish an interrupted legacy
CLIP migration.  This module deliberately has no encoder, query, upsert, or
delete operation.  A missing collection is treated as an empty export so a
fresh installation does not need to create a Chroma collection merely to
answer a migration request.
"""

from __future__ import annotations

import logging
from typing import Any, Dict

from collection_export import CollectionSnapshotExporter

logger = logging.getLogger(__name__)

CLIP_EMBEDDING_DIM = 512
LEGACY_COLLECTION_NAME = "screenshots"


class _EmptyCollection:
    """Small Chroma-shaped object used when the legacy collection is absent."""

    def get(self, ids=None, include=None, **_kwargs):
        return {"ids": [], "embeddings": []}


class LegacyClipVectorExporter:
    """Expose only the resumable, read-only legacy CLIP export contract."""

    def __init__(self, chroma_client: Any):
        self.client = chroma_client
        self._empty = _EmptyCollection()
        self._collection_cache = None
        self._snapshot_exporter = CollectionSnapshotExporter(
            namespace="clip",
            collection_getter=self._collection,
            dimensions=CLIP_EMBEDDING_DIM,
            thread_name_prefix="clip-vector-export",
        )

    def _collection(self):
        if self._collection_cache is not None:
            return self._collection_cache
        if self.client is None:
            return self._empty
        try:
            self._collection_cache = self.client.get_collection(LEGACY_COLLECTION_NAME)
        except Exception:
            # A read-only migration must not create a new collection.  Treat
            # an absent/invalid legacy store as an empty source instead.
            logger.debug("Legacy CLIP collection is unavailable", exc_info=True)
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
