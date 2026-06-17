import monitor.config as config
from monitor.worker_process import OcrPostprocessQueue, _process_ocr


class DummyOcrWorker:
    enable_vector_store = False
    vector_store = None


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
