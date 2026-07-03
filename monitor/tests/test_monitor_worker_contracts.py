import inspect

import pytest

import monitor as mm
from monitor.worker_process import RestartableModelWorker


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
