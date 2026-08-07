import ast
import inspect
from pathlib import Path

import pytest

import monitor as mm
from monitor import worker_process
from monitor.worker_process import (
    CLIP_VECTOR_COMMANDS,
    RestartableModelWorker,
    _handle_clip_vector_command,
)


class ProxyShapedSearchWorker:
    """Fake the production proxy shape: business args arrive as keywords."""

    enable_vector_store = True

    def __init__(self):
        self.calls = []

    def search_by_natural_language(self, **kwargs):
        self.calls.append(kwargs)
        return [{"id": "doc-1"}]


def _snapshot_monitor_globals():
    return {
        "_auth_token": mm._auth_token,
        "_last_seq_no": mm._last_seq_no,
        "_ocr_worker": mm._ocr_worker,
        "_clustering_scheduler": mm._clustering_scheduler,
        "_clustering_manager": mm._clustering_manager,
        "_clustering_scheduler_active": mm._clustering_scheduler_active,
        "_last_clustering_session_valid": mm._last_clustering_session_valid,
        "_storage_pipe": mm._storage_pipe,
    }


def _restore_monitor_globals(snapshot):
    mm._auth_token = snapshot["_auth_token"]
    mm._last_seq_no = snapshot["_last_seq_no"]
    mm._ocr_worker = snapshot["_ocr_worker"]
    mm._clustering_scheduler = snapshot["_clustering_scheduler"]
    mm._clustering_manager = snapshot["_clustering_manager"]
    mm._clustering_scheduler_active = snapshot["_clustering_scheduler_active"]
    mm._last_clustering_session_valid = snapshot["_last_clustering_session_valid"]
    mm._storage_pipe = snapshot["_storage_pipe"]
    mm.paused_event.clear()
    mm.stop_event.clear()


def test_search_nl_dispatch_uses_proxy_keyword_contract():
    """The monitor dispatcher must call the model worker with keyword args."""
    snapshot = _snapshot_monitor_globals()
    worker = ProxyShapedSearchWorker()

    try:
        mm._auth_token = None
        mm._last_seq_no = -1
        mm._ocr_worker = worker

        result = mm._handle_command_impl(
            {
                "command": "search_nl",
                "query": "invoice",
                "limit": 5,
                "offset": 2,
                "process_names": ["chrome.exe", "", 3, "code.exe"],
                "start_time": "100.5",
                "end_time": "",
            }
        )

        assert result == {"status": "success", "results": [{"id": "doc-1"}]}
        assert worker.calls == [
            {
                "query": "invoice",
                "n_results": 5,
                "offset": 2,
                "process_names": ["chrome.exe", "code.exe"],
                "start_time": 100.5,
                "end_time": None,
            }
        ]
    finally:
        _restore_monitor_globals(snapshot)


def test_model_worker_search_accepts_public_ocr_service_signature(monkeypatch):
    """The restartable proxy should expose the same public search signature."""
    worker = RestartableModelWorker(storage_pipe=None, data_dir="unused", env={})
    calls = []

    def fake_request(command, payload=None, timeout=120.0):
        calls.append({"command": command, "payload": payload, "timeout": timeout})
        return {"status": "success", "results": [{"id": "doc-1"}]}

    monkeypatch.setattr(worker, "request", fake_request)

    result = worker.search_by_natural_language(
        "invoice",
        n_results=5,
        offset=2,
        process_names=["chrome.exe"],
        start_time=100.5,
        end_time=None,
    )

    assert result == [{"id": "doc-1"}]
    assert calls == [
        {
            "command": "search_by_natural_language",
            "payload": {
                "args": {
                    "query": "invoice",
                    "n_results": 5,
                    "offset": 2,
                    "process_names": ["chrome.exe"],
                    "start_time": 100.5,
                    "end_time": None,
                }
            },
            "timeout": pytest.approx(120.0),
        }
    ]


def test_model_worker_search_signature_is_canonical():
    signature = inspect.signature(RestartableModelWorker.search_by_natural_language)

    assert list(signature.parameters) == [
        "self",
        "query",
        "n_results",
        "offset",
        "process_names",
        "start_time",
        "end_time",
    ]


def test_model_worker_classifier_proxy_payload_contract(monkeypatch):
    worker = RestartableModelWorker(storage_pipe=None, data_dir="unused", env={})
    calls = []
    responses = {
        "classify": {"status": "success", "category": "Development", "confidence": 0.87},
        "classify_debug": {"status": "success", "data": {"category": "Development"}},
        "add_anchor": {"status": "success", "data": {"title_global_added": True}},
        "remove_anchor": {"status": "success", "removed": True},
        "remove_local_anchors_by_process": {"status": "success", "removed_count": 3},
        "get_categories": {"status": "success", "categories": ["Development"]},
        "get_anchors": {"status": "success", "anchors": {"Development": []}},
        "delete_vector_image": {"status": "success", "ok": True},
    }

    def fake_request(command, payload=None, timeout=120.0):
        calls.append({"command": command, "payload": payload, "timeout": timeout})
        return responses[command]

    monkeypatch.setattr(worker, "request", fake_request)

    assert worker.classify("Editor", "text", process_name="code.exe") == ("Development", 0.87)
    assert worker.classify_debug("Editor", "text", process_name="code.exe") == {"category": "Development"}
    assert worker.add_anchor(
        "Development",
        "Editor",
        ocr_text="text",
        old_category="未分类",
        process_name="code.exe",
    ) == {"title_global_added": True}
    assert worker.remove_anchor("Development", "Editor") is True
    assert worker.remove_local_anchors_by_process("Development", "code.exe") == 3
    assert worker.get_categories() == ["Development"]
    assert worker.get_anchors() == {"Development": []}
    assert worker.delete_vector_image("hash-1") is True

    assert calls == [
        {
            "command": "classify",
            "payload": {
                "args": {
                    "title": "Editor",
                    "ocr_text": "text",
                    "process_name": "code.exe",
                }
            },
            "timeout": 30,
        },
        {
            "command": "classify_debug",
            "payload": {
                "args": {
                    "title": "Editor",
                    "ocr_text": "text",
                    "process_name": "code.exe",
                }
            },
            "timeout": 30,
        },
        {
            "command": "add_anchor",
            "payload": {
                "args": {
                    "category": "Development",
                    "title": "Editor",
                    "ocr_text": "text",
                    "old_category": "未分类",
                    "process_name": "code.exe",
                }
            },
            "timeout": 30,
        },
        {
            "command": "remove_anchor",
            "payload": {"category": "Development", "title": "Editor"},
            "timeout": 30,
        },
        {
            "command": "remove_local_anchors_by_process",
            "payload": {"category": "Development", "process_name": "code.exe"},
            "timeout": 30,
        },
        {"command": "get_categories", "payload": None, "timeout": 30},
        {"command": "get_anchors", "payload": None, "timeout": 30},
        {
            "command": "delete_vector_image",
            "payload": {"image_hash": "hash-1"},
            "timeout": 30,
        },
    ]


def test_model_worker_index_health_does_not_cold_start_by_default(monkeypatch):
    worker = RestartableModelWorker(storage_pipe=None, data_dir="unused", env={})
    calls = []

    monkeypatch.setattr(worker, "status_snapshot", lambda: {"alive": False, "state": "stopped"})
    monkeypatch.setattr(
        worker,
        "request",
        lambda *args, **kwargs: calls.append((args, kwargs)) or {"status": "success"},
    )

    result = worker.get_index_health(refresh=False)

    assert result["status"] == "success"
    assert result["worker_available"] is True
    assert result["worker_started"] is False
    assert result["stats"]["watchdog"]["alive"] is False
    assert calls == []


def test_model_worker_index_health_and_retry_payload_contract(monkeypatch):
    worker = RestartableModelWorker(storage_pipe=None, data_dir="unused", env={})
    calls = []
    responses = {
        "get_index_health": {
            "status": "success",
            "stats": {"vector_stats": {"count": 3}},
            "postprocess": {"vector_retry_backlog_count": 1},
        },
        "retry_vector_indexing": {"status": "success", "enqueued": 1},
    }

    def fake_request(command, payload=None, timeout=120.0):
        calls.append({"command": command, "payload": payload, "timeout": timeout})
        return responses[command]

    monkeypatch.setattr(worker, "request", fake_request)

    health = worker.get_index_health(refresh=True)
    retry = worker.retry_vector_indexing(limit=5)

    assert health["worker_available"] is True
    assert health["worker_started"] is True
    assert retry == {"status": "success", "enqueued": 1}
    assert calls == [
        {"command": "get_index_health", "payload": None, "timeout": 30},
        {"command": "retry_vector_indexing", "payload": {"limit": 5}, "timeout": 30},
    ]


# ---------------------------------------------------------------------------
# Which process owns what
#
# The monitor process holds a `RestartableModelWorker`, which is a proxy; the
# models and the Chroma collections live in the child it supervises. The CLIP
# export commands were written as if `_ocr_worker` were an in-process
# `OCRService`: they read `_ocr_worker.vector_store`, found the proxy's `None`
# placeholder, and answered "Vector store not enabled" on every machine, which
# failed the CLIP migration at startup and silently dropped every mirrored
# vector on the capture path. The tests below exist so that shape of mistake
# fails in CI instead of in a user's log.
# ---------------------------------------------------------------------------


def _monitor_proxy_attribute_usage():
    """Attribute names the monitor dispatcher takes off the `_ocr_worker` global.

    Returns `(read, called)`: every name accessed, and the subset invoked as a
    method. Read from the source rather than from a test double, because a
    double is exactly what hid the original defect — it happened to implement
    the attribute the production proxy lacks.
    """
    tree = ast.parse(Path(mm.__file__).read_text(encoding="utf-8"))
    read, called = set(), set()

    def is_proxy_attribute(node):
        return (
            isinstance(node, ast.Attribute)
            and isinstance(node.value, ast.Name)
            and node.value.id == "_ocr_worker"
        )

    for node in ast.walk(tree):
        if is_proxy_attribute(node):
            read.add(node.attr)
        if isinstance(node, ast.Call) and is_proxy_attribute(node.func):
            called.add(node.func.attr)
    return read, called


def test_monitor_dispatch_only_uses_attributes_the_production_proxy_provides():
    proxy = RestartableModelWorker(storage_pipe=None, data_dir="unused", env={})
    read, called = _monitor_proxy_attribute_usage()

    # Guards the scan itself: a rename of the `_ocr_worker` global would
    # otherwise turn both assertions below into vacuous truths.
    assert read, "the AST scan found no _ocr_worker attribute access at all"
    assert sorted(name for name in read if not hasattr(proxy, name)) == []
    assert sorted(name for name in called if not callable(getattr(proxy, name, None))) == []


def test_production_proxy_does_not_fake_an_in_process_vector_store():
    """The absent attribute is the fix; a `None` one would restore the defect.

    With no `vector_store` on the proxy, code that reaches for the collection
    from the monitor process raises where it is written. With a `None` one,
    every truthiness check silently reports a disabled store instead.
    """
    proxy = RestartableModelWorker(storage_pipe=None, data_dir="unused", env={})

    assert not hasattr(proxy, "vector_store")
    assert proxy.enable_vector_store is True


def test_clip_vector_commands_are_implemented_on_the_production_proxy():
    for command in CLIP_VECTOR_COMMANDS:
        assert callable(getattr(RestartableModelWorker, command, None)), command


def test_worker_main_routes_the_whole_clip_vector_command_set():
    """The child dispatcher must branch on the shared set, not on copies of it.

    Otherwise a command added to `CLIP_VECTOR_COMMANDS` and to the proxy would
    reach the child and fall through to "Unknown worker command".
    """
    assert "CLIP_VECTOR_COMMANDS" in inspect.getsource(worker_process._worker_main)


# The payload each command carries into the child, and what the child sends
# back. The child's half is nested under `result` on the wire; the monitor
# flattens it, because Rust deserialises these fields off the top level of the
# reply (`migration_support.rs::ExportStatus` / `ExportPage`).
_CLIP_REQUESTS = {
    "start_clip_vectors_export": {"export_id": "clip-run-1"},
    "get_clip_vectors_export_status": {"export_id": "clip-run-1"},
    "export_clip_vectors_page": {"export_id": "clip-run-1", "cursor": 0, "limit": 128},
    "finish_clip_vectors_export": {"export_id": "clip-run-1"},
    "upsert_clip_vectors": {"records": [{"image_hash": "abc123", "embedding": [0.1, 0.2]}]},
}
_CLIP_CHILD_RESULTS = {
    "start_clip_vectors_export": {"export_id": "clip-run-1", "state": "preparing", "total": 0},
    "get_clip_vectors_export_status": {"state": "ready", "total": 7},
    "export_clip_vectors_page": {
        "ids": ["200c8cdc45dea346718762f394f2ac40"],
        "dimensions": 512,
        "embeddings_f32_le_b64": "AAAAAA==",
        "missing_ids": [],
        "errors": [],
        "total": 7,
        "next_cursor": 1,
        "done": False,
    },
    "finish_clip_vectors_export": {"released": True},
    "upsert_clip_vectors": {"written": 2},
}


def test_clip_vector_commands_dispatch_through_the_production_proxy(monkeypatch):
    """End to end over the real proxy class, with only the child stubbed out."""
    snapshot = _snapshot_monitor_globals()
    proxy = RestartableModelWorker(storage_pipe=None, data_dir="unused", env={})
    calls = []

    def fake_request(command, payload=None, timeout=120.0):
        calls.append({"command": command, "payload": payload, "timeout": timeout})
        return {"status": "success", "result": _CLIP_CHILD_RESULTS[command]}

    monkeypatch.setattr(proxy, "request", fake_request)
    monkeypatch.setattr(mm, "_sync_clustering_scheduler_auth_gate", lambda force=False: True)

    try:
        mm._auth_token = None
        mm._last_seq_no = -1
        mm._ocr_worker = proxy

        replies = {
            command: mm._handle_command_impl({"command": command, **_CLIP_REQUESTS[command]})
            for command in CLIP_VECTOR_COMMANDS
        }
    finally:
        _restore_monitor_globals(snapshot)

    # Every command reached the child, in the set's own order.
    assert [call["command"] for call in calls] == list(CLIP_VECTOR_COMMANDS)
    for command, reply in replies.items():
        assert reply.get("status") == "success", (command, reply)
        assert "result" not in reply, command

    assert replies["start_clip_vectors_export"]["export_id"] == "clip-run-1"
    assert replies["get_clip_vectors_export_status"]["state"] == "ready"
    assert replies["get_clip_vectors_export_status"]["total"] == 7
    page = replies["export_clip_vectors_page"]
    assert page["ids"] == ["200c8cdc45dea346718762f394f2ac40"]
    assert page["dimensions"] == 512
    assert page["next_cursor"] == 1
    assert page["done"] is False
    assert replies["finish_clip_vectors_export"]["released"] is True
    assert replies["upsert_clip_vectors"]["written"] == 2

    by_command = {call["command"]: call for call in calls}
    assert by_command["export_clip_vectors_page"]["payload"] == {
        "export_id": "clip-run-1",
        "cursor": 0,
        "limit": 128,
    }
    assert by_command["upsert_clip_vectors"]["payload"] == {
        "records": [{"image_hash": "abc123", "embedding": [0.1, 0.2]}]
    }
    # The two calls that may find the model worker down are given the
    # supervisor's full ready budget; a shorter one would kill the child
    # mid-startup instead of waiting for it.
    assert by_command["start_clip_vectors_export"]["timeout"] == pytest.approx(180.0)
    assert by_command["upsert_clip_vectors"]["timeout"] == pytest.approx(180.0)
    assert by_command["export_clip_vectors_page"]["timeout"] == pytest.approx(120.0)


def test_clip_vector_command_refusal_reaches_the_caller_verbatim(monkeypatch):
    """A genuinely disabled store still answers with the string Rust expects."""
    snapshot = _snapshot_monitor_globals()
    proxy = RestartableModelWorker(storage_pipe=None, data_dir="unused", env={})

    monkeypatch.setattr(
        proxy,
        "request",
        lambda command, payload=None, timeout=120.0: {"error": "Vector store not enabled"},
    )
    monkeypatch.setattr(mm, "_sync_clustering_scheduler_auth_gate", lambda force=False: True)

    try:
        mm._auth_token = None
        mm._last_seq_no = -1
        mm._ocr_worker = proxy

        replies = {
            command: mm._handle_command_impl({"command": command, **_CLIP_REQUESTS[command]})
            for command in CLIP_VECTOR_COMMANDS
        }
    finally:
        _restore_monitor_globals(snapshot)

    for command, reply in replies.items():
        assert reply == {"error": "Vector store not enabled"}, command


class _FakeClipCollection:
    def __init__(self):
        self.calls = []

    def start_snapshot_export(self, export_id):
        self.calls.append(("start_snapshot_export", export_id))
        return {"export_id": export_id, "state": "preparing", "total": 0}

    def get_snapshot_export_status(self, export_id):
        self.calls.append(("get_snapshot_export_status", export_id))
        return {"state": "ready", "total": 7}

    def export_snapshot_page(self, export_id, cursor=0, limit=128):
        self.calls.append(("export_snapshot_page", export_id, cursor, limit))
        return {"ids": [], "dimensions": 512, "total": 7, "next_cursor": limit, "done": True}

    def finish_snapshot_export(self, export_id):
        self.calls.append(("finish_snapshot_export", export_id))
        return True

    def upsert_clip_vectors(self, records):
        self.calls.append(("upsert_clip_vectors", records))
        return len(records)


class _FakeChildOcrService:
    """The child's shape: it really does own the store."""

    def __init__(self, vector_store):
        self.enable_vector_store = vector_store is not None
        self.vector_store = vector_store


def test_child_dispatcher_forwards_every_clip_command_to_the_collection():
    collection = _FakeClipCollection()
    worker = _FakeChildOcrService(collection)

    responses = {
        command: _handle_clip_vector_command(command, dict(_CLIP_REQUESTS[command]), worker)
        for command in CLIP_VECTOR_COMMANDS
    }

    for command, response in responses.items():
        assert response.get("status") == "success", (command, response)
    assert responses["get_clip_vectors_export_status"]["result"] == {"state": "ready", "total": 7}
    assert responses["finish_clip_vectors_export"]["result"] == {"released": True}
    assert responses["upsert_clip_vectors"]["result"] == {"written": 1}
    assert collection.calls == [
        ("start_snapshot_export", "clip-run-1"),
        ("get_snapshot_export_status", "clip-run-1"),
        ("export_snapshot_page", "clip-run-1", 0, 128),
        ("finish_snapshot_export", "clip-run-1"),
        ("upsert_clip_vectors", [{"image_hash": "abc123", "embedding": [0.1, 0.2]}]),
    ]


def test_child_dispatcher_is_where_a_missing_store_is_reported():
    worker = _FakeChildOcrService(None)

    for command in CLIP_VECTOR_COMMANDS:
        assert _handle_clip_vector_command(command, {}, worker) == {
            "error": "Vector store not enabled"
        }


def test_child_dispatcher_turns_a_collection_failure_into_an_error_reply():
    class _FailingCollection(_FakeClipCollection):
        def start_snapshot_export(self, export_id):
            raise RuntimeError("chroma is unavailable")

    response = _handle_clip_vector_command(
        "start_clip_vectors_export",
        {"export_id": "clip-run-1"},
        _FakeChildOcrService(_FailingCollection()),
    )

    assert response == {"error": "chroma is unavailable"}


def test_child_dispatcher_rejects_a_command_it_does_not_implement():
    """Pins the guard against a new name falling through into `upsert`."""
    response = _handle_clip_vector_command(
        "clip_command_that_does_not_exist",
        {},
        _FakeChildOcrService(_FakeClipCollection()),
    )

    assert "Unknown CLIP vector command" in response["error"]
