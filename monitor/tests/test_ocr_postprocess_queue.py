import threading
import pytest

import monitor.config as config
from monitor.worker_process import PostprocessQueue, WORKER_PROTOCOL_VERSION, _enqueue_ocr_postprocess


class DummyClassifier:
    def __init__(self, result=("Development", 0.87654)):
        self.result = result
        self.calls = []

    def classify(self, title, ocr_text, process_name=""):
        self.calls.append((title, ocr_text, process_name))
        return self.result


class YieldingClassifier:
    def __init__(self, error):
        self.error = error

    def classify(self, **_kwargs):
        raise RuntimeError(self.error)


class DummyStorageClient:
    def __init__(self):
        self.updates = []
        self.postprocess_statuses = []
        self.postprocess_retries = []
        self.pending = threading.Event()

    def update_screenshot_category(self, screenshot_id, category, category_confidence=None):
        self.updates.append((screenshot_id, category, category_confidence))
        return True

    def set_ocr_postprocess_status(self, screenshot_id, status, error=None):
        self.postprocess_statuses.append((screenshot_id, status, error))
        if status == "pending":
            self.pending.set()
        return True

    def record_ocr_postprocess_retry(self, screenshot_id, error):
        self.postprocess_retries.append((screenshot_id, error))
        return True


def test_postprocess_queue_drops_when_full():
    queue = PostprocessQueue(None, maxsize=1)

    assert queue.enqueue({"screenshot_id": 1})
    assert not queue.enqueue({"screenshot_id": 2})
    assert queue.status_snapshot()["dropped"] == 1


def test_postprocess_classifies_and_updates_category(monkeypatch):
    classifier = DummyClassifier()
    storage = DummyStorageClient()
    queue = PostprocessQueue(classifier, maxsize=1)

    monkeypatch.setattr(config, "CLASSIFICATION_ENABLED", True)
    monkeypatch.setattr("storage_client.get_storage_client", lambda: storage)

    queue._handle_job({
        "screenshot_id": 42,
        "window_title": "Editor",
        "process_name": "code.exe",
        "ocr_text": "classification text",
    })

    assert classifier.calls == [("Editor", "classification text", "code.exe")]
    assert storage.updates == [(42, "Development", 0.8765)]


def test_scheduling_yield_persists_pending_state(monkeypatch):
    reason = "foreground_busy: query active"
    storage = DummyStorageClient()
    queue = PostprocessQueue(YieldingClassifier(reason), maxsize=1)
    monkeypatch.setattr(config, "CLASSIFICATION_ENABLED", True)
    monkeypatch.setattr("storage_client.get_storage_client", lambda: storage)

    queue.start()
    try:
        assert queue.enqueue({
            "screenshot_id": 43,
            "window_title": "Editor",
            "process_name": "code.exe",
            "ocr_text": "deferred text",
            "_persistent_postprocess": True,
        })
        assert storage.pending.wait(timeout=2.0)
    finally:
        queue.stop()

    assert storage.postprocess_statuses == [
        (43, "processing", None),
        (43, "pending", reason),
    ]
    assert storage.postprocess_retries == []
    assert queue.status_snapshot()["processed"] == 0
    assert queue.status_snapshot()["deferred"] == 1


def test_enqueue_contract_contains_only_classification_payload():
    queue = PostprocessQueue(None, maxsize=4)
    result = _enqueue_ocr_postprocess({
        "screenshot_id": 42,
        "image_hash": "hash-42",
        "window_title": "Editor",
        "process_name": "code.exe",
        "timestamp": 123,
        "ocr_text": "classification text",
        "image_bytes": b"must not cross the boundary",
    }, queue)
    queued = queue._queue.get_nowait()
    queue._queue.task_done()

    assert result == {
        "status": "success",
        "postprocess_enqueued": True,
        "worker_protocol": WORKER_PROTOCOL_VERSION,
    }
    assert queued == {
        "screenshot_id": 42,
        "window_title": "Editor",
        "process_name": "code.exe",
        "timestamp": 123,
        "ocr_text": "classification text",
        "_persistent_postprocess": True,
    }


def test_enqueue_requires_screenshot_id_or_queue():
    assert _enqueue_ocr_postprocess({}, None) == {"error": "screenshot_id is required"}
    assert _enqueue_ocr_postprocess({"screenshot_id": 1}, None) == {
        "error": "Classification postprocess service is unavailable"
    }


class StagedStorageClient(DummyStorageClient):
    def __init__(self):
        super().__init__()
        self.completions = []
        self.deferrals = []

    def complete_staged_postprocess(self, receipt, category=None, confidence=None):
        self.completions.append((receipt, category, confidence))

    def defer_staged_postprocess(self, receipt, failed=False):
        self.deferrals.append((receipt, failed))
        self.pending.set()


def staged_receipt():
    return {
        "task_id": "a" * 64, "dataset_id": "b" * 64, "screenshot_id": 42,
        "consumer": "classification", "lease_id": "c" * 64,
        "db_generation": 1, "source_revision": 2,
    }


def test_staged_enqueue_preserves_scope_without_forwarding_image_or_key():
    queue = PostprocessQueue(None, maxsize=1)
    receipt = staged_receipt()
    assert _enqueue_ocr_postprocess({
        "screenshot_id": 42, "ocr_text": "newly captured text", "staged_receipt": receipt,
        "task_key": b"must stay in Rust", "image_bytes": b"must not cross this pipe",
    }, queue)["postprocess_enqueued"]
    job = queue._queue.get_nowait()
    queue._queue.task_done()
    assert job["_staged_receipt"] == receipt
    assert "task_key" not in job
    assert "image_bytes" not in job


def test_staged_result_uses_receipt_instead_of_legacy_category_write(monkeypatch):
    storage = StagedStorageClient()
    queue = PostprocessQueue(DummyClassifier())
    receipt = staged_receipt()
    monkeypatch.setattr(config, "CLASSIFICATION_ENABLED", True)
    monkeypatch.setattr("storage_client.get_storage_client", lambda: storage)
    queue._handle_job({"screenshot_id": 999, "ocr_text": "text", "_staged_receipt": receipt})
    assert storage.completions == [(receipt, "Development", 0.8765)]
    assert storage.updates == []
    assert storage.postprocess_statuses == []


def test_staged_scheduling_yield_releases_only_its_lease(monkeypatch):
    storage = StagedStorageClient()
    queue = PostprocessQueue(YieldingClassifier("foreground_busy: query active"))
    receipt = staged_receipt()
    monkeypatch.setattr(config, "CLASSIFICATION_ENABLED", True)
    monkeypatch.setattr("storage_client.get_storage_client", lambda: storage)
    queue.start()
    try:
        assert queue.enqueue({"screenshot_id": 42, "_staged_receipt": receipt})
        assert storage.pending.wait(timeout=2.0)
    finally:
        queue.stop()
    assert storage.deferrals == [(receipt, False)]
    assert storage.completions == []
    assert storage.postprocess_statuses == []
    assert storage.postprocess_retries == []


def test_staged_disabled_classification_finishes_without_changing_category(monkeypatch):
    storage = StagedStorageClient()
    receipt = staged_receipt()
    monkeypatch.setattr(config, "CLASSIFICATION_ENABLED", False)
    monkeypatch.setattr("storage_client.get_storage_client", lambda: storage)
    PostprocessQueue(None)._handle_job({"_staged_receipt": receipt})
    assert storage.completions == [(receipt, None, None)]
    assert storage.updates == []


def test_missing_classifier_does_not_claim_the_staged_work_completed(monkeypatch):
    storage = StagedStorageClient()
    monkeypatch.setattr(config, "CLASSIFICATION_ENABLED", True)
    monkeypatch.setattr("storage_client.get_storage_client", lambda: storage)
    with pytest.raises(RuntimeError, match="unavailable"):
        PostprocessQueue(None)._handle_job({"_staged_receipt": staged_receipt()})
    assert storage.completions == []
