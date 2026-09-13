import ast
import inspect
from pathlib import Path

import monitor as mm


def _snapshot_monitor_globals():
    return {
        "_auth_token": mm._auth_token,
        "_last_seq_no": mm._last_seq_no,
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


def test_monitor_cannot_restart_the_retired_classification_worker():
    source = Path(mm.__file__).read_text(encoding="utf-8")
    for retired in ("worker_process", "_model_worker", "_classifier", "classify_debug", "enqueue_ocr_postprocess"):
        assert retired not in source
    assert not (Path(mm.__file__).parent / "worker_process.py").exists()
    assert not (Path(mm.__file__).parents[1] / "classifier.py").exists()


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
