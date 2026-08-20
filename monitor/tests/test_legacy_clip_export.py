import time

from legacy_clip_export import CLIP_EMBEDDING_DIM, LegacyClipVectorExporter


class MissingLegacyCollectionClient:
    def __init__(self):
        self.get_calls = []

    def get_collection(self, name):
        self.get_calls.append(name)
        raise RuntimeError("collection does not exist")

    def get_or_create_collection(self, **_kwargs):
        raise AssertionError("read-only export must not create a collection")

    def create_collection(self, **_kwargs):
        raise AssertionError("read-only export must not create a collection")


def _wait_until_ready(exporter, export_id, timeout=2.0):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        status = exporter.status(export_id)
        if status["state"] == "ready":
            return status
        if status["state"] in {"failed", "timed_out", "missing"}:
            raise AssertionError(status)
        time.sleep(0.01)
    raise AssertionError("legacy CLIP export did not become ready")


def test_missing_legacy_collection_exports_empty_without_creating_one():
    client = MissingLegacyCollectionClient()
    exporter = LegacyClipVectorExporter(client)
    export_id = "clip-empty-export-0001"

    assert exporter.start(export_id)["state"] == "preparing"
    assert _wait_until_ready(exporter, export_id)["total"] == 0
    assert exporter.page(export_id) == {
        "ids": [],
        "dimensions": CLIP_EMBEDDING_DIM,
        "embeddings_f32_le_b64": "",
        "missing_ids": [],
        "errors": [],
        "next_cursor": 0,
        "done": True,
        "total": 0,
    }
    assert client.get_calls == ["screenshots"]
    assert exporter.finish(export_id) is True


def test_legacy_clip_exporter_has_no_inference_or_write_surface():
    exporter = LegacyClipVectorExporter(None)

    for retired_operation in ("encode", "query", "upsert", "delete"):
        assert not hasattr(exporter, retired_operation)
