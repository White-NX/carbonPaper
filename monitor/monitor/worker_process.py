"""Restartable worker process for OCR, embedding, and classification.

The parent monitor process keeps the named-pipe server responsive. Expensive
model calls run here so a stuck OCR/embedding/classifier call can be terminated
by killing this process and starting a fresh one for the next request.
"""

from __future__ import annotations

import datetime
import io
import logging
import os
import queue
import threading
import time
import traceback
from collections import OrderedDict
from typing import Any, Dict, Optional

from .worker_supervisor import WorkerSupervisor, attach_response_metadata


logger = logging.getLogger(__name__)
WORKER_PROTOCOL_VERSION = 2


def _python_owns_clip_encoding() -> bool:
    """Return the CLIP capture owner latched into this worker at spawn."""
    return os.environ.get("CARBONPAPER_CLIP_RUNTIME", "rust").strip().lower() == "python"


class OcrPostprocessQueue:
    """Bounded best-effort OCR post-processing queue.

    The capture commit path must not wait for vector indexing or classification.
    Jobs are dropped when the queue is full; OCR results have already been
    returned to Rust by then.
    """

    def __init__(self, ocr_worker, classifier, maxsize: Optional[int] = None):
        self.ocr_worker = ocr_worker
        self.classifier = classifier
        self.maxsize = maxsize or int(os.environ.get("CARBONPAPER_OCR_POSTPROCESS_QUEUE_MAX", "64"))
        self._queue = queue.Queue(maxsize=max(1, self.maxsize))
        self._stop = threading.Event()
        self._thread: Optional[threading.Thread] = None
        self.dropped = 0
        self.processed = 0
        self.failed = 0
        self.vector_failed = 0
        self.vector_retry_enqueued = 0
        self.last_indexing_error: Optional[str] = None
        self.last_indexing_error_at: Optional[float] = None
        self.max_vector_retry_backlog = max(
            1,
            int(os.environ.get("CARBONPAPER_VECTOR_RETRY_BACKLOG_MAX", "32") or "32"),
        )
        self._vector_retry_backlog = OrderedDict()
        self._stats_lock = threading.Lock()

    def start(self):
        if self._thread and self._thread.is_alive():
            return
        self._thread = threading.Thread(
            target=self._run,
            name="ocr-postprocess",
            daemon=True,
        )
        self._thread.start()

    def stop(self, timeout: float = 2.0):
        self._stop.set()
        if self._thread:
            self._thread.join(timeout=timeout)

    def enqueue(self, job: Dict[str, Any]) -> bool:
        try:
            self._queue.put_nowait(job)
            logger.info(
                "[DIAG:ocr_postprocess] enqueued screenshot_id=%s text_len=%s queue_size=%s",
                job.get("screenshot_id"),
                len(job.get("ocr_text", "") or ""),
                self._queue.qsize(),
            )
            return True
        except queue.Full:
            self.dropped += 1
            logger.warning(
                "[DIAG:ocr_postprocess] queue full; dropped screenshot_id=%s dropped=%s maxsize=%s",
                job.get("screenshot_id"),
                self.dropped,
                self.maxsize,
            )
            return False

    def _run(self):
        while not self._stop.is_set():
            try:
                job = self._queue.get(timeout=0.2)
            except queue.Empty:
                maybe_unload = getattr(self.ocr_worker, "maybe_unload_idle_ocr_engine", None)
                if callable(maybe_unload):
                    maybe_unload()
                continue
            try:
                if job.get("_persistent_postprocess"):
                    try:
                        from storage_client import get_storage_client
                        sc = get_storage_client()
                        if sc:
                            sc.set_ocr_postprocess_status(
                                int(job.get("screenshot_id")), "processing", None
                            )
                    except Exception:
                        pass
                self._handle_job(job)
                self.processed += 1
            except Exception as exc:
                self.failed += 1
                if job.get("_persistent_postprocess"):
                    try:
                        from storage_client import get_storage_client
                        sc = get_storage_client()
                        if sc:
                            sc.record_ocr_postprocess_retry(
                                int(job.get("screenshot_id")), str(exc)
                            )
                    except Exception:
                        pass
                logger.warning(
                    "[DIAG:ocr_postprocess] failed screenshot_id=%s error=%s",
                    job.get("screenshot_id"),
                    exc,
                    exc_info=True,
                )
            finally:
                self._queue.task_done()

    def _record_vector_failure(self, job: Dict[str, Any], reason: str):
        screenshot_id = job.get("screenshot_id")
        key = str(screenshot_id or job.get("image_hash") or time.time())
        retry_job = dict(job)
        retry_job["_vector_retry_only"] = True
        with self._stats_lock:
            self.vector_failed += 1
            self.last_indexing_error = reason
            self.last_indexing_error_at = time.time()
            if key in self._vector_retry_backlog:
                self._vector_retry_backlog.move_to_end(key)
            self._vector_retry_backlog[key] = retry_job
            while len(self._vector_retry_backlog) > self.max_vector_retry_backlog:
                self._vector_retry_backlog.popitem(last=False)

    def _clear_vector_failure(self, job: Dict[str, Any]):
        screenshot_id = job.get("screenshot_id")
        key = str(screenshot_id or job.get("image_hash") or "")
        if not key:
            return
        with self._stats_lock:
            self._vector_retry_backlog.pop(key, None)

    def _handle_vector_indexing(self, job: Dict[str, Any]) -> bool:
        screenshot_id = job.get("screenshot_id")
        image_hash = job.get("image_hash", "")
        ocr_text = job.get("ocr_text", "")

        # M2.5 step 8: Rust owns CLIP encoding on the capture path. Running both
        # would embed every screenshot twice — once here on DirectML, once in
        # Rust on CPU — and write conflicting vectors into the one collection
        # they both target. Rust reads its registry key on every pass while this
        # is read once at spawn, so the value the running monitor was handed is
        # what decides ownership until the process exits; that is the same
        # arbitration `CARBONPAPER_RERANK_RUNTIME` uses for the Smart Cluster
        # queue, and for the same reason.
        if not _python_owns_clip_encoding():
            return True

        from PIL import Image

        image_bytes = job.get("image_bytes") or b""
        if self.ocr_worker.enable_vector_store and self.ocr_worker.vector_store and ocr_text.strip():
            try:
                image_pil = Image.open(io.BytesIO(image_bytes))
                image_pil.load()
                t_vector = time.perf_counter()
                result = self.ocr_worker.vector_store.add_image(
                    image_path=f"memory://{image_hash}",
                    image=image_pil,
                    metadata={
                        "window_title": job.get("window_title", ""),
                        "process_name": job.get("process_name", ""),
                        "timestamp": job.get("timestamp", 0),
                    },
                    ocr_text=ocr_text,
                )
                if not isinstance(result, dict) or result.get("status") != "success":
                    reason = result.get("error") if isinstance(result, dict) else str(result)
                    self._record_vector_failure(job, reason or "Vector index write failed")
                    logger.warning(
                        "[DIAG:ocr_postprocess] vector add failed screenshot_id=%s reason=%s",
                        screenshot_id,
                        reason,
                    )
                    return False
                self._clear_vector_failure(job)
                logger.debug(
                    "[DIAG:ocr_postprocess] vector add done screenshot_id=%s elapsed=%.3fs",
                    screenshot_id,
                    time.perf_counter() - t_vector,
                )
                return True
            except Exception as exc:
                self._record_vector_failure(job, str(exc))
                logger.warning("Vector store add failed: %s", exc)
                return False
        return True

    def _handle_job(self, job: Dict[str, Any]):
        from storage_client import get_storage_client
        from . import config

        screenshot_id = job.get("screenshot_id")
        ocr_text = job.get("ocr_text", "")
        started = time.perf_counter()

        vector_ok = self._handle_vector_indexing(job)

        if job.get("_vector_retry_only"):
            if job.get("_persistent_postprocess"):
                sc = get_storage_client()
                if sc:
                    if vector_ok:
                        sc.set_ocr_postprocess_status(int(screenshot_id), "completed", None)
                    else:
                        sc.record_ocr_postprocess_retry(
                            int(screenshot_id), "Vector indexing failed"
                        )
            logger.info(
                "[DIAG:ocr_postprocess] vector retry done screenshot_id=%s total=%.3fs queue_size=%s",
                screenshot_id,
                time.perf_counter() - started,
                self._queue.qsize(),
            )
            return

        if self.classifier and config.CLASSIFICATION_ENABLED:
            try:
                t_classify = time.perf_counter()
                category, category_confidence = self.classifier.classify(
                    title=job.get("window_title", ""),
                    ocr_text=ocr_text,
                    process_name=job.get("process_name", ""),
                )
                category_confidence = round(category_confidence, 4)
                if category:
                    sc = get_storage_client()
                    if sc:
                        ok = sc.update_screenshot_category(
                            int(screenshot_id),
                            category,
                            category_confidence,
                        )
                        if not ok:
                            logger.warning(
                                "[DIAG:ocr_postprocess] category update skipped/failed screenshot_id=%s category=%s",
                                screenshot_id,
                                category,
                            )
                logger.info(
                    "[DIAG:ocr_postprocess] classify done screenshot_id=%s elapsed=%.3fs category=%s confidence=%s",
                    screenshot_id,
                    time.perf_counter() - t_classify,
                    category,
                    category_confidence,
                )
            except Exception as exc:
                logger.warning("Classification failed: %s", exc)

        logger.info(
            "[DIAG:ocr_postprocess] done screenshot_id=%s total=%.3fs queue_size=%s",
            screenshot_id,
            time.perf_counter() - started,
            self._queue.qsize(),
        )
        if job.get("_persistent_postprocess"):
            sc = get_storage_client()
            if sc:
                if vector_ok:
                    sc.set_ocr_postprocess_status(int(screenshot_id), "completed", None)
                else:
                    sc.record_ocr_postprocess_retry(
                        int(screenshot_id), "Vector indexing failed"
                    )

    def retry_failed_vector_indexing(self, limit: int = 32) -> Dict[str, Any]:
        limit = max(1, min(int(limit or 32), self.max_vector_retry_backlog))
        enqueued = 0
        with self._stats_lock:
            retry_items = list(self._vector_retry_backlog.items())[:limit]
        for _key, job in retry_items:
            try:
                self._queue.put_nowait(dict(job))
                enqueued += 1
            except queue.Full:
                break
        with self._stats_lock:
            self.vector_retry_enqueued += enqueued
        return {
            "status": "success",
            "requested": len(retry_items),
            "enqueued": enqueued,
            "queue_full": enqueued < len(retry_items),
            "backlog_count": self.vector_retry_backlog_count(),
        }

    def vector_retry_backlog_count(self) -> int:
        with self._stats_lock:
            return len(self._vector_retry_backlog)

    def status_snapshot(self) -> Dict[str, Any]:
        with self._stats_lock:
            last_error = self.last_indexing_error
            last_error_at = self.last_indexing_error_at
            backlog_count = len(self._vector_retry_backlog)
        return {
            "queue_size": self._queue.qsize(),
            "queue_max_size": self._queue.maxsize,
            "dropped": self.dropped,
            "processed": self.processed,
            "failed": self.failed,
            "vector_failed": self.vector_failed,
            "vector_retry_enqueued": self.vector_retry_enqueued,
            "vector_retry_backlog_count": backlog_count,
            "last_indexing_error": last_error,
            "last_indexing_error_at": last_error_at,
        }


def _json_safe(obj):
    if isinstance(obj, (datetime.datetime, datetime.date)):
        return obj.isoformat()
    return obj


def _enqueue_ocr_postprocess(req: Dict[str, Any], postprocess_queue: Optional[OcrPostprocessQueue]) -> Dict[str, Any]:
    """Enqueue vector/classification work for OCR produced by Rust.

    Image bytes are fetched through authenticated reverse IPC only while the
    legacy Python CLIP encoder owns the capture path. Rust-owned CLIP jobs still
    enter this queue for text-only classification and persistent status updates.
    """
    from storage_client import get_storage_client

    screenshot_id = req.get("screenshot_id")
    if screenshot_id is None:
        return {"error": "screenshot_id is required"}
    if not postprocess_queue:
        return {"error": "OCR postprocess queue is unavailable"}
    set_provider = getattr(postprocess_queue.ocr_worker, "set_rust_ocr_provider_active", None)
    if callable(set_provider):
        set_provider(bool(req.get("rust_provider_active", True)))
    sc = get_storage_client()
    if not sc:
        return {"error": "Storage client not available"}
    image_bytes = b""
    if _python_owns_clip_encoding():
        response = sc.get_temp_image_bytes(int(screenshot_id))
        if response.get("status") != "success":
            return {"error": f"Failed to fetch image: {response.get('error', 'unknown')}"}
        image_bytes = response.get("data", {}).get("image_bytes")
        if not image_bytes:
            return {"error": "No image data returned from storage"}
    enqueued = postprocess_queue.enqueue({
        "screenshot_id": int(screenshot_id),
        "image_hash": req.get("image_hash", ""),
        "window_title": req.get("window_title", ""),
        "process_name": req.get("process_name", ""),
        "timestamp": req.get("timestamp", 0),
        "ocr_text": req.get("ocr_text", ""),
        "image_bytes": image_bytes,
        "_persistent_postprocess": True,
    })
    return {
        "status": "success",
        "postprocess_enqueued": bool(enqueued),
        "worker_protocol": WORKER_PROTOCOL_VERSION,
    }


#: The CLIP vector commands, which only the model worker process can answer.
#:
#: The `screenshots` collection lives inside this process, so every one of these
#: has to be dispatched here rather than in the monitor's own handler. Naming the
#: set once keeps the proxy, the dispatcher, and the contract test from drifting
#: into three different opinions about what the set contains.
CLIP_VECTOR_COMMANDS = (
    "start_clip_vectors_export",
    "get_clip_vectors_export_status",
    "export_clip_vectors_page",
    "finish_clip_vectors_export",
    "upsert_clip_vectors",
)

#: Calls that may have to cold-start the model worker: the migration's first
#: export, and the capture-path mirror whenever the child is not already up.
#: The supervisor's own `ready_timeout` is 180s, so a smaller budget here would
#: time the request out mid-startup and kill the child that was about to become
#: ready. For the migration that matters twice over, because a failed run does
#: not settle its sentinel — it retries under maintenance mode at every launch.
CLIP_VECTOR_COLD_START_TIMEOUT = 180.0
#: A page is 128 rows of 512 float32s read out of Chroma and Base64-encoded.
CLIP_EXPORT_PAGE_TIMEOUT = 120.0
#: Status polls and snapshot release, which always follow a start on a worker
#: that is warm by then.
CLIP_EXPORT_CONTROL_TIMEOUT = 60.0


def _handle_clip_vector_command(command: str, msg: Dict[str, Any], ocr_worker) -> Dict[str, Any]:
    """Answer one CLIP snapshot-export or vector-mirror command.

    This is where "is the vector store enabled" can honestly be asked. The
    monitor process holds a `RestartableModelWorker` proxy, not an `OCRService`,
    so the store it would inspect there is always absent — the question is only
    answerable in the process that owns the collection.

    Results are nested under `result` instead of being merged into the response.
    The worker protocol owns the top level (`status`, `error`, `_request_id`),
    and `get_snapshot_export_status` already returns a `state` key; one future
    exporter key named like a protocol field would silently corrupt the reply.
    """
    store = getattr(ocr_worker, "vector_store", None)
    if not getattr(ocr_worker, "enable_vector_store", False) or not store:
        return {"error": "Vector store not enabled"}

    export_id = msg.get("export_id", "")
    try:
        if command == "start_clip_vectors_export":
            return {"status": "success", "result": store.start_snapshot_export(export_id)}
        if command == "get_clip_vectors_export_status":
            return {"status": "success", "result": store.get_snapshot_export_status(export_id)}
        if command == "export_clip_vectors_page":
            return {
                "status": "success",
                "result": store.export_snapshot_page(
                    export_id,
                    cursor=msg.get("cursor", 0),
                    limit=msg.get("limit", 128),
                ),
            }
        if command == "finish_clip_vectors_export":
            return {
                "status": "success",
                "result": {"released": bool(store.finish_snapshot_export(export_id))},
            }
        if command == "upsert_clip_vectors":
            return {
                "status": "success",
                "result": {"written": int(store.upsert_clip_vectors(msg.get("records") or []))},
            }
        # Reached when a name is added to CLIP_VECTOR_COMMANDS without being
        # handled here. Falling through to the last branch instead would run an
        # unrelated command's body against the new one's arguments.
        return {"error": f"Unknown CLIP vector command: {command}"}
    except Exception as exc:
        logger.exception("%s failed", command)
        return {"error": str(exc)}


def _worker_main(conn, storage_pipe: Optional[str], data_dir: str, env: Dict[str, str]):
    os.environ.update(env or {})
    if data_dir:
        os.environ["CARBONPAPER_DATA_DIR"] = data_dir

    try:
        from logging_config import setup_logging
        setup_logging()
    except Exception:
        pass

    try:
        from storage_client import init_storage_client
        from ocr_service import OCRService
        from classifier import ClassificationService
        from .config import update_feature_config

        update_feature_config(
            os.environ.get("CARBONPAPER_CLUSTERING_ENABLED", "true").lower() in ("1", "true", "yes", "on"),
            os.environ.get("CARBONPAPER_CLASSIFICATION_ENABLED", "true").lower() in ("1", "true", "yes", "on"),
        )

        if storage_pipe:
            init_storage_client(storage_pipe)

        try:
            import chromadb
            from chromadb.config import Settings as ChromaSettings
            shared_chroma_client = chromadb.PersistentClient(
                path=os.path.join(data_dir, "chroma_db"),
                settings=ChromaSettings(anonymized_telemetry=False),
            )
        except Exception as exc:
            logger.error("Worker ChromaDB init failed: %s", exc)
            shared_chroma_client = None

        ocr_worker = OCRService(
            vector_db_path=os.path.join(data_dir, "chroma_db"),
            storage_pipe=storage_pipe,
            chroma_client=shared_chroma_client,
        )
        ocr_worker.start()

        try:
            classifier = ClassificationService(anchors_path=os.path.join(data_dir, "anchors.json"))
        except Exception as exc:
            logger.warning("Worker classifier init failed: %s", exc)
            classifier = None

        postprocess_queue = OcrPostprocessQueue(ocr_worker, classifier)
        postprocess_queue.start()

        conn.send({"status": "ready", "worker_protocol": WORKER_PROTOCOL_VERSION})
    except Exception as exc:
        conn.send({"status": "error", "error": str(exc), "traceback": traceback.format_exc()})
        return

    while True:
        try:
            msg = conn.recv()
        except EOFError:
            return
        command = msg.get("command")

        def send_response(response: Dict[str, Any]):
            conn.send(attach_response_metadata(msg, response))

        try:
            if command == "stop":
                postprocess_queue.stop()
                send_response({"status": "success"})
                return
            if command == "enqueue_ocr_postprocess":
                send_response(_enqueue_ocr_postprocess(msg.get("request", {}), postprocess_queue))
            elif command == "get_stats":
                send_response({"status": "success", "stats": ocr_worker.get_stats()})
            elif command == "get_index_health":
                storage_ipc = None
                try:
                    from storage_client import get_storage_client
                    sc = get_storage_client()
                    if sc and hasattr(sc, "ipc_health_snapshot"):
                        storage_ipc = sc.ipc_health_snapshot()
                except Exception as exc:
                    storage_ipc = {"error": str(exc)}
                send_response({
                    "status": "success",
                    "stats": ocr_worker.get_stats(),
                    "postprocess": postprocess_queue.status_snapshot(),
                    "worker_storage_ipc": storage_ipc,
                })
            elif command == "retry_vector_indexing":
                limit = int(msg.get("limit", 32) or 32)
                send_response(postprocess_queue.retry_failed_vector_indexing(limit=limit))
            elif command == "search_by_natural_language":
                args = msg.get("args", {})
                send_response({
                    "status": "success",
                    "results": ocr_worker.search_by_natural_language(**args),
                })
            elif command == "delete_vector_image":
                image_hash = msg.get("image_hash", "")
                ok = False
                if ocr_worker.vector_store:
                    ok = bool(ocr_worker.vector_store.delete_image(f"memory://{image_hash}"))
                send_response({"status": "success", "ok": ok})
            elif command in CLIP_VECTOR_COMMANDS:
                send_response(_handle_clip_vector_command(command, msg, ocr_worker))
            elif command == "classify":
                if not classifier:
                    send_response({"error": "Classification service not initialised"})
                else:
                    args = msg.get("args", {})
                    category, confidence = classifier.classify(**args)
                    send_response({"status": "success", "category": category, "confidence": confidence})
            elif command == "classify_debug":
                if not classifier:
                    send_response({"error": "Classification service not initialised"})
                else:
                    send_response({"status": "success", "data": classifier.classify_debug(**msg.get("args", {}))})
            elif command == "add_anchor":
                if not classifier:
                    send_response({"error": "Classification service not initialised"})
                else:
                    send_response({"status": "success", "data": classifier.add_anchor(**msg.get("args", {}))})
            elif command == "remove_anchor":
                if not classifier:
                    send_response({"error": "Classification service not initialised"})
                else:
                    removed = classifier.remove_anchor(msg.get("category", ""), msg.get("title", ""))
                    send_response({"status": "success", "removed": removed})
            elif command == "remove_local_anchors_by_process":
                if not classifier:
                    send_response({"error": "Classification service not initialised"})
                else:
                    removed_count = classifier.remove_local_anchors_by_process(
                        msg.get("category", ""),
                        msg.get("process_name", ""),
                    )
                    send_response({"status": "success", "removed_count": removed_count})
            elif command == "get_categories":
                if not classifier:
                    send_response({"error": "Classification service not initialised"})
                else:
                    send_response({"status": "success", "categories": classifier.get_categories()})
            elif command == "get_anchors":
                if not classifier:
                    send_response({"error": "Classification service not initialised"})
                else:
                    send_response({"status": "success", "anchors": classifier.get_anchors()})
            else:
                send_response({"error": f"Unknown worker command: {command}"})
        except Exception as exc:
            send_response({"error": str(exc), "traceback": traceback.format_exc()})


class RestartableModelWorker(WorkerSupervisor):
    def __init__(self, storage_pipe: Optional[str], data_dir: str, env: Optional[Dict[str, str]] = None):
        self.storage_pipe = storage_pipe
        self.data_dir = data_dir
        self.env = env or {}
        self._stats = {"processed_count": 0, "failed_count": 0, "total_texts_found": 0, "start_time": None}
        self.stats = self._stats
        # `enable_vector_store` says the store exists somewhere and is reachable
        # through this proxy, which is true. There is deliberately no
        # `vector_store` attribute to go with it: the object lives in the child
        # process, and a `None` standing in for it here reads as "the store is
        # disabled" to every caller that checks it. That is precisely how the
        # CLIP export commands came to answer "Vector store not enabled" on
        # every machine. Without the attribute, reaching for the store in this
        # process raises instead of quietly degrading.
        self.enable_vector_store = True
        super().__init__(
            name="CarbonModelWorker",
            target=_worker_main,
            args=(self.storage_pipe, self.data_dir, self.env),
            ready_timeout=180.0,
            stop_timeout=2.0,
            kill_timeout=5.0,
            log=logger,
        )

    def request(self, command: str, payload: Optional[Dict[str, Any]] = None, timeout: float = 120.0):
        try:
            result = super().request(
                command,
                payload,
                timeout=timeout,
                start_timeout=max(30.0, min(180.0, timeout)),
            )
        except Exception:
            self._stats["failed_count"] += 1
            raise
        if command == "enqueue_ocr_postprocess" and result.get("status") == "success":
            self._stats["processed_count"] += 1
        elif result.get("error"):
            self._stats["failed_count"] += 1
        return result

    def get_stats(self):
        # Status polling must never start the model worker. Cold-starting OCR,
        # vector, or classifier models from a cheap health check blocks the
        # monitor pipe long enough for Rust-side status callers to time out.
        stats = dict(self._stats)
        stats["watchdog"] = self.status_snapshot()
        return stats

    def get_index_health(self, refresh: bool = False):
        snapshot = self.status_snapshot()
        if not refresh and not snapshot.get("alive"):
            return {
                "status": "success",
                "worker_available": True,
                "worker_started": False,
                "stats": self.get_stats(),
                "postprocess": None,
            }

        result = self.request("get_index_health", timeout=30)
        if result.get("status") == "success":
            result["worker_available"] = True
            result["worker_started"] = True
            return result
        raise RuntimeError(result.get("error", "Model worker index health failed"))

    def retry_vector_indexing(self, limit: int = 32):
        result = self.request("retry_vector_indexing", {"limit": int(limit or 32)}, timeout=30)
        if result.get("status") == "success":
            return result
        raise RuntimeError(result.get("error", "Model worker vector retry failed"))

    def pause(self):
        logger.info("Model worker proxy paused")

    def resume(self):
        logger.info("Model worker proxy resumed")

    def search_by_natural_language(
        self,
        query: str,
        n_results: int = 10,
        offset: int = 0,
        process_names=None,
        start_time=None,
        end_time=None,
    ):
        result = self.request(
            "search_by_natural_language",
            {
                "args": {
                    "query": query,
                    "n_results": n_results,
                    "offset": offset,
                    "process_names": process_names,
                    "start_time": start_time,
                    "end_time": end_time,
                }
            },
            timeout=max(30.0, float(os.environ.get("CARBONPAPER_OCR_TIMEOUT_SECS", "120") or "120")),
        )
        if result.get("status") == "success":
            return result.get("results", [])
        raise RuntimeError(result.get("error", "Model worker search failed"))

    def delete_vector_image(self, image_hash: str) -> bool:
        result = self.request("delete_vector_image", {"image_hash": image_hash}, timeout=30)
        return bool(result.get("ok"))

    # ----- CLIP image-vector snapshot export and mirror -----
    #
    # Five forwards into the process that owns the `screenshots` collection.
    # Each returns the payload the monitor's handler flattens into its reply, so
    # the wire format Rust parses is unchanged by the hop through here.

    def _clip_vector_request(
        self,
        command: str,
        payload: Optional[Dict[str, Any]],
        timeout: float,
    ) -> Dict[str, Any]:
        result = self.request(command, payload, timeout=timeout)
        if result.get("status") == "success":
            return result.get("result") or {}
        # A refusal from the child — a disabled store, a snapshot the exporter
        # has forgotten — arrives as an `error` field, and the monitor handler
        # turns this exception back into one. The text survives the round trip
        # because Rust matches on it.
        raise RuntimeError(result.get("error") or f"Model worker {command} failed")

    def start_clip_vectors_export(self, export_id: str) -> Dict[str, Any]:
        return self._clip_vector_request(
            "start_clip_vectors_export",
            {"export_id": export_id},
            timeout=CLIP_VECTOR_COLD_START_TIMEOUT,
        )

    def get_clip_vectors_export_status(self, export_id: str) -> Dict[str, Any]:
        return self._clip_vector_request(
            "get_clip_vectors_export_status",
            {"export_id": export_id},
            timeout=CLIP_EXPORT_CONTROL_TIMEOUT,
        )

    def export_clip_vectors_page(
        self,
        export_id: str,
        cursor: int = 0,
        limit: int = 128,
    ) -> Dict[str, Any]:
        return self._clip_vector_request(
            "export_clip_vectors_page",
            {"export_id": export_id, "cursor": cursor, "limit": limit},
            timeout=CLIP_EXPORT_PAGE_TIMEOUT,
        )

    def finish_clip_vectors_export(self, export_id: str) -> bool:
        result = self._clip_vector_request(
            "finish_clip_vectors_export",
            {"export_id": export_id},
            timeout=CLIP_EXPORT_CONTROL_TIMEOUT,
        )
        return bool(result.get("released"))

    def upsert_clip_vectors(self, records) -> int:
        result = self._clip_vector_request(
            "upsert_clip_vectors",
            {"records": records},
            timeout=CLIP_VECTOR_COLD_START_TIMEOUT,
        )
        return int(result.get("written", 0))

    def classify(self, title: str, ocr_text: str, process_name: str = ""):
        result = self.request(
            "classify",
            {"args": {"title": title, "ocr_text": ocr_text, "process_name": process_name}},
            timeout=30,
        )
        if result.get("status") == "success":
            return result.get("category"), result.get("confidence")
        raise RuntimeError(result.get("error", "Model worker classify failed"))

    def classify_debug(self, title: str, ocr_text: str, process_name: str = ""):
        result = self.request(
            "classify_debug",
            {"args": {"title": title, "ocr_text": ocr_text, "process_name": process_name}},
            timeout=30,
        )
        if result.get("status") == "success":
            return result.get("data", {})
        raise RuntimeError(result.get("error", "Model worker classify_debug failed"))

    def add_anchor(self, category: str, title: str, ocr_text: str = "", old_category=None, process_name: str = ""):
        result = self.request(
            "add_anchor",
            {
                "args": {
                    "category": category,
                    "title": title,
                    "ocr_text": ocr_text,
                    "old_category": old_category,
                    "process_name": process_name,
                }
            },
            timeout=30,
        )
        if result.get("status") == "success":
            return result.get("data", {})
        raise RuntimeError(result.get("error", "Model worker add_anchor failed"))

    def remove_anchor(self, category: str, title: str):
        result = self.request("remove_anchor", {"category": category, "title": title}, timeout=30)
        if result.get("status") == "success":
            return result.get("removed", False)
        raise RuntimeError(result.get("error", "Model worker remove_anchor failed"))

    def remove_local_anchors_by_process(self, category: str, process_name: str):
        result = self.request(
            "remove_local_anchors_by_process",
            {"category": category, "process_name": process_name},
            timeout=30,
        )
        if result.get("status") == "success":
            return result.get("removed_count", 0)
        raise RuntimeError(result.get("error", "Model worker remove_local_anchors_by_process failed"))

    def get_categories(self):
        result = self.request("get_categories", timeout=30)
        if result.get("status") == "success":
            return result.get("categories", [])
        raise RuntimeError(result.get("error", "Model worker get_categories failed"))

    def get_anchors(self):
        result = self.request("get_anchors", timeout=30)
        if result.get("status") == "success":
            return result.get("anchors", {})
        raise RuntimeError(result.get("error", "Model worker get_anchors failed"))
