from monitor.clustering_commands import handle_clustering_command


class FakeStorageClient:
    def __init__(self, *, background=False, session=False):
        self.background = background
        self.session = session

    def is_background_authorized(self):
        return self.background

    def is_session_valid(self):
        return self.session


class FakeManager:
    def __init__(self, storage_client):
        self._storage_client = storage_client


class FakeScheduler:
    def __init__(self, result=None):
        self.result = result or {"status": "waiting_for_index"}
        self.calls = 0

    def run_scheduled(self):
        self.calls += 1
        return self.result


def test_scheduled_clustering_requires_background_lease():
    scheduler = FakeScheduler()
    manager = FakeManager(FakeStorageClient(session=True))

    result = handle_clustering_command(
        {"command": "run_scheduled_clustering"},
        scheduler=scheduler,
        manager=manager,
        auth_gate=lambda **_kwargs: True,
    )

    assert result == {"error": "AUTH_REQUIRED"}
    assert scheduler.calls == 0


def test_manual_scheduled_slice_accepts_live_ui_session_without_background_lease():
    scheduler = FakeScheduler({"status": "success"})
    manager = FakeManager(FakeStorageClient(session=True))

    result = handle_clustering_command(
        {"command": "run_scheduled_clustering", "manual": True},
        scheduler=scheduler,
        manager=manager,
        auth_gate=lambda **_kwargs: False,
    )

    assert result == {"status": "success", "result": {"status": "success"}}
    assert scheduler.calls == 1


def test_scheduled_waiting_for_index_result_is_forwarded_unchanged():
    scheduler = FakeScheduler({"status": "waiting_for_index"})
    manager = FakeManager(FakeStorageClient(background=True))

    result = handle_clustering_command(
        {"command": "run_scheduled_clustering"},
        scheduler=scheduler,
        manager=manager,
        auth_gate=lambda **_kwargs: False,
    )

    assert result == {
        "status": "success",
        "result": {"status": "waiting_for_index"},
    }


def test_scheduled_request_without_storage_client_fails_closed():
    scheduler = FakeScheduler({"status": "success"})
    manager = object()

    result = handle_clustering_command(
        {"command": "run_scheduled_clustering"},
        scheduler=scheduler,
        manager=manager,
        auth_gate=lambda **_kwargs: True,
    )

    assert result == {"error": "AUTH_REQUIRED"}
    assert scheduler.calls == 0


def test_scheduled_already_running_is_reported_as_deferred_error():
    scheduler = FakeScheduler({"status": "already_running"})
    manager = FakeManager(FakeStorageClient(background=True))

    result = handle_clustering_command(
        {"command": "run_scheduled_clustering"},
        scheduler=scheduler,
        manager=manager,
        auth_gate=lambda **_kwargs: False,
    )

    assert result == {
        "error": "CLUSTERING_ALREADY_RUNNING",
        "result": {"status": "already_running"},
    }
