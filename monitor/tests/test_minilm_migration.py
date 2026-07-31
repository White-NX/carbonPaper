import base64

import numpy as np
import pytest
import time

import task_clustering as tc
import storage_client as tc_storage_client
from monitor.clustering_commands import handle_clustering_command


class FakeCollection:
    def __init__(self):
        self.rows = {}
        self.last_get = None
        self.get_calls = []

    def get(self, ids=None, include=None, where=None, offset=0, limit=None):
        self.last_get = {
            "ids": ids,
            "include": include,
            "where": where,
            "offset": offset,
            "limit": limit,
        }
        self.get_calls.append(self.last_get)
        if ids is not None:
            selected = [(doc_id, self.rows[doc_id]) for doc_id in ids if doc_id in self.rows]
        else:
            selected = list(self.rows.items())
            if where:
                if "$and" in where:
                    bounds = {
                        "$gte": where["$and"][0]["timestamp"]["$gte"],
                        "$lte": where["$and"][1]["timestamp"]["$lte"],
                    }
                else:
                    bounds = where["timestamp"]
                selected = [
                    item
                    for item in selected
                    if item[1]["metadata"]["timestamp"] >= bounds.get("$gte", float("-inf"))
                    and item[1]["metadata"]["timestamp"] <= bounds.get("$lte", float("inf"))
                ]
            selected = selected[offset: offset + limit if limit is not None else None]
        return {
            "ids": [item[0] for item in selected],
            "embeddings": [item[1]["embedding"] for item in selected],
        }

    def add(self, ids, embeddings, metadatas, documents=None):
        for index, doc_id in enumerate(ids):
            self.rows[doc_id] = {
                "embedding": embeddings[index],
                "metadata": metadatas[index],
                "document": documents[index] if documents else None,
            }

    def upsert(self, ids, embeddings, metadatas, documents=None):
        self.add(ids, embeddings, metadatas, documents)


class FakeClient:
    def __init__(self):
        self.collection = FakeCollection()

    def get_or_create_collection(self, **_kwargs):
        return self.collection


class FakeStorageClient:
    def __init__(self):
        self.pending = []

    def encrypt_for_chromadb(self, text):
        return text

    def smart_cluster_enqueue_pending(self, screenshot_id):
        self.pending.append(screenshot_id)
        return True


def _wait_for_export(manager, export_id, timeout=2.0):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        status = manager.get_task_vectors_export_status(export_id)
        if status["state"] == "ready":
            return status
        if status["state"] in {"failed", "timed_out", "missing"}:
            raise AssertionError(status)
        time.sleep(0.01)
    raise AssertionError("export did not become ready")


def test_export_snapshot_is_stable_ordered_and_vector_only():
    manager = tc.HotColdManager(FakeClient())
    vector = [0.25] * tc.EMBEDDING_DIM
    assert manager.upsert_task_vectors([
        {
            "id": doc_id,
            "embedding": vector,
            "process_name": "app.exe",
            "window_title": "Title",
            "category": "work",
            "timestamp": 123.0,
            "document": "app.exe | Title",
        }
        for doc_id in ("10", "2", "7")
    ]) == 3
    snapshot = manager.start_task_vectors_export(
        export_id="export-snapshot-0001",
    )
    assert snapshot["state"] == "preparing"
    snapshot = _wait_for_export(manager, snapshot["export_id"])
    assert snapshot["total"] == 3
    assert manager.hot_collection.get_calls[0]["include"] == []
    assert manager.hot_collection.get_calls[0]["where"] is None

    # Rows added after begin are outside the snapshot; rows removed after begin
    # are reported explicitly instead of shifting later pages.
    manager.upsert_task_vectors([{
        "id": "3",
        "embedding": vector,
        "timestamp": 123.0,
    }])
    manager.hot_collection.rows.pop("7")

    first = manager.export_task_vectors_page(
        export_id=snapshot["export_id"],
        cursor=0,
        limit=2,
    )
    expected_blob = base64.b64encode(
        np.asarray(vector, dtype="<f4").tobytes()
    ).decode("ascii")
    assert first == {
        "ids": ["2"],
        "dimensions": tc.EMBEDDING_DIM,
        "embeddings_f32_le_b64": expected_blob,
        "missing_ids": ["7"],
        "errors": [],
        "next_cursor": 2,
        "done": False,
        "total": 3,
    }
    second = manager.export_task_vectors_page(
        export_id=snapshot["export_id"],
        cursor=first["next_cursor"],
        limit=2,
    )
    assert second == {
        "ids": ["10"],
        "dimensions": tc.EMBEDDING_DIM,
        "embeddings_f32_le_b64": expected_blob,
        "missing_ids": [],
        "errors": [],
        "next_cursor": 3,
        "done": True,
        "total": 3,
    }
    assert manager.finish_task_vectors_export(snapshot["export_id"]) is True
    with pytest.raises(ValueError, match="unknown or expired"):
        manager.export_task_vectors_page(snapshot["export_id"])


def test_a_mirrored_vector_lands_in_the_hot_layer_with_its_metadata():
    """The Rust mirror is now the only writer of new hot-layer rows.

    Metadata is the part worth pinning. Clustering selects hot vectors with a
    `timestamp` filter and the reranker scores the stored document, so a row
    that arrived with either one missing would be silently outside every
    clustering window and unscoreable — visible months later as degraded
    clustering, and never as an error.
    """
    storage = FakeStorageClient()
    manager = tc.HotColdManager(FakeClient(), storage_client=storage)
    vector = [0.25] * tc.EMBEDDING_DIM

    written = manager.upsert_task_vectors([{
        "id": "9",
        "embedding": vector,
        "timestamp": 456.0,
        "process_name": "app.exe",
        "window_title": "Title",
        "category": "work",
        "document": "app.exe | Title | OCR",
    }])

    assert written == 1
    row = manager.hot_collection.rows["9"]
    assert row["embedding"] == vector
    assert row["document"] == "app.exe | Title | OCR"
    assert row["metadata"] == {
        "screenshot_id": 9,
        "timestamp": 456.0,
        "process_name": "app.exe",
        "window_title": "Title",
        "category": "work",
        "layer": "hot",
    }


def test_the_capture_path_no_longer_encodes_or_mirrors_from_python():
    """M2.5 step 5 left exactly one MiniLM encoder, and it is not this one.

    Named rather than merely deleted: reviving any of these would restore the
    double encode the step removed, and would do it on the capture path rather
    than while the machine is idle.
    """
    for retired in (
        "add_snapshot",
        "_dual_write_rust",
        "_queue_rust_imports",
        "_flush_pending_rust_imports",
        "_queue_rust_deletes",
        "_flush_pending_rust_deletes",
        "_report_import_debt",
    ):
        assert not hasattr(tc.HotColdManager, retired), retired
    for retired in (
        "upsert_minilm_derived_embeddings",
        "delete_minilm_derived_embeddings",
        "report_minilm_import_debt",
    ):
        assert not hasattr(tc_storage_client.StorageClient, retired), retired


def test_migration_commands_require_auth_and_dispatch_to_manager():
    manager = tc.HotColdManager(FakeClient())
    denied = handle_clustering_command(
        {"command": "start_task_vectors_export", "export_id": "export-command-0001"},
        scheduler=None,
        manager=manager,
        auth_gate=lambda **_kwargs: False,
    )
    assert "AUTH_REQUIRED" in denied["error"]

    begun = handle_clustering_command(
        {"command": "start_task_vectors_export", "export_id": "export-command-0001"},
        scheduler=None,
        manager=manager,
        auth_gate=lambda **_kwargs: True,
    )
    assert begun["state"] == "preparing"
    _wait_for_export(manager, begun["export_id"])
    assert manager.hot_collection.last_get["where"] is None
    export_status = handle_clustering_command(
        {
            "command": "get_task_vectors_export_status",
            "export_id": begun["export_id"],
        },
        scheduler=None,
        manager=manager,
        auth_gate=lambda **_kwargs: True,
    )
    page = handle_clustering_command(
        {
            "command": "export_task_vectors_page",
            "export_id": begun["export_id"],
        },
        scheduler=None,
        manager=manager,
        auth_gate=lambda **_kwargs: True,
    )
    released = handle_clustering_command(
        {
            "command": "finish_task_vectors_export",
            "export_id": begun["export_id"],
        },
        scheduler=None,
        manager=manager,
        auth_gate=lambda **_kwargs: True,
    )
    assert page["status"] == "success"
    assert page["done"] is True
    assert export_status["state"] == "ready"
    assert released == {"status": "success", "released": True}

    result = handle_clustering_command(
        {
            "command": "upsert_task_vectors",
            "records": [{
                "id": "11",
                "embedding": [0.0] * tc.EMBEDDING_DIM,
                "timestamp": 1.0,
            }],
        },
        scheduler=None,
        manager=manager,
        auth_gate=lambda **_kwargs: True,
    )
    assert result == {"status": "success", "upserted": 1}
