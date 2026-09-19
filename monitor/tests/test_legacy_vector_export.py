import base64
import time

import numpy as np
import pytest

from legacy_vector_export import CLIP_EMBEDDING_DIM, MINILM_EMBEDDING_DIM, LegacyVectorExporter


class MissingLegacyCollectionClient:
    def __init__(self):
        self.get_calls = []

    def get_collection(self, name):
        self.get_calls.append(name)
        raise RuntimeError("collection does not exist")

    def list_collections(self):
        return []

    def get_or_create_collection(self, **_kwargs):
        raise AssertionError("read-only export must not create a collection")

    def create_collection(self, **_kwargs):
        raise AssertionError("read-only export must not create a collection")


def _wait_until_ready(exporter, export_id, timeout=2.0, expected_state="ready"):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        status = exporter.status(export_id)
        if status["state"] == expected_state:
            return status
        if status["state"] in {"failed", "timed_out", "missing"}:
            raise AssertionError(status)
        time.sleep(0.01)
    raise AssertionError("legacy vector export did not become ready")


@pytest.mark.parametrize("kind, collection, dimensions", [
    ("clip", "screenshots", CLIP_EMBEDDING_DIM),
    ("minilm", "task_vectors", MINILM_EMBEDDING_DIM),
])
def test_missing_legacy_collection_exports_empty_without_creating_one(kind, collection, dimensions):
    client = MissingLegacyCollectionClient()
    exporter = LegacyVectorExporter(client, kind=kind)
    export_id = f"{kind}-empty-export-0001"

    assert exporter.start(export_id)["state"] == "preparing"
    assert _wait_until_ready(exporter, export_id)["total"] == 0
    assert exporter.page(export_id) == {
        "ids": [],
        "dimensions": dimensions,
        "embeddings_f32_le_b64": "",
        "missing_ids": [],
        "errors": [],
        "next_cursor": 0,
        "done": True,
        "total": 0,
    }
    assert client.get_calls == [collection]
    assert exporter.finish(export_id) is True


@pytest.mark.parametrize("kind", ["clip", "minilm"])
def test_legacy_exporter_has_no_inference_or_write_surface(kind):
    exporter = LegacyVectorExporter(None, kind=kind)

    for retired_operation in ("encode", "query", "upsert", "delete"):
        assert not hasattr(exporter, retired_operation)


@pytest.mark.parametrize("kind, collection_name", [("clip", "screenshots"), ("minilm", "task_vectors")])
def test_unreadable_legacy_collection_does_not_publish_an_empty_migration(kind, collection_name):
    class UnreadableClient:
        def get_collection(self, name):
            assert name == collection_name
            raise OSError("source temporarily unreadable")

        def list_collections(self):
            return [collection_name]

    exporter = LegacyVectorExporter(UnreadableClient(), kind=kind)
    export_id = f"{kind}-unreadable-export-0001"
    exporter.start(export_id)
    failed = _wait_until_ready(exporter, export_id, expected_state="failed")
    assert "source temporarily unreadable" in failed["error"]
    with pytest.raises(ValueError, match="failed"):
        exporter.page(export_id)
    exporter.finish(export_id)


def test_minilm_export_keeps_numeric_snapshot_order_and_reports_missing_rows():
    vector = [0.25] * MINILM_EMBEDDING_DIM

    class Collection:
        def __init__(self):
            self.rows = {key: vector for key in ("10", "2", "7")}
            self.includes = []

        def get(self, ids=None, include=None):
            self.includes.append(include)
            selected = [key for key in (self.rows if ids is None else ids) if key in self.rows]
            return {"ids": selected, "embeddings": [self.rows[key] for key in selected]}

    collection = Collection()

    class Client:
        def get_collection(self, name):
            assert name == "task_vectors"
            return collection

    exporter = LegacyVectorExporter(Client(), kind="minilm")
    export_id = "minilm-snapshot-export-0001"
    exporter.start(export_id)
    assert _wait_until_ready(exporter, export_id)["total"] == 3
    assert collection.includes == [[]]

    # Later inserts stay outside the snapshot; deleted rows retain their cursor slot.
    collection.rows["3"] = vector
    del collection.rows["7"]
    first = exporter.page(export_id, cursor=0, limit=2)
    assert first == {
        "ids": ["2"],
        "dimensions": MINILM_EMBEDDING_DIM,
        "embeddings_f32_le_b64": base64.b64encode(np.asarray(vector, dtype="<f4").tobytes()).decode("ascii"),
        "missing_ids": ["7"],
        "errors": [],
        "next_cursor": 2,
        "done": False,
        "total": 3,
    }
    second = exporter.page(export_id, cursor=first["next_cursor"], limit=2)
    assert second["ids"] == ["10"]
    assert second["next_cursor"] == 3
    assert second["done"] is True
    assert collection.includes == [[], ["embeddings"], ["embeddings"]]
    assert exporter.finish(export_id) is True
    with pytest.raises(ValueError, match="unknown or expired"):
        exporter.page(export_id)
