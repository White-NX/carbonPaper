import monitor as mm


class DummyWorker:
    def __init__(self):
        self.calls = []

    def request(self, command, payload=None, timeout=120.0):
        self.calls.append((command, payload, timeout))
        if command == "enqueue_ocr_postprocess":
            return {"status": "success", "postprocess_enqueued": True, "worker_protocol": 3}
        if command == "classify":
            return {"status": "success", "category": "Development", "confidence": 0.9}
        return {"status": "success"}

    def get_stats(self):
        return {"processed_count": 1}

    def classify(self, title, ocr_text, process_name=""):
        self.calls.append(("classify", {"title": title, "ocr_text": ocr_text, "process_name": process_name}, 30))
        return "Development", 0.9

    def pause(self):
        self.calls.append(("pause", None, None))

    def resume(self):
        self.calls.append(("resume", None, None))

    def stop(self):
        self.calls.append(("stop", None, None))


class DummyScheduler:
    def __init__(self):
        self.last_args = None

    def run_now(self, start_time=None, end_time=None, clustering_mode="auto", manual=False):
        self.last_args = {
            "start_time": start_time,
            "end_time": end_time,
            "clustering_mode": clustering_mode,
            "manual": manual,
        }
        return {"n_clusters": 2, "n_noise": 1}


def _snapshot_globals():
    return {
        "_auth_token": mm._auth_token,
        "_last_seq_no": mm._last_seq_no,
        "_seen_seq_nos": set(mm._seen_seq_nos),
        "_model_worker": mm._model_worker,
        "_classifier": mm._classifier,
        "_clustering_scheduler": mm._clustering_scheduler,
        "_clustering_manager": mm._clustering_manager,
        "_clustering_scheduler_active": mm._clustering_scheduler_active,
        "_last_clustering_session_valid": mm._last_clustering_session_valid,
        "_storage_pipe": mm._storage_pipe,
    }


def _restore_globals(snapshot):
    for key, value in snapshot.items():
        setattr(mm, key, value)
    mm._seen_seq_nos.clear()
    mm._seen_seq_nos.update(snapshot["_seen_seq_nos"])
    mm.paused_event.clear()
    mm.stop_event.clear()


def test_enqueue_postprocess_dispatches_to_model_worker():
    snapshot = _snapshot_globals()
    worker = DummyWorker()
    try:
        mm._auth_token = None
        mm._last_seq_no = -1
        mm._model_worker = worker
        result = mm._handle_command_impl({
            "command": "enqueue_ocr_postprocess",
            "screenshot_id": 7,
            "window_title": "Editor",
            "ocr_text": "text",
        })
    finally:
        _restore_globals(snapshot)

    assert result["postprocess_enqueued"] is True
    assert worker.calls == [
        (
            "enqueue_ocr_postprocess",
            {"request": {
                "command": "enqueue_ocr_postprocess",
                "screenshot_id": 7,
                "window_title": "Editor",
                "ocr_text": "text",
            }},
            120,
        )
    ]


def test_classification_dispatch_returns_normalized_confidence():
    snapshot = _snapshot_globals()
    worker = DummyWorker()
    try:
        mm._model_worker = worker
        mm._classifier = worker
        result = mm._handle_command_impl({
            "command": "classify",
            "title": "Editor",
            "ocr_text": "text",
            "process_name": "code.exe",
        })
    finally:
        _restore_globals(snapshot)

    assert result == {
        "status": "success",
        "category": "Development",
        "category_confidence": 0.9,
    }


def test_run_clustering_requires_unlocked_session(monkeypatch):
    snapshot = _snapshot_globals()
    scheduler = DummyScheduler()
    try:
        mm._clustering_scheduler = scheduler
        monkeypatch.setattr(mm, "_sync_clustering_scheduler_auth_gate", lambda force=False: False)
        result = mm._handle_command_impl({"command": "run_clustering"})
    finally:
        _restore_globals(snapshot)

    assert "AUTH_REQUIRED" in result["error"]
    assert scheduler.last_args is None


def test_run_clustering_parses_numeric_range(monkeypatch):
    snapshot = _snapshot_globals()
    scheduler = DummyScheduler()
    try:
        mm._clustering_scheduler = scheduler
        monkeypatch.setattr(mm, "_sync_clustering_scheduler_auth_gate", lambda force=False: True)
        result = mm._handle_command_impl({
            "command": "run_clustering",
            "start_time": "1000",
            "end_time": 2000,
            "clustering_mode": "full",
            "manual": True,
        })
    finally:
        _restore_globals(snapshot)

    assert result["status"] == "success"
    assert scheduler.last_args == {
        "start_time": 1000.0,
        "end_time": 2000.0,
        "clustering_mode": "full",
        "manual": True,
    }


def test_auth_token_and_sequence_number_guard():
    snapshot = _snapshot_globals()
    try:
        mm._auth_token = "secret-token"
        mm._last_seq_no = 8
        mm._seen_seq_nos.clear()
        mm._seen_seq_nos.add(8)
        auth_fail = mm._handle_command_impl({"command": "status", "_auth_token": "wrong", "_seq_no": 9})
        seq_fail = mm._handle_command_impl({"command": "status", "_auth_token": "secret-token", "_seq_no": 8})
        ok = mm._handle_command_impl({"command": "status", "_auth_token": "secret-token", "_seq_no": 10})
    finally:
        _restore_globals(snapshot)

    assert "Authentication failed" in auth_fail["error"]
    assert "Replayed or expired sequence number" in seq_fail["error"]
    assert "error" not in ok


def test_retired_index_health_command_is_rejected():
    snapshot = _snapshot_globals()
    try:
        result = mm._handle_command_impl({"command": "index_health"})
    finally:
        _restore_globals(snapshot)

    assert result == {"error": "unknown command"}
