"""OCR service module — lightweight container for OCR engine and vector store.

Replaces the former ScreenshotOCRWorker.  The screenshot capture loop
and task queue have been removed (handled by Rust).  Python now only
provides OCR inference and vector-store management through IPC commands.
"""

import datetime
import logging
from typing import Optional, Dict, Any, List

logger = logging.getLogger(__name__)

from ocr_engine import OCREngine, get_ocr_engine
from vector_store import VectorStore
from storage_client import StorageClient, get_storage_client, init_storage_client


class OCRService:
    """Lightweight OCR + vector-store service container.

    Responsibilities:
    - Hold references to the OCR engine and vector store singletons.
    - Expose ``search_by_natural_language`` and ``get_stats``.
    - Provide ``pause`` / ``resume`` / ``stop`` lifecycle helpers.
    """

    def __init__(
        self,
        vector_db_path: str = "./chroma_db",
        enable_vector_store: bool = True,
        storage_pipe: str = None,
        chroma_client = None,
    ):
        """Initialise the OCR service.

        Args:
            vector_db_path: ChromaDB persistence directory.
            enable_vector_store: Whether to load Chinese-CLIP and ChromaDB.
            storage_pipe: Named pipe for the Rust storage service (reverse IPC).
            chroma_client: Optional shared ChromaDB persistent client.
        """
        # Storage client (for sending data to Rust)
        self.storage_pipe = storage_pipe
        self.storage_client: Optional[StorageClient] = None
        if storage_pipe:
            self.storage_client = init_storage_client(storage_pipe)
            logger.info("Storage client initialised: %s", storage_pipe)

        # OCR engine (thread-safe singleton)
        logger.info("Initialising OCR engine...")
        try:
            self.ocr_engine: OCREngine = get_ocr_engine()
            logger.info("OCR engine initialised successfully.")
        except Exception as e:
            logger.error("OCR engine initialisation failed: %s", e)
            logger.exception("OCR engine initialisation error")
            raise

        # Vector store
        self.enable_vector_store = enable_vector_store
        self.vector_store: Optional[VectorStore] = None
        if enable_vector_store:
            logger.info("Initialising vector store...")
            self.vector_store = VectorStore(
                collection_name="screenshots",
                persist_directory=vector_db_path,
                storage_client=self.storage_client,
                chroma_client=chroma_client,
            )

        # Statistics
        self.stats = {
            "processed_count": 0,
            "failed_count": 0,
            "total_texts_found": 0,
            "start_time": None,
        }

    # ------------------------------------------------------------------
    # Lifecycle
    # ------------------------------------------------------------------

    def start(self):
        """Mark the service as started (records start time)."""
        self.stats["start_time"] = datetime.datetime.now()
        logger.info("OCR service started.")

    def pause(self):
        """Pause the service (handled via config.paused_event)."""
        logger.info("OCR service paused.")

    def resume(self):
        """Resume the service."""
        logger.info("OCR service resumed.")

    def stop(self):
        """Stop the service."""
        logger.info("OCR service stopped.")

    # ------------------------------------------------------------------
    # Queries
    # ------------------------------------------------------------------

    def search_by_natural_language(
        self,
        query: str,
        n_results: int = 10,
        offset: int = 0,
        process_names: Optional[List[str]] = None,
        start_time: Optional[float] = None,
        end_time: Optional[float] = None,
    ) -> list:
        """Search screenshots using natural language via Chinese-CLIP vectors."""
        import time as _time

        _t_total = _time.perf_counter()

        if not self.vector_store:
            raise RuntimeError("Vector store not enabled")

        # Fetch extra candidates for post-filtering
        target_count = max(int(n_results) + int(offset), int(n_results))
        buffer_multiplier = 2
        fetch_count = max(target_count * buffer_multiplier, target_count + 20)

        _t0 = _time.perf_counter()
        raw_results = self.vector_store.search_by_text(query, n_results=fetch_count)
        _t_vector_search = _time.perf_counter() - _t0

        filtered: List[Dict[str, Any]] = []
        normalized_processes = None
        if process_names:
            normalized_processes = [
                p for p in process_names if isinstance(p, str) and p.strip()
            ]

        def _parse_timestamp(value: Optional[str]) -> Optional[float]:
            if not value:
                return None
            try:
                return datetime.datetime.strptime(
                    value, "%Y-%m-%d %H:%M:%S"
                ).timestamp()
            except ValueError:
                return None

        start_ts = float(start_time) if start_time is not None else None
        end_ts = float(end_time) if end_time is not None else None

        _t0 = _time.perf_counter()
        for item in raw_results:
            metadata = item.get("metadata") or {}
            process_name = (metadata.get("process_name") or "").strip()

            if normalized_processes and process_name not in normalized_processes:
                continue

            created_at_str = metadata.get("created_at") or metadata.get(
                "screenshot_created_at"
            )
            created_ts = _parse_timestamp(created_at_str)

            # Ensure screenshot_created_at is always present (frontend timeline depends on it)
            if created_at_str:
                if "screenshot_created_at" not in metadata:
                    metadata["screenshot_created_at"] = created_at_str
                item["screenshot_created_at"] = created_at_str

            if start_ts is not None and created_ts is not None and created_ts < start_ts:
                continue
            if end_ts is not None and created_ts is not None and created_ts > end_ts:
                continue

            filtered.append(item)
        _t_filter = _time.perf_counter() - _t0

        # Apply offset and limit
        result = filtered[int(offset) : int(offset) + int(n_results)]

        if (_time.perf_counter() - _t_total) > 5.0:
            logger.warning(
                "[DIAG:search_nl] vector_search=%.3fs filter=%.3fs "
                "raw=%d filtered=%d returned=%d total=%.3fs",
                _t_vector_search,
                _t_filter,
                len(raw_results),
                len(filtered),
                len(result),
                _time.perf_counter() - _t_total,
            )
        return result

    # ------------------------------------------------------------------
    # Statistics
    # ------------------------------------------------------------------

    def get_stats(self) -> Dict[str, Any]:
        """Return runtime statistics."""
        stats = self.stats.copy()
        if stats["start_time"]:
            stats["runtime"] = str(datetime.datetime.now() - stats["start_time"])

        if self.vector_store:
            stats["vector_stats"] = self.vector_store.get_collection_stats()

        return stats
