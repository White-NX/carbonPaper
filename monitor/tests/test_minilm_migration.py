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
        self.dual_writes = []
        self.reported_pending = []
        self.reported_debt = []
        self.pending = []
        self.deletes = []
        self.fail_deletes = False
        self.fail_upserts = False
        # screenshot ids the fake Rust side rejects and will never accept.
        self.permanently_rejected = set()

    def upsert_minilm_derived_embeddings(self, records, pending_imports=0):
        self.reported_pending.append(pending_imports)
        if self.fail_upserts:
            return tc_storage_client.MinilmMirrorResult.whole_batch_failed()
        accepted = [
            record for record in records
            if str(record["screenshot_id"]) not in self.permanently_rejected
        ]
        dropped = [
            str(record["screenshot_id"]) for record in records
            if str(record["screenshot_id"]) in self.permanently_rejected
        ]
        if accepted:
            self.dual_writes.append(accepted)
        return tc_storage_client.MinilmMirrorResult(
            delivered=not dropped,
            retry_ids=[],
            dropped_ids=dropped,
        )

    def report_minilm_import_debt(self, pending_imports):
        self.reported_debt.append(pending_imports)
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


def test_failed_dual_write_is_queued_and_retried_from_chroma(monkeypatch):
    # A mirror that fails must not be forgotten: Rust serves semantic
    # retrieval from this store, so a dropped vector is a screenshot that can
    # never be found again by natural-language search.
    storage = FakeStorageClient()
    storage.fail_upserts = True
    manager = tc.HotColdManager(FakeClient(), storage_client=storage)
    vector = np.full((tc.EMBEDDING_DIM,), 0.25, dtype=np.float32)
    monkeypatch.setattr(tc.TaskEmbedder, "is_model_available", staticmethod(lambda: True))
    monkeypatch.setattr(manager._embedder, "encode_single", lambda _text: vector)

    manager.add_snapshot(9, "app.exe", "Title", "OCR", 456.0, "work")

    # Chroma stays authoritative and keeps the row; only the mirror is owed.
    assert "9" in manager.hot_collection.rows
    assert storage.dual_writes == []
    assert manager._pending_rust_imports == {"9"}

    # The queue is durable, so a monitor restart still owes the same vector.
    restored = tc.HotColdManager(manager._client, storage_client=storage)
    assert restored._pending_rust_imports == {"9"}

    storage.fail_upserts = False
    restored._flush_pending_rust_imports()

    assert restored._pending_rust_imports == set()
    assert storage.dual_writes == [[{
        "screenshot_id": 9,
        "embedding": vector.tolist(),
    }]]


def test_dual_write_reports_the_debt_that_excludes_the_batch_in_flight(monkeypatch):
    # Rust stands down while this number is non-zero, so counting the batch
    # currently being delivered would keep retrieval on Python forever.
    storage = FakeStorageClient()
    manager = tc.HotColdManager(FakeClient(), storage_client=storage)
    vector = np.full((tc.EMBEDDING_DIM,), 0.5, dtype=np.float32)
    monkeypatch.setattr(tc.TaskEmbedder, "is_model_available", staticmethod(lambda: True))
    monkeypatch.setattr(manager._embedder, "encode_single", lambda _text: vector)

    manager.add_snapshot(4, "app.exe", "Title", "OCR", 456.0, "work")
    assert storage.reported_pending == [0]

    manager._queue_rust_imports(["11", "12"])
    storage.reported_pending.clear()
    manager._dual_write_rust(["13"], [vector])
    assert storage.reported_pending == [2]


def test_import_retry_forgets_ids_chroma_no_longer_holds():
    # An expired or deleted screenshot has nothing left to mirror. Keeping it
    # queued would hold semantic retrieval on Python indefinitely.
    storage = FakeStorageClient()
    manager = tc.HotColdManager(FakeClient(), storage_client=storage)

    manager._queue_rust_imports(["404"])
    manager._flush_pending_rust_imports()

    assert manager._pending_rust_imports == set()
    assert storage.dual_writes == []


def test_a_permanently_rejected_mirror_is_dropped_rather_than_retried_forever(monkeypatch):
    # Chroma keeps documents for screenshots the user deleted until they age
    # out, so a queued id can become one Rust rejects every single time. Left
    # queued it would hold the debt above zero — and Rust retrieval switched
    # off — for as long as the queue survives, which is to say forever.
    storage = FakeStorageClient()
    manager = tc.HotColdManager(FakeClient(), storage_client=storage)
    vector = np.full((tc.EMBEDDING_DIM,), 0.25, dtype=np.float32)
    monkeypatch.setattr(tc.TaskEmbedder, "is_model_available", staticmethod(lambda: True))
    monkeypatch.setattr(manager._embedder, "encode_single", lambda _text: vector)

    manager.add_snapshot(3, "app.exe", "Title", "OCR", 456.0, "work")
    manager.add_snapshot(4, "app.exe", "Title", "OCR", 457.0, "work")
    # Screenshot 3 is deleted from SQLite while its mirror is queued.
    storage.permanently_rejected.add("3")
    manager._queue_rust_imports(["3", "4"])

    manager._flush_pending_rust_imports()

    # The poisoned row is gone and the healthy one in the same batch settled.
    assert manager._pending_rust_imports == set()
    # And Rust is told the debt is clear, so retrieval can come back.
    assert storage.reported_debt[-1] == 0


def test_only_the_rows_that_failed_are_queued_not_the_whole_batch(monkeypatch):
    # The mirror is a batch call, but the debt gates a user-visible feature.
    # Queueing rows Rust already accepted would overstate how far behind the
    # index is and keep retrieval on Python longer than the facts justify.
    storage = FakeStorageClient()
    storage.permanently_rejected.add("21")
    manager = tc.HotColdManager(FakeClient(), storage_client=storage)
    vector = np.full((tc.EMBEDDING_DIM,), 0.5, dtype=np.float32)

    manager._dual_write_rust(["20", "21", "22"], [vector, vector, vector])

    # 21 is permanently rejected, so it is dropped, not queued; 20 and 22 were
    # accepted in the same call and are not owed at all.
    assert manager._pending_rust_imports == set()
    assert [record["screenshot_id"] for record in storage.dual_writes[0]] == [20, 22]


def test_a_queue_that_survives_a_restart_is_reported_before_the_next_capture():
    # Rust's debt counter is process-global and starts at zero. Without this
    # report it would rank a knowably incomplete index until the next capture
    # happens to write — which, with the monitor paused, may be never.
    storage = FakeStorageClient()
    manager = tc.HotColdManager(FakeClient(), storage_client=storage)
    manager._queue_rust_imports(["31", "32"])

    storage.reported_debt.clear()
    restored = tc.HotColdManager(manager._client, storage_client=storage)

    assert restored._pending_rust_imports == {"31", "32"}
    assert storage.reported_debt == [2]


def test_a_flush_that_writes_nothing_still_reports_the_resulting_debt():
    # Ids Chroma no longer holds settle without any dual-write carrying the new
    # number across, so the flush has to report it itself.
    storage = FakeStorageClient()
    manager = tc.HotColdManager(FakeClient(), storage_client=storage)
    manager._queue_rust_imports(["404", "405"])

    storage.reported_debt.clear()
    manager._flush_pending_rust_imports()

    assert manager._pending_rust_imports == set()
    assert storage.dual_writes == []
    assert storage.reported_debt == [0]


def _journal_lines():
    path = tc.HotColdManager._rust_import_retry_path()
    try:
        with open(path, "r", encoding="utf-8") as stream:
            return [line.strip() for line in stream if line.strip()]
    except FileNotFoundError:
        return []


def test_the_import_queue_is_journalled_by_appending_and_compacts_on_removal():
    # Queueing happens on the capture path and the queue is largest exactly
    # when the mirror has been failing, so growth must not cost a full rewrite.
    storage = FakeStorageClient()
    manager = tc.HotColdManager(FakeClient(), storage_client=storage)

    manager._queue_rust_imports(["7"])
    manager._queue_rust_imports(["8"])
    assert _journal_lines() == ["7", "8"]

    # Re-queueing an id already owed appends nothing.
    manager._queue_rust_imports(["8"])
    assert _journal_lines() == ["7", "8"]

    # A removal cannot be appended, so it compacts instead.
    manager._flush_pending_rust_imports()
    assert _journal_lines() == []
    assert manager._pending_rust_imports == set()

    # The JSON array earlier builds wrote still loads.
    with open(tc.HotColdManager._rust_import_retry_path(), "w", encoding="utf-8") as stream:
        stream.write('["41", "42"]')
    restored = tc.HotColdManager(manager._client, storage_client=storage)
    assert restored._pending_rust_imports == {"41", "42"}


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
