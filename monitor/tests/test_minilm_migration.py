import base64

import numpy as np
import pytest
import time

import task_clustering as tc
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
        self.dual_writes = []
        self.pending = []
        self.deletes = []
        self.fail_deletes = False

    def upsert_minilm_derived_embeddings(self, records):
        self.dual_writes.append(records)
        return True

    def delete_minilm_derived_embeddings(self, screenshot_ids):
        if self.fail_deletes:
            return False
        self.deletes.append(screenshot_ids)
        return True

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


def test_new_snapshot_dual_writes_after_chroma_success(monkeypatch):
    storage = FakeStorageClient()
    manager = tc.HotColdManager(FakeClient(), storage_client=storage)
    vector = np.full((tc.EMBEDDING_DIM,), 0.5, dtype=np.float32)
    monkeypatch.setattr(tc.TaskEmbedder, "is_model_available", staticmethod(lambda: True))
    monkeypatch.setattr(manager._embedder, "encode_single", lambda _text: vector)

    manager.add_snapshot(9, "app.exe", "Title", "OCR", 456.0, "work")

    assert "9" in manager.hot_collection.rows
    assert storage.dual_writes == [[{
        "screenshot_id": 9,
        "embedding": vector.tolist(),
    }]]
    assert storage.pending == [9]


def test_dual_write_chunks_large_backfill_pages():
    storage = FakeStorageClient()
    manager = tc.HotColdManager(FakeClient(), storage_client=storage)
    ids = [str(index) for index in range(1, 66)]
    vectors = np.zeros((65, tc.EMBEDDING_DIM), dtype=np.float32)

    manager._dual_write_rust(ids, vectors)

    assert [len(batch) for batch in storage.dual_writes] == [32, 32, 1]


def test_queued_rust_deletes_flush_and_survive_ipc_failure():
    storage = FakeStorageClient()
    manager = tc.HotColdManager(FakeClient(), storage_client=storage)

    manager._queue_rust_deletes(["12", "3"])
    assert storage.deletes == [[3, 12]]
    assert manager._pending_rust_deletes == set()

    storage.fail_deletes = True
    manager._queue_rust_deletes(["7"])
    assert manager._pending_rust_deletes == {"7"}

    # The retry file is the durable source: a fresh manager reloads the ids
    # that were queued while IPC was failing.
    restored = tc.HotColdManager(FakeClient(), storage_client=storage)
    assert restored._pending_rust_deletes == {"7"}


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
