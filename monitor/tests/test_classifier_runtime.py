import numpy as np
import pytest

import classifier
import storage_client


class _RustStorage:
    def __init__(self, response=None, error=None):
        self.response = response
        self.error = error
        self.requests = []
        self.fallbacks = []
        self.python_inferences = 0

    def embed_bge_texts(self, texts):
        self.requests.append(list(texts))
        if self.error:
            raise RuntimeError(self.error)
        return self.response

    def record_classification_python_fallback(self, error):
        self.fallbacks.append(error)

    def record_classification_python_inference(self):
        self.python_inferences += 1


@pytest.fixture
def fresh_embedder(monkeypatch):
    monkeypatch.setenv("CARBONPAPER_CLASSIFICATION_RUNTIME", "rust")
    classifier.TextEmbedder._instance = None
    classifier.TextEmbedder._model = None
    classifier.TextEmbedder._tokenizer = None
    classifier.TextEmbedder._is_onnx = False
    classifier.TextEmbedder._initialized = False
    classifier.TextEmbedder._selected_runtime = None
    classifier.TextEmbedder._last_backend = None
    yield classifier.TextEmbedder()
    classifier.TextEmbedder._instance = None


def test_rust_is_the_default_classification_embedder(fresh_embedder, monkeypatch):
    storage = _RustStorage({
        "dimensions": 2,
        "vectors": [[0.0, 1.0], [1.0, 0.0]],
    })
    monkeypatch.setattr(storage_client, "get_storage_client", lambda: storage)

    result = fresh_embedder.encode(["alpha", "beta"])

    np.testing.assert_array_equal(
        result,
        np.asarray([[0.0, 1.0], [1.0, 0.0]], dtype=np.float32),
    )
    assert storage.requests == [["alpha", "beta"]]
    assert fresh_embedder._last_backend == "rust"
    assert fresh_embedder._model is None


def test_rust_failure_falls_back_to_python_and_records_it(fresh_embedder, monkeypatch):
    storage = _RustStorage(error="worker_stopped: test")
    monkeypatch.setattr(storage_client, "get_storage_client", lambda: storage)
    expected = np.asarray([[0.25, 0.75]], dtype=np.float32)
    monkeypatch.setattr(
        fresh_embedder,
        "_initialize_python_model",
        lambda: setattr(fresh_embedder, "_model", object()),
    )
    monkeypatch.setattr(fresh_embedder, "_encode_python", lambda texts: expected)

    result = fresh_embedder.encode(["fallback"])

    assert result is expected
    assert fresh_embedder._last_backend == "python"
    assert storage.fallbacks == ["worker_stopped: test"]


def test_foreground_priority_does_not_trigger_python_competition(fresh_embedder, monkeypatch):
    storage = _RustStorage(error="foreground_busy: query active")
    monkeypatch.setattr(storage_client, "get_storage_client", lambda: storage)
    called = []
    monkeypatch.setattr(fresh_embedder, "_encode_python", lambda texts: called.append(texts))

    with pytest.raises(RuntimeError, match="foreground_busy"):
        fresh_embedder.encode(["wait"])

    assert called == []


def test_background_batch_contention_does_not_trigger_python_competition(
    fresh_embedder, monkeypatch
):
    storage = _RustStorage(error="background_busy: CLIP batch active")
    monkeypatch.setattr(storage_client, "get_storage_client", lambda: storage)
    called = []
    monkeypatch.setattr(fresh_embedder, "_encode_python", lambda texts: called.append(texts))

    with pytest.raises(RuntimeError, match="background_busy"):
        fresh_embedder.encode(["wait"])

    assert called == []


def test_explicit_python_runtime_reports_the_serving_backend(fresh_embedder, monkeypatch):
    storage = _RustStorage()
    monkeypatch.setattr(storage_client, "get_storage_client", lambda: storage)
    fresh_embedder._selected_runtime = "python"
    fresh_embedder._initialized = True
    expected = np.asarray([[0.5, 0.5]], dtype=np.float32)
    monkeypatch.setattr(fresh_embedder, "_encode_python", lambda texts: expected)

    result = fresh_embedder.encode(["python"])

    assert result is expected
    assert fresh_embedder._last_backend == "python"
    assert storage.python_inferences == 1
