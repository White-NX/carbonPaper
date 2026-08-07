"""Resumable ID snapshots over a ChromaDB collection.

A derived-index migration cannot walk a live collection page by page: Chroma
gives no stable ordering across calls, so a row inserted or expired mid-walk
silently shifts every later page. The migration therefore takes one ID snapshot
up front, persists it, and pages against that fixed list. Rust holds a durable
cursor into it, so an interrupted run resumes at the page it never committed
rather than starting over.

Two collections use this. The MiniLM hot layer (``task_vectors``) exports
integer screenshot IDs; the Chinese-CLIP image collection (``screenshots``)
exports MD5 document IDs. Only the sort key and the vector width differ, which
is why they are parameters and everything else — the asynchronous build, the
atomically renamed manifest, the four TTLs, and the Base64 float32 page
payload — is shared.

Building the snapshot happens on a private single-worker thread rather than
inside the IPC handler. ``collection.get()`` over tens of thousands of rows can
take longer than one named-pipe request window, and a parked handler costs a
slot out of a pool of eight.
"""

import base64
import json
import logging
import os
import secrets
import shutil
import threading
import time
from concurrent.futures import ThreadPoolExecutor
from typing import Any, Callable, Dict, List, Optional, Sequence

import numpy as np

logger = logging.getLogger(__name__)

MAX_CONCURRENT_EXPORTS = 4
#: How long a snapshot may spend being built before the caller is told to give
#: up. Rust allows itself a minute more than this before it stops polling.
EXPORT_LOGICAL_TIMEOUT_SECS = 10 * 60
#: In-memory state is dropped after this long without access; the persisted
#: artifact survives and is restored on the next request.
EXPORT_IDLE_TTL_SECS = 24 * 60 * 60
#: A finished artifact nobody claimed is eventually reclaimed.
EXPORT_HARD_TTL_SECS = 7 * 24 * 60 * 60
#: A half-written directory from a crashed build.
EXPORT_TMP_TTL_SECS = 60 * 60

#: Chroma refuses very large `get(ids=...)` batches, and a page is also the unit
#: Rust commits in one transaction.
MAX_PAGE_ROWS = 500


def migration_artifact_root(namespace: str) -> str:
    """Where one migration's snapshots live, under the app's data directory."""
    data_dir = os.environ.get("CARBONPAPER_DATA_DIR")
    if not data_dir:
        local_appdata = os.environ.get("LOCALAPPDATA", os.path.expanduser("~"))
        data_dir = os.path.join(local_appdata, "CarbonPaper", "data")
    return os.path.join(data_dir, "migrations", namespace)


def validate_export_id(export_id: str) -> str:
    """Reject anything that could escape the artifact directory.

    The id arrives over IPC and becomes a path component, so it is constrained
    to an alphanumeric/dash/underscore token of a plausible length rather than
    merely sanitised.
    """
    export_id = str(export_id or "")
    if not (16 <= len(export_id) <= 128) or any(
        not (ch.isalnum() or ch in "-_") for ch in export_id
    ):
        raise ValueError("invalid collection export id")
    return export_id


def lexicographic_sort_key(doc_id: str):
    """Default ordering: stable, and meaningful for opaque hex ids."""
    return doc_id


def numeric_first_sort_key(doc_id: str):
    """Order canonical positive integers numerically, everything else after.

    ``task_vectors`` is keyed by ``str(screenshot_id)``, so a plain string sort
    would interleave ``"10"`` between ``"1"`` and ``"2"`` and make a resumed
    page walk hard to reason about against SQLite ids.
    """
    try:
        parsed = int(doc_id)
        if parsed > 0 and str(parsed) == doc_id:
            return (0, parsed)
    except (TypeError, ValueError):
        pass
    return (1, doc_id)


class CollectionSnapshotExporter:
    """One snapshot registry for one collection.

    ``collection_getter`` is a callable rather than a collection because both
    owners resolve their handle lazily and may rebuild it between calls.
    """

    def __init__(
        self,
        namespace: str,
        collection_getter: Callable[[], Any],
        dimensions: int,
        sort_key: Callable[[str], Any] = lexicographic_sort_key,
        thread_name_prefix: str = "collection-export",
    ):
        self._namespace = namespace
        self._collection_getter = collection_getter
        self._dimensions = int(dimensions)
        self._sort_key = sort_key
        self._exports: Dict[str, Dict[str, Any]] = {}
        self._lock = threading.RLock()
        self._executor = ThreadPoolExecutor(
            max_workers=1,
            thread_name_prefix=thread_name_prefix,
        )

    # -- paths ---------------------------------------------------------------

    def _export_dir(self, export_id: str) -> str:
        return os.path.join(migration_artifact_root(self._namespace), export_id)

    # -- lifecycle -----------------------------------------------------------

    def _cleanup(self) -> None:
        now_wall = time.time()
        now_mono = time.monotonic()
        root = migration_artifact_root(self._namespace)
        try:
            os.makedirs(root, exist_ok=True)
            for name in os.listdir(root):
                path = os.path.join(root, name)
                if not os.path.isdir(path):
                    continue
                try:
                    age = now_wall - os.path.getmtime(path)
                    ttl = (
                        EXPORT_TMP_TTL_SECS
                        if name.endswith(".tmp")
                        else EXPORT_HARD_TTL_SECS
                    )
                    if age > ttl:
                        shutil.rmtree(path, ignore_errors=True)
                except OSError:
                    pass
        except OSError:
            logger.debug(
                "[collection_export] failed to clean %s artifacts",
                self._namespace,
                exc_info=True,
            )

        with self._lock:
            expired = [
                export_id
                for export_id, state in self._exports.items()
                if state.get("status") != "preparing"
                and now_mono - state.get("last_access", now_mono) > EXPORT_IDLE_TTL_SECS
            ]
            for export_id in expired:
                self._exports.pop(export_id, None)

    def _build(self, export_id: str, stop_event: threading.Event) -> None:
        final_dir = self._export_dir(export_id)
        temp_dir = final_dir + ".tmp"
        try:
            results = self._collection_getter().get(include=[])
            ids = [str(doc_id) for doc_id in (results.get("ids") or [])]
            ids.sort(key=self._sort_key)

            with self._lock:
                state = self._exports.get(export_id)
                if (
                    state is None
                    or state.get("status") != "preparing"
                    or stop_event.is_set()
                ):
                    return

            os.makedirs(migration_artifact_root(self._namespace), exist_ok=True)
            shutil.rmtree(temp_dir, ignore_errors=True)
            os.makedirs(temp_dir, exist_ok=False)
            self._write_json(os.path.join(temp_dir, "ids.json"), ids)
            self._write_json(
                os.path.join(temp_dir, "manifest.json"),
                {
                    "export_id": export_id,
                    "namespace": self._namespace,
                    "dimensions": self._dimensions,
                    "total": len(ids),
                    "created_at": time.time(),
                },
            )

            with self._lock:
                state = self._exports.get(export_id)
                if (
                    state is None
                    or state.get("status") != "preparing"
                    or stop_event.is_set()
                ):
                    shutil.rmtree(temp_dir, ignore_errors=True)
                    return
                if os.path.isdir(final_dir):
                    shutil.rmtree(final_dir, ignore_errors=True)
                # The rename is what publishes the snapshot: a reader either
                # sees a complete directory or none at all.
                os.replace(temp_dir, final_dir)
                state.update(
                    {
                        "status": "ready",
                        "total": len(ids),
                        "ids": tuple(ids),
                        "last_access": time.monotonic(),
                        "finished_at": time.time(),
                    }
                )
        except Exception as exc:  # noqa: BLE001 - reported through the status call
            shutil.rmtree(temp_dir, ignore_errors=True)
            with self._lock:
                state = self._exports.get(export_id)
                if state is not None and state.get("status") == "preparing":
                    state.update(
                        {
                            "status": "failed",
                            "error": str(exc),
                            "last_access": time.monotonic(),
                        }
                    )
            logger.exception("%s export snapshot failed", self._namespace)

    @staticmethod
    def _write_json(path: str, payload: Any) -> None:
        with open(path + ".tmp", "w", encoding="utf-8") as stream:
            json.dump(payload, stream, ensure_ascii=False, separators=(",", ":"))
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(path + ".tmp", path)

    def start(self, export_id: str) -> Dict[str, Any]:
        """Begin a snapshot without blocking the caller's IPC window."""
        self._cleanup()
        export_id = validate_export_id(export_id or secrets.token_hex(16))
        now_mono = time.monotonic()
        with self._lock:
            if self._exports.get(export_id) is not None:
                return self.status(export_id)
            if len(self._exports) >= MAX_CONCURRENT_EXPORTS:
                oldest = min(
                    self._exports,
                    key=lambda key: self._exports[key].get("created_mono", now_mono),
                )
                self._exports.pop(oldest, None)
            stop_event = threading.Event()
            self._exports[export_id] = {
                "status": "preparing",
                "total": 0,
                "ids": None,
                "error": None,
                "created_mono": now_mono,
                "created_at": time.time(),
                "last_access": now_mono,
                "stop_event": stop_event,
            }
        self._executor.submit(self._build, export_id, stop_event)
        return {"export_id": export_id, "state": "preparing", "total": 0}

    def _restore(self, export_id: str) -> Optional[Dict[str, Any]]:
        export_dir = self._export_dir(export_id)
        manifest_path = os.path.join(export_dir, "manifest.json")
        ids_path = os.path.join(export_dir, "ids.json")
        if not (os.path.isfile(manifest_path) and os.path.isfile(ids_path)):
            return None
        try:
            with open(manifest_path, "r", encoding="utf-8") as stream:
                manifest = json.load(stream)
            with open(ids_path, "r", encoding="utf-8") as stream:
                ids = tuple(str(value) for value in json.load(stream))
            if manifest.get("export_id") != export_id or int(
                manifest.get("total", -1)
            ) != len(ids):
                raise ValueError("collection export manifest does not match ids")
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
        except Exception:  # noqa: BLE001 - a corrupt artifact restarts the export
            logger.exception("failed to restore %s export %s", self._namespace, export_id)
            return None

    def status(self, export_id: str) -> Dict[str, Any]:
        export_id = validate_export_id(export_id)
        self._cleanup()
        with self._lock:
            state = self._exports.get(export_id)
        if state is None:
            restored = self._restore(export_id)
            if restored is not None:
                with self._lock:
                    self._exports[export_id] = restored
                    state = restored
        if state is None:
            return {"export_id": export_id, "state": "missing", "total": 0}

        with self._lock:
            state = self._exports[export_id]
            elapsed = time.monotonic() - state.get("created_mono", time.monotonic())
            if (
                state.get("status") == "preparing"
                and elapsed > EXPORT_LOGICAL_TIMEOUT_SECS
            ):
                state["status"] = "timed_out"
                state["error"] = (
                    "collection ID snapshot exceeded its 10 minute deadline"
                )
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

    def _resolve_ready(self, export_id: str) -> Sequence[str]:
        with self._lock:
            snapshot = self._exports.get(export_id)
        if snapshot is None:
            snapshot = self._restore(export_id)
            if snapshot is not None:
                with self._lock:
                    self._exports[export_id] = snapshot
        if snapshot is None:
            raise ValueError("unknown or expired collection export")
        with self._lock:
            snapshot = self._exports[export_id]
            if snapshot.get("status") != "ready":
                raise ValueError(f"collection export is {snapshot.get('status')}")
            snapshot["last_access"] = time.monotonic()
            return snapshot["ids"]

    def page(self, export_id: str, cursor: int = 0, limit: int = 128) -> Dict[str, Any]:
        """Export one page in snapshot order.

        Vectors travel as one little-endian float32 blob wrapped in Base64
        rather than as tens of thousands of JSON floats, which keeps a 128-row
        page to a few hundred kilobytes of pipe traffic.

        A row that vanished between the snapshot and this call is reported in
        ``missing_ids``, and one whose stored vector does not decode is reported
        in ``errors``. Neither stops the walk: both are terminal diagnostics the
        importer records and counts.
        """
        export_id = validate_export_id(export_id)
        cursor = max(0, int(cursor))
        limit = max(1, min(MAX_PAGE_ROWS, int(limit)))
        snapshot_ids = self._resolve_ready(export_id)
        page_ids = list(snapshot_ids[cursor : cursor + limit])
        total = len(snapshot_ids)

        if not page_ids:
            return {
                "ids": [],
                "dimensions": self._dimensions,
                "embeddings_f32_le_b64": "",
                "missing_ids": [],
                "errors": [],
                "next_cursor": cursor,
                "done": True,
                "total": total,
            }

        results = self._collection_getter().get(ids=page_ids, include=["embeddings"])
        returned_ids = [str(doc_id) for doc_id in (results.get("ids") or [])]
        embeddings = results.get("embeddings")
        if embeddings is None:
            embeddings = []
        vectors_by_id: Dict[str, np.ndarray] = {}
        errors: List[Dict[str, str]] = []
        for doc_id, vector in zip(returned_ids, embeddings):
            try:
                row = np.asarray(vector, dtype="<f4").reshape(-1)
                if row.shape[0] != self._dimensions:
                    raise ValueError(
                        f"expected {self._dimensions} dimensions, got {row.shape[0]}"
                    )
                vectors_by_id[doc_id] = row
            except Exception as exc:  # noqa: BLE001 - quarantined, not fatal
                errors.append({"id": doc_id, "error": str(exc)})

        # Snapshot order, not Chroma's return order: the importer pairs ids with
        # vectors positionally.
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
            "dimensions": self._dimensions,
            "embeddings_f32_le_b64": embeddings_b64,
            "missing_ids": missing_ids,
            "errors": errors,
            "next_cursor": next_cursor,
            "done": next_cursor >= total,
            "total": total,
        }

    def finish(self, export_id: str) -> bool:
        """Release memory and persistent artifacts after a completed migration."""
        export_id = validate_export_id(export_id)
        with self._lock:
            state = self._exports.pop(export_id, None)
            if state is not None:
                state["stop_event"].set()
        shutil.rmtree(self._export_dir(export_id), ignore_errors=True)
        shutil.rmtree(self._export_dir(export_id) + ".tmp", ignore_errors=True)
        return state is not None
