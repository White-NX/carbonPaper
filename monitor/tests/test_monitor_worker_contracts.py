import ast
import inspect
from pathlib import Path

import monitor as mm
from monitor import worker_process
from monitor.worker_process import RestartableModelWorker, WORKER_PROTOCOL_VERSION


def _snapshot_monitor_globals():
    return {
        "_auth_token": mm._auth_token,
        "_last_seq_no": mm._last_seq_no,
        "_model_worker": mm._model_worker,
        "_classifier": mm._classifier,
        "_clip_exporter": mm._clip_exporter,
        "_clustering_scheduler": mm._clustering_scheduler,
        "_clustering_manager": mm._clustering_manager,
        "_clustering_scheduler_active": mm._clustering_scheduler_active,
        "_last_clustering_session_valid": mm._last_clustering_session_valid,
        "_storage_pipe": mm._storage_pipe,
    }


def _restore_monitor_globals(snapshot):
    for key, value in snapshot.items():
        setattr(mm, key, value)
    mm.paused_event.clear()
    mm.stop_event.clear()


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
    }

    def fake_request(command, payload=None, timeout=120.0):
        calls.append({"command": command, "payload": payload, "timeout": timeout})
        return responses[command]

    monkeypatch.setattr(worker, "request", fake_request)

    assert worker.classify("Editor", "text", process_name="code.exe") == ("Development", 0.87)
    assert worker.classify_debug("Editor", "text", process_name="code.exe") == {"category": "Development"}
    assert worker.add_anchor(
        "Development", "Editor", ocr_text="text", old_category="未分类", process_name="code.exe"
    ) == {"title_global_added": True}
    assert worker.remove_anchor("Development", "Editor") is True
    assert worker.remove_local_anchors_by_process("Development", "code.exe") == 3
    assert worker.get_categories() == ["Development"]
    assert worker.get_anchors() == {"Development": []}

    assert [call["command"] for call in calls] == [
        "classify",
        "classify_debug",
        "add_anchor",
        "remove_anchor",
        "remove_local_anchors_by_process",
        "get_categories",
        "get_anchors",
    ]
    assert calls[0]["payload"] == {
        "args": {"title": "Editor", "ocr_text": "text", "process_name": "code.exe"}
    }


def test_model_worker_postprocess_payload(monkeypatch):
    worker = RestartableModelWorker(storage_pipe=None, data_dir="unused", env={})
    calls = []

    def fake_request(command, payload=None, timeout=120.0):
        calls.append((command, payload, timeout))
        if command == "enqueue_ocr_postprocess":
            return {"status": "success", "postprocess_enqueued": True}
        return {"status": "success"}

    monkeypatch.setattr(worker, "request", fake_request)
    assert worker.request("enqueue_ocr_postprocess", {"request": {"screenshot_id": 1}})["status"] == "success"
    assert calls == [("enqueue_ocr_postprocess", {"request": {"screenshot_id": 1}}, 120.0)]


def test_worker_protocol_and_dispatch_have_no_retired_model_commands():
    source = inspect.getsource(worker_process._worker_main)
    assert WORKER_PROTOCOL_VERSION == 3
    for retired in (
        "search_by_natural_language",
        "upsert_clip_vectors",
        "delete_vector_image",
        "retry_vector_indexing",
        "get_index_health",
    ):
        assert retired not in source
    assert not hasattr(RestartableModelWorker, "get_index_health")


def test_monitor_dispatch_only_uses_attributes_the_production_proxy_provides():
    tree = ast.parse(Path(mm.__file__).read_text(encoding="utf-8"))
    read, called = set(), set()
    for node in ast.walk(tree):
        if (
            isinstance(node, ast.Attribute)
            and isinstance(node.value, ast.Name)
            and node.value.id == "_model_worker"
        ):
            read.add(node.attr)
        if (
            isinstance(node, ast.Call)
            and isinstance(node.func, ast.Attribute)
            and isinstance(node.func.value, ast.Name)
            and node.func.value.id == "_model_worker"
        ):
            called.add(node.func.attr)

    proxy = RestartableModelWorker(storage_pipe=None, data_dir="unused", env={})
    assert read
    assert sorted(name for name in read if not hasattr(proxy, name)) == []
    assert sorted(name for name in called if not callable(getattr(proxy, name, None))) == []


class _FakeExporter:
    def __init__(self):
        self.calls = []

    def start(self, export_id):
        self.calls.append(("start", export_id))
        return {"export_id": export_id, "state": "preparing", "total": 0}

    def status(self, export_id):
        self.calls.append(("status", export_id))
        return {"export_id": export_id, "state": "ready", "total": 2}

    def page(self, export_id, cursor=0, limit=128):
        self.calls.append(("page", export_id, cursor, limit))
        return {"ids": [], "dimensions": 512, "next_cursor": 2, "done": True, "total": 2}

    def finish(self, export_id):
        self.calls.append(("finish", export_id))
        return True


def test_legacy_clip_export_dispatch_is_monitor_owned(monkeypatch):
    snapshot = _snapshot_monitor_globals()
    exporter = _FakeExporter()
    try:
        mm._auth_token = None
        mm._last_seq_no = -1
        mm._clip_exporter = exporter
        monkeypatch.setattr(mm, "_sync_clustering_scheduler_auth_gate", lambda force=False: True)

        start = mm._handle_command_impl({"command": "start_clip_vectors_export", "export_id": "clip-run-123456"})
        status = mm._handle_command_impl({"command": "get_clip_vectors_export_status", "export_id": "clip-run-123456"})
        page = mm._handle_command_impl({"command": "export_clip_vectors_page", "export_id": "clip-run-123456", "cursor": 0, "limit": 2})
        finish = mm._handle_command_impl({"command": "finish_clip_vectors_export", "export_id": "clip-run-123456"})
    finally:
        _restore_monitor_globals(snapshot)

    assert start["state"] == "preparing"
    assert status["state"] == "ready"
    assert page["dimensions"] == 512
    assert finish["released"] is True
    assert [call[0] for call in exporter.calls] == ["start", "status", "page", "finish"]
