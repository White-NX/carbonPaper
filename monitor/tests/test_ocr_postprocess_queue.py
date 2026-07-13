import monitor.config as config
from monitor.worker_process import OcrPostprocessQueue, _process_ocr
from ocr_service import OCRService, _parse_ocr_idle_unload_secs


class DummyOcrWorker:
    enable_vector_store = False
    vector_store = None


class DummyVectorWorker:
    enable_vector_store = True

    def __init__(self, vector_store):
        self.vector_store = vector_store


class FailingVectorStore:
    def add_image(self, **_kwargs):
        return {"status": "error", "stage": "add", "error": "chroma down"}


class SuccessfulVectorStore:
    def __init__(self):
        self.calls = []

    def add_image(self, **kwargs):
        self.calls.append(kwargs)
        return {"status": "success", "id": "doc-1", "skipped": False}


class DummyClassifier:
    def __init__(self):
        self.calls = []

    def classify(self, title, ocr_text, process_name=""):
        self.calls.append((title, ocr_text, process_name))
        return "Development", 0.87654


class DummyStorageClient:
    def __init__(self):
        self.updates = []

    def get_temp_image_bytes(self, screenshot_id):
        from PIL import Image
        import io

        image = Image.new("RGB", (2, 2), color="white")
        buf = io.BytesIO()
        image.save(buf, format="PNG")
        return {
            "status": "success",
            "data": {"image_bytes": buf.getvalue(), "screenshot_id": screenshot_id},
        }

    def update_screenshot_category(self, screenshot_id, category, category_confidence=None):
        self.updates.append((screenshot_id, category, category_confidence))
        return True


class DummyOcrEngine:
    def recognize(self, _image):
        return []


class DummyProcessOcrWorker:
    enable_vector_store = False
    vector_store = None

    def __init__(self):
        self.ocr_engine = DummyOcrEngine()
        self.stats = {"processed_count": 0, "total_texts_found": 0}


class CapturingPostprocessQueue:
    def __init__(self):
        self.jobs = []

    def enqueue(self, job):
        self.jobs.append(job)
        return True


def _png_bytes():
    from PIL import Image
    import io

    image = Image.new("RGB", (2, 2), color="white")
    buf = io.BytesIO()
    image.save(buf, format="PNG")
    return buf.getvalue()


def test_ocr_postprocess_queue_drops_when_full():
    queue = OcrPostprocessQueue(DummyOcrWorker(), None, maxsize=1)

    assert queue.enqueue({"screenshot_id": 1})
    assert not queue.enqueue({"screenshot_id": 2})
    assert queue.dropped == 1


def test_ocr_postprocess_updates_category_async_path(monkeypatch):
    classifier = DummyClassifier()
    storage = DummyStorageClient()
    queue = OcrPostprocessQueue(DummyOcrWorker(), classifier, maxsize=1)

    monkeypatch.setattr(config, "CLASSIFICATION_ENABLED", True)
    monkeypatch.setattr("storage_client.get_storage_client", lambda: storage)

    queue._handle_job({
        "screenshot_id": 42,
        "window_title": "Editor",
        "process_name": "code.exe",
        "ocr_text": "async classification text",
        "image_bytes": b"",
    })

    assert classifier.calls == [("Editor", "async classification text", "code.exe")]
    assert storage.updates == [(42, "Development", 0.8765)]


def test_process_ocr_enqueues_postprocess_even_when_ocr_text_is_empty(monkeypatch):
    storage = DummyStorageClient()
    postprocess = CapturingPostprocessQueue()

    monkeypatch.setattr("storage_client.get_storage_client", lambda: storage)

    result = _process_ocr(
        {
            "screenshot_id": 42,
            "image_hash": "hash-42",
            "window_title": "Editor",
            "process_name": "code.exe",
            "timestamp": 123,
        },
        DummyProcessOcrWorker(),
        postprocess,
    )

    assert result["status"] == "success"
    assert result["ocr_text"] == ""
    assert result["postprocess_enqueued"] is True
    assert len(postprocess.jobs) == 1
    assert postprocess.jobs[0]["ocr_text"] == ""
    assert postprocess.jobs[0]["window_title"] == "Editor"


def test_ocr_service_loads_python_engine_only_on_first_inference(monkeypatch):
    created = []
    engine = DummyOcrEngine()
    monkeypatch.setattr("ocr_service.get_ocr_engine", lambda: created.append(True) or engine)
    service = OCRService(enable_vector_store=False)

    assert service.ocr_engine is None
    assert service.get_ocr_engine_for_inference() is engine
    assert service.get_ocr_engine_for_inference() is engine
    assert created == [True]


def test_ocr_service_unloads_idle_engine_only_in_rust_provider_mode(monkeypatch):
    engine = DummyOcrEngine()
    monkeypatch.setattr("ocr_service.get_ocr_engine", lambda: engine)
    service = OCRService(enable_vector_store=False)
    service.get_ocr_engine_for_inference()
    service._ocr_idle_unload_secs = 30.0
    service._ocr_last_used_monotonic = 100.0

    service.set_rust_ocr_provider_active(False)
    assert service.maybe_unload_idle_ocr_engine(now=200.0) is False
    assert service.ocr_engine is engine

    service.set_rust_ocr_provider_active(True)
    assert service.maybe_unload_idle_ocr_engine(now=200.0) is True
    assert service.ocr_engine is None


def test_invalid_ocr_idle_unload_value_falls_back_to_default():
    assert _parse_ocr_idle_unload_secs("5m") == 300.0
    assert _parse_ocr_idle_unload_secs("nan") == 300.0
    assert _parse_ocr_idle_unload_secs("10") == 30.0


def test_vector_indexing_failure_records_retry_backlog():
    queue = OcrPostprocessQueue(DummyVectorWorker(FailingVectorStore()), None, maxsize=4)

    ok = queue._handle_vector_indexing({
        "screenshot_id": 42,
        "image_hash": "hash-42",
        "window_title": "Editor",
        "process_name": "code.exe",
        "timestamp": 123,
        "ocr_text": "indexed text",
        "image_bytes": _png_bytes(),
    })

    snapshot = queue.status_snapshot()
    assert ok is False
    assert snapshot["vector_failed"] == 1
    assert snapshot["vector_retry_backlog_count"] == 1
    assert snapshot["last_indexing_error"] == "chroma down"
    assert snapshot["last_indexing_error_at"] is not None


def test_vector_retry_enqueues_retry_only_job_without_classification(monkeypatch):
    vector_store = SuccessfulVectorStore()
    classifier = DummyClassifier()
    storage = DummyStorageClient()
    queue = OcrPostprocessQueue(DummyVectorWorker(vector_store), classifier, maxsize=4)
    retry_job = {
        "screenshot_id": 42,
        "image_hash": "hash-42",
        "window_title": "Editor",
        "process_name": "code.exe",
        "timestamp": 123,
        "ocr_text": "indexed text",
        "image_bytes": _png_bytes(),
    }
    queue._record_vector_failure(retry_job, "previous failure")

    result = queue.retry_failed_vector_indexing(limit=1)
    queued = queue._queue.get_nowait()
    queue._queue.task_done()

    monkeypatch.setattr(config, "CLASSIFICATION_ENABLED", True)
    monkeypatch.setattr("storage_client.get_storage_client", lambda: storage)
    queue._handle_job(queued)

    assert result["status"] == "success"
    assert result["enqueued"] == 1
    assert queued["_vector_retry_only"] is True
    assert len(vector_store.calls) == 1
    assert classifier.calls == []
    assert storage.updates == []
    assert queue.vector_retry_backlog_count() == 0
