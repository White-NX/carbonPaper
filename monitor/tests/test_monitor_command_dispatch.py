import monitor as mm


class DummyOcrWorker:
    def __init__(self, enabled=True, should_raise=False):
        self.enable_vector_store = enabled
        self.should_raise = should_raise
        self.calls = []
        self.index_health_calls = []
        self.retry_calls = []

    def search_by_natural_language(self, query, n_results, offset, process_names, start_time, end_time):
        self.calls.append(
            {
                "query": query,
                "n_results": n_results,
                "offset": offset,
                "process_names": process_names,
                "start_time": start_time,
                "end_time": end_time,
            }
        )
        if self.should_raise:
            raise RuntimeError("search failed")
        return [{"id": "doc-1", "metadata": {"process_name": "chrome.exe"}}]

    def get_stats(self):
        return {"processed_count": 1}

    def get_index_health(self, refresh=False):
        self.index_health_calls.append(refresh)
        return {
            "status": "success",
            "worker_available": True,
            "worker_started": bool(refresh),
            "stats": {"vector_stats": {"count": 7}},
            "postprocess": {"vector_retry_backlog_count": 2},
        }

    def retry_vector_indexing(self, limit=32):
        self.retry_calls.append(limit)
        return {"status": "success", "enqueued": min(limit, 2)}


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
        "_ocr_worker": mm._ocr_worker,
        "_clustering_scheduler": mm._clustering_scheduler,
        "_clustering_manager": mm._clustering_manager,
        "_clustering_scheduler_active": mm._clustering_scheduler_active,
        "_last_clustering_session_valid": mm._last_clustering_session_valid,
        "_storage_pipe": mm._storage_pipe,
    }


def _restore_globals(snapshot):
    mm._auth_token = snapshot["_auth_token"]
    mm._last_seq_no = snapshot["_last_seq_no"]
    mm._seen_seq_nos.clear()
    mm._seen_seq_nos.update(snapshot["_seen_seq_nos"])
    mm._ocr_worker = snapshot["_ocr_worker"]
    mm._clustering_scheduler = snapshot["_clustering_scheduler"]
    mm._clustering_manager = snapshot["_clustering_manager"]
    mm._clustering_scheduler_active = snapshot["_clustering_scheduler_active"]
    mm._last_clustering_session_valid = snapshot["_last_clustering_session_valid"]
    mm._storage_pipe = snapshot["_storage_pipe"]
    mm.paused_event.clear()
    mm.stop_event.clear()


def test_search_nl_normalizes_filters_and_timestamps(monkeypatch):
    snapshot = _snapshot_globals()
    worker = DummyOcrWorker(enabled=True)

    try:
        mm._auth_token = None
        mm._last_seq_no = -1
        mm._ocr_worker = worker

        req = {
            "command": "search_nl",
            "query": "hello",
            "limit": 5,
            "offset": 2,
            "process_names": ["chrome.exe", "", "   ", 100, "code.exe"],
            "start_time": "100.5",
            "end_time": "",
        }

        result = mm._handle_command_impl(req)

        assert result["status"] == "success"
        assert len(worker.calls) == 1

        call = worker.calls[0]
        assert call["query"] == "hello"
        assert call["n_results"] == 5
        assert call["offset"] == 2
        assert call["process_names"] == ["chrome.exe", "code.exe"]
        assert call["start_time"] == 100.5
        assert call["end_time"] is None
    finally:
        _restore_globals(snapshot)


def test_search_nl_reports_disabled_vector_store():
    snapshot = _snapshot_globals()
    try:
        mm._ocr_worker = DummyOcrWorker(enabled=False)
        result = mm._handle_command_impl({"command": "search_nl", "query": "test"})
        assert "Vector store not enabled" in result["error"]
    finally:
        _restore_globals(snapshot)


def test_search_nl_propagates_worker_error():
    snapshot = _snapshot_globals()
    try:
        mm._ocr_worker = DummyOcrWorker(enabled=True, should_raise=True)
        result = mm._handle_command_impl({"command": "search_nl", "query": "test"})
        assert "search failed" in result["error"]
    finally:
        _restore_globals(snapshot)


def test_run_clustering_requires_unlocked_session(monkeypatch):
    snapshot = _snapshot_globals()
    scheduler = DummyScheduler()

    try:
        mm._clustering_scheduler = scheduler
        monkeypatch.setattr(mm, "_sync_clustering_scheduler_auth_gate", lambda force=False: False)

        result = mm._handle_command_impl({"command": "run_clustering"})

        assert "AUTH_REQUIRED" in result["error"]
        assert scheduler.last_args is None
    finally:
        _restore_globals(snapshot)


def test_run_clustering_parses_numeric_range(monkeypatch):
    snapshot = _snapshot_globals()
    scheduler = DummyScheduler()

    try:
        mm._clustering_scheduler = scheduler
        monkeypatch.setattr(mm, "_sync_clustering_scheduler_auth_gate", lambda force=False: True)

        result = mm._handle_command_impl(
            {
                "command": "run_clustering",
                "start_time": "1000",
                "end_time": 2000,
                "clustering_mode": "full",
                "manual": True,
            }
        )

        assert result["status"] == "success"
        assert scheduler.last_args == {
            "start_time": 1000.0,
            "end_time": 2000.0,
            "clustering_mode": "full",
            "manual": True,
        }
    finally:
        _restore_globals(snapshot)


def test_auth_token_and_sequence_number_guard():
    snapshot = _snapshot_globals()
    try:
        mm._auth_token = "secret-token"
        mm._last_seq_no = 8
        mm._seen_seq_nos.clear()
        mm._seen_seq_nos.add(8)

        auth_fail = mm._handle_command_impl(
            {"command": "status", "_auth_token": "wrong", "_seq_no": 9}
        )
        assert "Authentication failed" in auth_fail["error"]

        seq_fail = mm._handle_command_impl(
            {"command": "status", "_auth_token": "secret-token", "_seq_no": 8}
        )
        assert "Replayed or expired sequence number" in seq_fail["error"]

        ok = mm._handle_command_impl(
            {"command": "status", "_auth_token": "secret-token", "_seq_no": 10}
        )
        assert "error" not in ok
    finally:
        _restore_globals(snapshot)


def test_sequence_guard_accepts_out_of_order_concurrent_window_and_rejects_replay():
    snapshot = _snapshot_globals()
    try:
        mm._auth_token = "secret-token"
        mm._last_seq_no = -1
        mm._seen_seq_nos.clear()

        high = mm._handle_command_impl(
            {"command": "status", "_auth_token": "secret-token", "_seq_no": 11}
        )
        lower = mm._handle_command_impl(
            {"command": "status", "_auth_token": "secret-token", "_seq_no": 10}
        )
        replay = mm._handle_command_impl(
            {"command": "status", "_auth_token": "secret-token", "_seq_no": 10}
        )

        assert "error" not in high
        assert "error" not in lower
        assert "Replayed or expired sequence number" in replay["error"]
        assert mm._last_seq_no == 11
    finally:
        _restore_globals(snapshot)


def test_status_uses_cached_clustering_auth_without_sync_gate(monkeypatch):
    snapshot = _snapshot_globals()
    try:
        mm._auth_token = None
        mm._last_seq_no = -1
        mm._ocr_worker = None
        mm._clustering_scheduler = DummyScheduler()
        mm._clustering_scheduler_active = True
        mm._last_clustering_session_valid = True
        mm._storage_pipe = "storage-pipe"

        def fail_if_called(force=False):
            raise AssertionError("status must not call auth gate synchronously")

        monkeypatch.setattr(mm, "_sync_clustering_scheduler_auth_gate", fail_if_called)

        result = mm._handle_command_impl({"command": "status"})

        assert result["clustering_auth_unlocked"] is True
        assert result["clustering_scheduler_active"] is True
    finally:
        _restore_globals(snapshot)


def test_index_health_dispatches_to_worker_with_refresh_flag():
    snapshot = _snapshot_globals()
    worker = DummyOcrWorker(enabled=True)

    try:
        mm._auth_token = None
        mm._last_seq_no = -1
        mm._ocr_worker = worker

        result = mm._handle_command_impl({"command": "index_health", "refresh": True})

        assert result["status"] == "success"
        assert result["worker_started"] is True
        assert result["stats"]["vector_stats"]["count"] == 7
        assert result["postprocess"]["vector_retry_backlog_count"] == 2
        assert worker.index_health_calls == [True]
    finally:
        _restore_globals(snapshot)


def test_index_health_reports_unavailable_without_worker():
    snapshot = _snapshot_globals()
    try:
        mm._auth_token = None
        mm._last_seq_no = -1
        mm._ocr_worker = None

        result = mm._handle_command_impl({"command": "index_health"})

        assert result["status"] == "success"
        assert result["worker_available"] is False
        assert result["worker_started"] is False
    finally:
        _restore_globals(snapshot)


def test_retry_vector_indexing_dispatches_to_worker():
    snapshot = _snapshot_globals()
    worker = DummyOcrWorker(enabled=True)

    try:
        mm._auth_token = None
        mm._last_seq_no = -1
        mm._ocr_worker = worker

        result = mm._handle_command_impl({"command": "retry_vector_indexing", "limit": 5})

        assert result == {"status": "success", "enqueued": 2}
        assert worker.retry_calls == [5]
    finally:
        _restore_globals(snapshot)
