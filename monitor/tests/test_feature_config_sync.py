"""Runtime feature-toggle propagation tests.

The `update_feature_config` IPC command must reach not only the monitor
process's own `monitor.config` state but also the classification worker child,
which snapshots its config from the environment at startup and never re-reads
it. These tests cover the dispatch layer and the parent-side proxy without
spawning real child processes.
"""

import monitor as mm
from monitor.worker_process import RestartableModelWorker


class FeatureWorker:
    """Stub worker that records the feature-config calls it receives."""

    def __init__(self):
        self.feature_calls = []
        self.fail = False

    def update_feature_config(self, clustering_enabled, classification_enabled):
        self.feature_calls.append((clustering_enabled, classification_enabled))
        if self.fail:
            raise RuntimeError("worker unreachable")
        return {"status": "success"}


class RunningProxyStub:
    """`RestartableModelWorker` test double exercising the real proxy logic."""

    # The proxy under test is a plain method; bind it onto the stub so the
    # child-forward path can be exercised without a real supervisor.
    update_feature_config = RestartableModelWorker.update_feature_config

    def __init__(self, running=True, fail_request=False):
        self.env = {
            "CARBONPAPER_CLUSTERING_ENABLED": "true",
            "CARBONPAPER_CLASSIFICATION_ENABLED": "false",
        }
        self.requests = []
        self._running = running
        self._fail_request = fail_request

    def is_running(self):
        return self._running

    def request(self, command, payload=None, timeout=120.0):
        self.requests.append((command, payload, timeout))
        if self._fail_request:
            raise TimeoutError("pipe timed out")
        return {"status": "success"}


def _snapshot_globals():
    return {
        "_model_worker": mm._model_worker,
    }


def _restore_globals(snapshot):
    for key, value in snapshot.items():
        setattr(mm, key, value)


def test_update_feature_config_updates_local_state_and_forwards_to_worker():
    snapshot = _snapshot_globals()
    worker = FeatureWorker()
    try:
        mm._model_worker = worker
        result = mm._handle_command_impl({
            "command": "update_feature_config",
            "clustering_enabled": False,
            "classification_enabled": False,
        })
        assert result["status"] == "success"
        assert result["clustering_enabled"] is False
        assert result["classification_enabled"] is False
        assert worker.feature_calls == [(False, False)]
        assert mm.config.CLUSTERING_ENABLED is False
        assert mm.config.CLASSIFICATION_ENABLED is False
    finally:
        _restore_globals(snapshot)


def test_update_feature_config_survives_worker_sync_failure():
    snapshot = _snapshot_globals()
    worker = FeatureWorker()
    worker.fail = True
    try:
        mm._model_worker = worker
        result = mm._handle_command_impl({
            "command": "update_feature_config",
            "clustering_enabled": True,
            "classification_enabled": False,
        })
        # The monitor process still reports success: the local config was
        # updated and the child will pick the new value up on next start.
        assert result["status"] == "success"
        assert result["worker_sync"]["status"] == "deferred"
        assert mm.config.CLUSTERING_ENABLED is True
        assert mm.config.CLASSIFICATION_ENABLED is False
    finally:
        _restore_globals(snapshot)


def test_update_feature_config_without_worker_still_updates_local_state():
    snapshot = _snapshot_globals()
    try:
        mm._model_worker = None
        result = mm._handle_command_impl({
            "command": "update_feature_config",
            "clustering_enabled": False,
            "classification_enabled": True,
        })
        assert result["status"] == "success"
        assert result["worker_sync"] is None
        assert mm.config.CLUSTERING_ENABLED is False
        assert mm.config.CLASSIFICATION_ENABLED is True
    finally:
        _restore_globals(snapshot)


def test_proxy_updates_environment_snapshot_and_forwards_when_running():
    proxy = RunningProxyStub(running=True)
    result = proxy.update_feature_config(False, True)

    assert result["status"] == "success"
    assert proxy.env["CARBONPAPER_CLUSTERING_ENABLED"] == "False"
    assert proxy.env["CARBONPAPER_CLASSIFICATION_ENABLED"] == "True"
    assert len(proxy.requests) == 1
    command, payload, _timeout = proxy.requests[0]
    assert command == "update_feature_config"
    assert payload["args"] == {
        "clustering_enabled": False,
        "classification_enabled": True,
    }


def test_proxy_updates_environment_without_spawning_stopped_worker():
    proxy = RunningProxyStub(running=False)
    result = proxy.update_feature_config(True, False)

    assert result["status"] == "deferred"
    assert result["reason"] == "worker not running"
    assert proxy.requests == []
    assert proxy.env["CARBONPAPER_CLUSTERING_ENABLED"] == "True"
    assert proxy.env["CARBONPAPER_CLASSIFICATION_ENABLED"] == "False"


def test_proxy_returns_deferred_when_forward_fails():
    proxy = RunningProxyStub(running=True, fail_request=True)
    result = proxy.update_feature_config(False, False)

    assert result["status"] == "deferred"
    assert "pipe timed out" in result["error"]
    assert proxy.env["CARBONPAPER_CLUSTERING_ENABLED"] == "False"
    assert proxy.env["CARBONPAPER_CLASSIFICATION_ENABLED"] == "False"
