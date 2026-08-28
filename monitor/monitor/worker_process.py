"""Restartable Python worker for classification post-processing.

Rust owns screenshot capture, OCR, semantic/CLIP inference, and Smart Cluster
scoring.  The child process kept here exists for the remaining Python work:
anchor-based classification orchestration and durable post-processing status.
Legacy Chroma migration export is served by the monitor process itself and is
intentionally absent from this worker.
"""

from __future__ import annotations

import logging
import os
import queue
import threading
import time
import traceback
from typing import Any, Dict, Optional

from .worker_supervisor import WorkerSupervisor, attach_response_metadata

logger = logging.getLogger(__name__)
WORKER_PROTOCOL_VERSION = 3

_CLASSIFICATION_SCHEDULING_YIELDS = (
    "foreground_busy:",
    "background_busy:",
)


class _PostprocessDeferred(RuntimeError):
    """The job yielded its model slot and must remain durably pending."""


def _is_classification_scheduling_yield(error: Exception) -> bool:
    message = str(error)
    return any(marker in message for marker in _CLASSIFICATION_SCHEDULING_YIELDS)


class PostprocessQueue:
    """Bounded queue for classification after Rust has committed OCR text."""

    def __init__(self, classifier, maxsize: Optional[int] = None):
        configured = maxsize or int(
            os.environ.get("CARBONPAPER_POSTPROCESS_QUEUE_MAX", "64") or "64"
        )
        self.classifier = classifier
        self._queue = queue.Queue(maxsize=max(1, configured))
        self._stop = threading.Event()
        self._thread: Optional[threading.Thread] = None
        self.dropped = 0
        self.processed = 0
        self.failed = 0
        self.deferred = 0
        self._stats_lock = threading.Lock()

    def start(self):
        if self._thread and self._thread.is_alive():
            return
        self._thread = threading.Thread(
            target=self._run,
            name="classification-postprocess",
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
                "[DIAG:postprocess] enqueued screenshot_id=%s queue_size=%s",
                job.get("screenshot_id"),
                self._queue.qsize(),
            )
            return True
        except queue.Full:
            with self._stats_lock:
                self.dropped += 1
            logger.warning(
                "[DIAG:postprocess] queue full; dropped screenshot_id=%s",
                job.get("screenshot_id"),
            )
            return False

    @staticmethod
    def _storage_client():
        try:
            from storage_client import get_storage_client

            return get_storage_client()
        except Exception:
            return None

    def _set_status(self, job: Dict[str, Any], status: str, error: Optional[str] = None):
        if not job.get("_persistent_postprocess"):
            return
        storage = self._storage_client()
        if storage:
            storage.set_ocr_postprocess_status(
                int(job.get("screenshot_id")), status, error
            )

    def _record_retry(self, job: Dict[str, Any], error: str):
        if not job.get("_persistent_postprocess"):
            return
        storage = self._storage_client()
        if storage:
            storage.record_ocr_postprocess_retry(
                int(job.get("screenshot_id")), error
            )

    def _run(self):
        while not self._stop.is_set():
            try:
                job = self._queue.get(timeout=0.2)
            except queue.Empty:
                continue
            try:
                self._set_status(job, "processing")
                self._handle_job(job)
                with self._stats_lock:
                    self.processed += 1
                self._set_status(job, "completed")
            except _PostprocessDeferred as exc:
                with self._stats_lock:
                    self.deferred += 1
                try:
                    self._set_status(job, "pending", str(exc))
                except Exception:
                    logger.warning(
                        "[DIAG:postprocess] failed to persist deferred state screenshot_id=%s",
                        job.get("screenshot_id"),
                        exc_info=True,
                    )
                    try:
                        self._record_retry(job, str(exc))
                    except Exception:
                        logger.warning(
                            "[DIAG:postprocess] failed to persist deferred retry screenshot_id=%s",
                            job.get("screenshot_id"),
                            exc_info=True,
                        )
            except Exception as exc:
                with self._stats_lock:
                    self.failed += 1
                try:
                    self._record_retry(job, str(exc))
                except Exception:
                    pass
                logger.warning(
                    "[DIAG:postprocess] failed screenshot_id=%s error=%s",
                    job.get("screenshot_id"),
                    exc,
                    exc_info=True,
                )
            finally:
                self._queue.task_done()

    def _handle_job(self, job: Dict[str, Any]):
        from . import config

        if not self.classifier or not config.CLASSIFICATION_ENABLED:
            return

        started = time.perf_counter()
        try:
            category, confidence = self.classifier.classify(
                title=job.get("window_title", ""),
                ocr_text=job.get("ocr_text", ""),
                process_name=job.get("process_name", ""),
            )
        except Exception as exc:
            if _is_classification_scheduling_yield(exc):
                raise _PostprocessDeferred(str(exc)) from exc
            raise

        if category:
            storage = self._storage_client()
            if storage:
                storage.update_screenshot_category(
                    int(job.get("screenshot_id")),
                    category,
                    round(float(confidence), 4),
                )
        logger.info(
            "[DIAG:postprocess] classify done screenshot_id=%s elapsed=%.3fs category=%s confidence=%s",
            job.get("screenshot_id"),
            time.perf_counter() - started,
            category,
            confidence,
        )

    def status_snapshot(self) -> Dict[str, Any]:
        with self._stats_lock:
            stats = {
                "dropped": self.dropped,
                "processed": self.processed,
                "failed": self.failed,
                "deferred": self.deferred,
            }
        stats.update(
            {
                "queue_size": self._queue.qsize(),
                "queue_max_size": self._queue.maxsize,
            }
        )
        return stats


def _enqueue_ocr_postprocess(
    req: Dict[str, Any], postprocess_queue: Optional[PostprocessQueue]
) -> Dict[str, Any]:
    """Queue classification for OCR text committed by Rust."""

    screenshot_id = req.get("screenshot_id")
    if screenshot_id is None:
        return {"error": "screenshot_id is required"}
    if not postprocess_queue:
        return {"error": "Classification postprocess service is unavailable"}
    enqueued = postprocess_queue.enqueue(
        {
            "screenshot_id": int(screenshot_id),
            "window_title": req.get("window_title", ""),
            "process_name": req.get("process_name", ""),
            "timestamp": req.get("timestamp", 0),
            "ocr_text": req.get("ocr_text", ""),
            "_persistent_postprocess": True,
        }
    )
    return {
        "status": "success",
        "postprocess_enqueued": bool(enqueued),
        "worker_protocol": WORKER_PROTOCOL_VERSION,
    }


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
        from classifier import ClassificationService
        from .config import update_feature_config

        update_feature_config(
            os.environ.get("CARBONPAPER_CLUSTERING_ENABLED", "true").lower()
            in ("1", "true", "yes", "on"),
            os.environ.get("CARBONPAPER_CLASSIFICATION_ENABLED", "true").lower()
            in ("1", "true", "yes", "on"),
        )
        if storage_pipe:
            init_storage_client(storage_pipe)

        try:
            classifier = ClassificationService(
                anchors_path=os.path.join(data_dir, "anchors.json")
            )
        except Exception as exc:
            logger.warning("Worker classifier init failed: %s", exc)
            classifier = None

        postprocess_queue = PostprocessQueue(classifier)
        postprocess_queue.start()
        conn.send({"status": "ready", "worker_protocol": WORKER_PROTOCOL_VERSION})
    except Exception as exc:
        conn.send(
            {
                "status": "error",
                "error": str(exc),
                "traceback": traceback.format_exc(),
            }
        )
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
                send_response(
                    _enqueue_ocr_postprocess(msg.get("request", {}), postprocess_queue)
                )
            elif command == "update_feature_config":
                # Runtime feature-toggle sync from the monitor process. The
                # child snapshots its config from the environment at startup,
                # so a settings change must be forwarded for it to take effect
                # on jobs this process has not dequeued yet.
                from .config import update_feature_config as _update_feature_config

                args = msg.get("args", {})
                _update_feature_config(
                    bool(args.get("clustering_enabled", True)),
                    bool(args.get("classification_enabled", True)),
                )
                send_response({"status": "success"})
            elif command == "get_stats":
                send_response(
                    {
                        "status": "success",
                        "stats": {"postprocess": postprocess_queue.status_snapshot()},
                    }
                )
            elif command == "classify":
                if not classifier:
                    send_response({"error": "Classification service not initialised"})
                else:
                    args = msg.get("args", {})
                    category, confidence = classifier.classify(**args)
                    send_response(
                        {
                            "status": "success",
                            "category": category,
                            "confidence": confidence,
                        }
                    )
            elif command == "classify_debug":
                if not classifier:
                    send_response({"error": "Classification service not initialised"})
                else:
                    send_response(
                        {
                            "status": "success",
                            "data": classifier.classify_debug(
                                **msg.get("args", {})
                            ),
                        }
                    )
            elif command == "add_anchor":
                if not classifier:
                    send_response({"error": "Classification service not initialised"})
                else:
                    send_response(
                        {
                            "status": "success",
                            "data": classifier.add_anchor(**msg.get("args", {})),
                        }
                    )
            elif command == "remove_anchor":
                if not classifier:
                    send_response({"error": "Classification service not initialised"})
                else:
                    send_response(
                        {
                            "status": "success",
                            "removed": classifier.remove_anchor(
                                msg.get("category", ""), msg.get("title", "")
                            ),
                        }
                    )
            elif command == "remove_local_anchors_by_process":
                if not classifier:
                    send_response({"error": "Classification service not initialised"})
                else:
                    send_response(
                        {
                            "status": "success",
                            "removed_count": classifier.remove_local_anchors_by_process(
                                msg.get("category", ""),
                                msg.get("process_name", ""),
                            ),
                        }
                    )
            elif command == "get_categories":
                if not classifier:
                    send_response({"error": "Classification service not initialised"})
                else:
                    send_response(
                        {
                            "status": "success",
                            "categories": classifier.get_categories(),
                        }
                    )
            elif command == "get_anchors":
                if not classifier:
                    send_response({"error": "Classification service not initialised"})
                else:
                    send_response(
                        {"status": "success", "anchors": classifier.get_anchors()}
                    )
            else:
                send_response({"error": f"Unknown worker command: {command}"})
        except Exception as exc:
            send_response({"error": str(exc), "traceback": traceback.format_exc()})


class RestartableModelWorker(WorkerSupervisor):
    """Parent-side proxy for the classification/postprocess child."""

    def __init__(
        self,
        storage_pipe: Optional[str],
        data_dir: str,
        env: Optional[Dict[str, str]] = None,
    ):
        self.storage_pipe = storage_pipe
        self.data_dir = data_dir
        self.env = env or {}
        self._stats = {
            "processed_count": 0,
            "failed_count": 0,
            "start_time": None,
        }
        self.stats = self._stats
        super().__init__(
            name="CarbonModelWorker",
            target=_worker_main,
            args=(self.storage_pipe, self.data_dir, self.env),
            ready_timeout=180.0,
            stop_timeout=2.0,
            kill_timeout=5.0,
            log=logger,
        )

    def request(
        self,
        command: str,
        payload: Optional[Dict[str, Any]] = None,
        timeout: float = 120.0,
    ):
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
        stats = dict(self._stats)
        stats["watchdog"] = self.status_snapshot()
        return stats

    def update_feature_config(self, clustering_enabled: bool, classification_enabled: bool):
        """Propagate a runtime feature-toggle change to the child process.

        The child snapshots ``monitor.config`` from its environment at startup
        and never re-reads it, so without this forward a settings change made
        while the app is running would not reach the process that dequeues
        classification jobs. The environment snapshot is updated in place first
        — it is the same dict a restart pickles into a fresh child — so the
        change also survives worker restarts. Forwarding to a live child is
        best-effort: a failed notify still leaves the environment correct.
        """
        self.env["CARBONPAPER_CLUSTERING_ENABLED"] = str(bool(clustering_enabled))
        self.env["CARBONPAPER_CLASSIFICATION_ENABLED"] = str(bool(classification_enabled))
        if not self.is_running():
            return {"status": "deferred", "reason": "worker not running"}
        try:
            return self.request(
                "update_feature_config",
                {
                    "args": {
                        "clustering_enabled": bool(clustering_enabled),
                        "classification_enabled": bool(classification_enabled),
                    }
                },
                timeout=30,
            )
        except Exception as exc:
            logger.warning(
                "Feature-config forward to model worker failed (env snapshot updated): %s",
                exc,
            )
            return {"status": "deferred", "error": str(exc)}

    def pause(self):
        logger.info("Model worker proxy paused")

    def resume(self):
        logger.info("Model worker proxy resumed")

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

    def add_anchor(
        self,
        category: str,
        title: str,
        ocr_text: str = "",
        old_category=None,
        process_name: str = "",
    ):
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
        result = self.request(
            "remove_anchor", {"category": category, "title": title}, timeout=30
        )
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
        raise RuntimeError(
            result.get("error", "Model worker remove_local_anchors_by_process failed")
        )

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
