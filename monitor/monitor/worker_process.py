"""Restartable worker process for OCR, embedding, and classification.

The parent monitor process keeps the named-pipe server responsive. Expensive
model calls run here so a stuck OCR/embedding/classifier call can be terminated
by killing this process and starting a fresh one for the next request.
"""

from __future__ import annotations

import datetime
import io
import logging
import multiprocessing
import os
import time
import traceback
from typing import Any, Dict, Optional


logger = logging.getLogger(__name__)


def _json_safe(obj):
    if isinstance(obj, (datetime.datetime, datetime.date)):
        return obj.isoformat()
    return obj


def _process_ocr(req: Dict[str, Any], ocr_worker, classifier) -> Dict[str, Any]:
    from PIL import Image
    from storage_client import get_storage_client
    from . import config

    screenshot_id = req.get("screenshot_id")
    if screenshot_id is None:
        return {"error": "screenshot_id is required"}

    cmd_started = time.perf_counter()
    sc = get_storage_client()
    if not sc:
        return {"error": "Storage client not available"}

    resp = sc.get_temp_image_bytes(screenshot_id)
    if resp.get("status") != "success":
        return {"error": f"Failed to fetch image: {resp.get('error', 'unknown')}"}

    image_bytes = resp.get("data", {}).get("image_bytes")
    if not image_bytes:
        return {"error": "No image data returned from storage"}

    image_pil = Image.open(io.BytesIO(image_bytes))
    image_pil.load()

    ocr_results = ocr_worker.ocr_engine.recognize(image_pil)
    filtered = [r for r in ocr_results if r.get("confidence", 0) >= 0.5]
    ocr_worker.stats["processed_count"] += 1
    ocr_worker.stats["total_texts_found"] += len(filtered)

    image_hash = req.get("image_hash", "")
    ocr_text = " ".join([r.get("text", "") for r in filtered])
    if ocr_worker.enable_vector_store and ocr_worker.vector_store and ocr_text.strip():
        try:
            ocr_worker.vector_store.add_image(
                image_path=f"memory://{image_hash}",
                image=image_pil,
                metadata={
                    "window_title": req.get("window_title", ""),
                    "process_name": req.get("process_name", ""),
                    "timestamp": req.get("timestamp", 0),
                },
                ocr_text=ocr_text,
            )
        except Exception as exc:
            logger.warning("Vector store add failed: %s", exc)

    category = None
    category_confidence = None
    if classifier and config.CLASSIFICATION_ENABLED:
        try:
            category, category_confidence = classifier.classify(
                title=req.get("window_title", ""),
                ocr_text=ocr_text,
                process_name=req.get("process_name", ""),
            )
            category_confidence = round(category_confidence, 4)
        except Exception as exc:
            logger.warning("Classification failed: %s", exc)

    result = {
        "status": "success",
        "ocr_results": filtered,
        "ocr_text": ocr_text,
        "elapsed": time.perf_counter() - cmd_started,
    }
    if category:
        result["category"] = category
        result["category_confidence"] = category_confidence
    return result


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

        conn.send({"status": "ready"})
    except Exception as exc:
        conn.send({"status": "error", "error": str(exc), "traceback": traceback.format_exc()})
        return

    while True:
        try:
            msg = conn.recv()
        except EOFError:
            return
        command = msg.get("command")
        try:
            if command == "stop":
                conn.send({"status": "success"})
                return
            if command == "process_ocr":
                conn.send(_process_ocr(msg.get("request", {}), ocr_worker, classifier))
            elif command == "get_stats":
                conn.send({"status": "success", "stats": ocr_worker.get_stats()})
            elif command == "search_by_natural_language":
                args = msg.get("args", {})
                conn.send({
                    "status": "success",
                    "results": ocr_worker.search_by_natural_language(**args),
                })
            elif command == "delete_vector_image":
                image_hash = msg.get("image_hash", "")
                ok = False
                if ocr_worker.vector_store:
                    ok = bool(ocr_worker.vector_store.delete_image(f"memory://{image_hash}"))
                conn.send({"status": "success", "ok": ok})
            elif command == "classify":
                if not classifier:
                    conn.send({"error": "Classification service not initialised"})
                else:
                    args = msg.get("args", {})
                    category, confidence = classifier.classify(**args)
                    conn.send({"status": "success", "category": category, "confidence": confidence})
            elif command == "classify_debug":
                if not classifier:
                    conn.send({"error": "Classification service not initialised"})
                else:
                    conn.send({"status": "success", "data": classifier.classify_debug(**msg.get("args", {}))})
            elif command == "add_anchor":
                if not classifier:
                    conn.send({"error": "Classification service not initialised"})
                else:
                    conn.send({"status": "success", "data": classifier.add_anchor(**msg.get("args", {}))})
            elif command == "remove_anchor":
                if not classifier:
                    conn.send({"error": "Classification service not initialised"})
                else:
                    removed = classifier.remove_anchor(msg.get("category", ""), msg.get("title", ""))
                    conn.send({"status": "success", "removed": removed})
            elif command == "remove_local_anchors_by_process":
                if not classifier:
                    conn.send({"error": "Classification service not initialised"})
                else:
                    removed_count = classifier.remove_local_anchors_by_process(
                        msg.get("category", ""),
                        msg.get("process_name", ""),
                    )
                    conn.send({"status": "success", "removed_count": removed_count})
            elif command == "get_categories":
                if not classifier:
                    conn.send({"error": "Classification service not initialised"})
                else:
                    conn.send({"status": "success", "categories": classifier.get_categories()})
            elif command == "get_anchors":
                if not classifier:
                    conn.send({"error": "Classification service not initialised"})
                else:
                    conn.send({"status": "success", "anchors": classifier.get_anchors()})
            else:
                conn.send({"error": f"Unknown worker command: {command}"})
        except Exception as exc:
            conn.send({"error": str(exc), "traceback": traceback.format_exc()})


class RestartableModelWorker:
    def __init__(self, storage_pipe: Optional[str], data_dir: str, env: Optional[Dict[str, str]] = None):
        self.storage_pipe = storage_pipe
        self.data_dir = data_dir
        self.env = env or {}
        self._ctx = multiprocessing.get_context("spawn")
        self._conn = None
        self._proc = None
        self._lock = multiprocessing.RLock()
        self._stats = {"processed_count": 0, "failed_count": 0, "total_texts_found": 0, "start_time": None}
        self.stats = self._stats
        self.enable_vector_store = True
        self.vector_store = None

    def start(self, timeout: float = 180.0):
        with self._lock:
            if self._proc is not None and self._proc.is_alive():
                return
            parent_conn, child_conn = self._ctx.Pipe()
            proc = self._ctx.Process(
                target=_worker_main,
                args=(child_conn, self.storage_pipe, self.data_dir, self.env),
                name="CarbonModelWorker",
                daemon=True,
            )
            proc.start()
            self._conn = parent_conn
            self._proc = proc
            if not parent_conn.poll(timeout):
                self.restart()
                raise TimeoutError(f"Model worker cold start timed out after {timeout}s")
            ready = parent_conn.recv()
            if ready.get("status") != "ready":
                self.restart()
                raise RuntimeError(ready.get("error", "Model worker failed to start"))

    def request(self, command: str, payload: Optional[Dict[str, Any]] = None, timeout: float = 120.0):
        with self._lock:
            self.start(timeout=max(30.0, min(180.0, timeout)))
            assert self._conn is not None
            self._conn.send({"command": command, **(payload or {})})
            if not self._conn.poll(timeout):
                self.restart()
                self._stats["failed_count"] += 1
                raise TimeoutError(f"Model worker command {command} timed out after {timeout}s")
            result = self._conn.recv()
            if command == "process_ocr" and result.get("status") == "success":
                self._stats["processed_count"] += 1
                self._stats["total_texts_found"] += len(result.get("ocr_results") or [])
            elif result.get("error"):
                self._stats["failed_count"] += 1
            return result

    def restart(self):
        with self._lock:
            if self._proc is not None and self._proc.is_alive():
                self._proc.terminate()
                self._proc.join(timeout=5)
                if self._proc.is_alive():
                    self._proc.kill()
            self._proc = None
            self._conn = None

    def stop(self):
        with self._lock:
            try:
                if self._conn and self._proc and self._proc.is_alive():
                    self._conn.send({"command": "stop"})
                    self._conn.poll(2)
            except Exception:
                pass
            self.restart()

    def get_stats(self):
        # Status polling must never start the model worker. Cold-starting OCR,
        # vector, or classifier models from a cheap health check blocks the
        # monitor pipe long enough for Rust-side status callers to time out.
        if self._proc is None or not self._proc.is_alive() or self._conn is None:
            return dict(self._stats)
        if not self._lock.acquire(block=False):
            return dict(self._stats)
        try:
            self._conn.send({"command": "get_stats"})
            if self._conn.poll(0.05):
                result = self._conn.recv()
                if result.get("status") == "success":
                    return result.get("stats", self._stats)
        except Exception:
            return dict(self._stats)
        finally:
            self._lock.release()
        return dict(self._stats)

    def pause(self):
        logger.info("Model worker proxy paused")

    def resume(self):
        logger.info("Model worker proxy resumed")

    def search_by_natural_language(self, **kwargs):
        result = self.request(
            "search_by_natural_language",
            {"args": kwargs},
            timeout=max(30.0, float(os.environ.get("CARBONPAPER_OCR_TIMEOUT_SECS", "120") or "120")),
        )
        if result.get("status") == "success":
            return result.get("results", [])
        raise RuntimeError(result.get("error", "Model worker search failed"))

    def delete_vector_image(self, image_hash: str) -> bool:
        result = self.request("delete_vector_image", {"image_hash": image_hash}, timeout=30)
        return bool(result.get("ok"))

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
