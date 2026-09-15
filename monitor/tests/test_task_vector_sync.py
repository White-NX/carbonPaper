"""The Python consumer accepts Rust vectors but never creates them itself."""
from pathlib import Path

import numpy as np
import pytest

from monitor.clustering_commands import handle_clustering_command
from task_clustering import HotColdManager, EMBEDDING_DIM


class Collection:
    id = "collection-one"

    def __init__(self):
        self.rows = {"1": [1.0] + [0.0] * (EMBEDDING_DIM - 1)}
        self.metadatas = {}

    def upsert(self, ids, embeddings, metadatas, **_kwargs):
        self.rows.update(zip(ids, embeddings))
        self.metadatas.update(zip(ids, metadatas))

    def get(self, ids, include):
        assert include == ["metadatas"]
        present = [sid for sid in ids if sid in self.rows]
        return {"ids": present, "metadatas": [self.metadatas.get(sid, {}) for sid in present]}


class Client:
    def __init__(self):
        self.collection = Collection()

    def get_or_create_collection(self, **kwargs):
        assert kwargs["embedding_function"] is None
        return self.collection


class Storage:
    def __init__(self, authorized):
        self.authorized = authorized

    def is_background_authorized(self):
        return self.authorized


def record(sid):
    return {"id": str(sid), "embedding": [1.0] + [0.0] * (EMBEDDING_DIM - 1)}


def test_partial_and_replayed_pages_are_idempotent():
    client = Client()
    manager = HotColdManager(client)
    assert manager.upsert_task_vectors([record(1), record(2)], target="collection-one") == 2
    assert set(client.collection.rows) == {"1", "2"}
    assert manager.upsert_task_vectors([record(1), record(2)], target="collection-one") == 2
    assert len(client.collection.rows) == 2


def test_recreated_collection_rejects_old_cursor_before_writing():
    client = Client()
    manager = HotColdManager(client)
    old_target = manager.task_vector_sync_target()
    client.collection = Collection()
    client.collection.id = "replacement"
    assert manager.task_vector_sync_target() == "replacement"
    with pytest.raises(RuntimeError, match="collection changed"):
        manager.upsert_task_vectors([record(2)], target=old_target)
    assert set(client.collection.rows) == {"1"}


def test_late_page_acknowledges_but_never_overwrites_a_newer_source():
    client = Client()
    manager = HotColdManager(client)
    newer = {**record(2), "source_revision": 4}
    older = {**record(2), "source_revision": 3,
             "embedding": [0.0, 1.0] + [0.0] * (EMBEDDING_DIM - 2)}
    assert manager.upsert_task_vectors([newer], target="collection-one") == 1
    assert manager.upsert_task_vectors([older], target="collection-one") == 1
    assert client.collection.rows["2"] == newer["embedding"]
    assert client.collection.metadatas["2"]["source_revision"] == 4
    assert manager.upsert_task_vectors([{**older, "source_revision": 5}], target="collection-one") == 1
    assert client.collection.rows["2"] == older["embedding"]


@pytest.mark.parametrize("command", ["get_task_vector_sync_target", "upsert_task_vectors"])
def test_background_sync_requires_archive_read_authorization(command):
    manager = HotColdManager(Client(), Storage(False))
    result = handle_clustering_command(
        {"command": command, "background": True, "records": [record(2)]},
        None, manager, lambda **_kwargs: True,
    )
    assert result == {"error": "AUTH_REQUIRED"}
    manager._storage_client.authorized = True
    result = handle_clustering_command(
        {"command": command, "background": True, "records": [record(2)]},
        None, manager, lambda **_kwargs: False,
    )
    assert result["status"] == "success"


@pytest.mark.parametrize("background", [False, True])
def test_empty_input_never_loads_an_encoder(monkeypatch, background):
    manager = HotColdManager(None)
    monkeypatch.setattr(manager, "estimate_clustering_inputs", lambda *_: {"count": 0})
    monkeypatch.setattr(manager, "get_hot_vectors", lambda: (np.empty((0, EMBEDDING_DIM)), [], []))
    result = manager.run_clustering(background=background)
    assert result["status"] == "empty"
    source = (Path(__file__).parents[1] / "task_clustering.py").read_text(encoding="utf-8")
    for retired in ("TaskEmbedder", "onnxruntime", "create_onnx_session", "_backfill_from_screenshots"):
        assert retired not in source
