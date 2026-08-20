import numpy as np
import pytest

import classifier
import storage_client


class _RustStorage:
    def __init__(self, response=None, error=None):
        self.response = response
        self.error = error
        self.requests = []

    def embed_bge_texts(self, texts):
        self.requests.append(list(texts))
        if self.error:
            raise RuntimeError(self.error)
        return self.response


@pytest.fixture
def fresh_embedder():
    classifier.TextEmbedder._instance = None
    classifier.TextEmbedder._initialized = False
    yield classifier.TextEmbedder()
    classifier.TextEmbedder._instance = None
    classifier.TextEmbedder._initialized = False


def test_text_embedder_uses_the_rust_bridge(fresh_embedder, monkeypatch):
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
    assert fresh_embedder._initialized is True
    assert not hasattr(fresh_embedder, "_model")


def test_text_embedder_propagates_rust_failures_without_python_fallback(
    fresh_embedder, monkeypatch
):
    storage = _RustStorage(error="worker_stopped: test")
    monkeypatch.setattr(storage_client, "get_storage_client", lambda: storage)

    with pytest.raises(RuntimeError, match="worker_stopped"):
        fresh_embedder.encode(["text"])

    assert not hasattr(fresh_embedder, "_encode_python")
    assert not hasattr(fresh_embedder, "_selected_runtime")


@pytest.mark.parametrize(
    "response, message",
    [
        ({"dimensions": 2, "vectors": [[1.0]]}, "expected 2"),
        ({"dimensions": 3, "vectors": [[1.0, 0.0]]}, "expected 3"),
        ({"dimensions": 2, "vectors": [[float("nan"), 0.0]]}, "non-finite"),
    ],
)
def test_text_embedder_validates_rust_contract(
    fresh_embedder, monkeypatch, response, message
):
    storage = _RustStorage(response)
    monkeypatch.setattr(storage_client, "get_storage_client", lambda: storage)

    with pytest.raises(RuntimeError, match=message):
        fresh_embedder.encode(["text"])


def test_text_embedder_requires_a_storage_client(fresh_embedder, monkeypatch):
    monkeypatch.setattr(storage_client, "get_storage_client", lambda: None)

    with pytest.raises(RuntimeError, match="Rust storage client is unavailable"):
        fresh_embedder.encode(["text"])
