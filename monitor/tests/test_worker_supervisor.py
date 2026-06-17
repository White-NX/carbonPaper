import pytest
import logging

from monitor.worker_process import RestartableModelWorker
from monitor.worker_supervisor import (
    WorkerBackoffError,
    WorkerProtocolError,
    WorkerSupervisor,
    attach_response_metadata,
)


def _unused_worker(conn):
    conn.send({"status": "ready"})


class FakeProc:
    def __init__(self):
        self.pid = 12345
        self.alive = True
        self.terminated = False
        self.killed = False

    def is_alive(self):
        return self.alive

    def terminate(self):
        self.terminated = True
        self.alive = False

    def join(self, timeout=None):
        return None

    def kill(self):
        self.killed = True
        self.alive = False


class EchoConn:
    def __init__(self, response=None, poll_result=True):
        self.sent = []
        self.response = response
        self.poll_result = poll_result

    def send(self, message):
        self.sent.append(message)

    def poll(self, timeout=None):
        return self.poll_result

    def recv(self):
        if self.response is not None:
            return self.response
        return attach_response_metadata(self.sent[-1], {"status": "success", "value": 42})


def _ready_supervisor(conn, **kwargs):
    supervisor = WorkerSupervisor(
        name="TestWorker",
        target=_unused_worker,
        restart_limit=3,
        restart_window_secs=60,
        restart_cooldown_secs=30,
        **kwargs,
    )
    supervisor._proc = FakeProc()
    supervisor._conn = conn
    supervisor._state = "ready"
    return supervisor


def test_supervisor_adds_and_verifies_request_metadata():
    conn = EchoConn()
    supervisor = _ready_supervisor(conn)

    result = supervisor.request("echo", {"payload": "ok"}, timeout=1)

    assert result["status"] == "success"
    assert result["value"] == 42
    assert conn.sent == [{"command": "echo", "_request_id": 1, "payload": "ok"}]
    assert supervisor.status_snapshot()["state"] == "ready"


def test_supervisor_restarts_on_response_mismatch():
    conn = EchoConn(response={"status": "success", "stats": {}})
    supervisor = _ready_supervisor(conn)
    proc = supervisor._proc

    with pytest.raises(WorkerProtocolError):
        supervisor.request("process_ocr", timeout=1)

    assert proc.terminated
    snapshot = supervisor.status_snapshot()
    assert snapshot["last_restart_reason"] == "protocol_desync"
    assert snapshot["state"] == "stopped"


def test_supervisor_logs_response_mismatch_keys(caplog):
    conn = EchoConn(response={"status": "success", "stats": {}})
    supervisor = _ready_supervisor(conn)

    with caplog.at_level(logging.ERROR):
        with pytest.raises(WorkerProtocolError):
            supervisor.request("process_ocr", timeout=1)

    text = caplog.text
    assert "protocol desync" in text
    assert "expected_command=process_ocr" in text
    assert "result_keys=['stats', 'status']" in text


def test_supervisor_restarts_on_timeout():
    conn = EchoConn(poll_result=False)
    supervisor = _ready_supervisor(conn)
    proc = supervisor._proc

    with pytest.raises(TimeoutError):
        supervisor.request("slow_command", timeout=0.01)

    assert proc.terminated
    snapshot = supervisor.status_snapshot()
    assert snapshot["last_restart_reason"] == "request_timeout"
    assert snapshot["failure_count"] == 1


def test_supervisor_logs_slow_request(caplog):
    conn = EchoConn()
    supervisor = _ready_supervisor(conn, slow_request_secs=0.0)

    with caplog.at_level(logging.WARNING):
        supervisor.request("echo", timeout=1)

    text = caplog.text
    assert "command slow" in text
    assert "command=echo" in text
    assert "result_keys=['_command', '_request_id', 'status', 'value']" in text


def test_supervisor_enters_degraded_state_after_restart_churn():
    supervisor = WorkerSupervisor(
        name="ChurnWorker",
        target=_unused_worker,
        restart_limit=2,
        restart_window_secs=60,
        restart_cooldown_secs=30,
    )

    supervisor.restart(reason="one")
    supervisor.restart(reason="two")
    supervisor.restart(reason="three")

    snapshot = supervisor.status_snapshot()
    assert snapshot["state"] == "degraded"
    assert snapshot["degraded_until"] is not None
    with pytest.raises(WorkerBackoffError):
        supervisor.start(timeout=0.01)


def test_supervisor_stop_does_not_count_as_restart_churn():
    conn = EchoConn()
    supervisor = _ready_supervisor(conn)
    proc = supervisor._proc

    supervisor.stop()

    assert proc.terminated
    snapshot = supervisor.status_snapshot()
    assert snapshot["last_restart_reason"] == "stop"
    assert snapshot["restart_count"] == 0
    assert snapshot["state"] == "stopped"


def test_model_worker_status_does_not_send_pipe_request():
    worker = RestartableModelWorker(storage_pipe=None, data_dir="unused", env={})
    conn = EchoConn()
    worker._proc = FakeProc()
    worker._conn = conn
    worker._state = "ready"

    stats = worker.get_stats()

    assert conn.sent == []
    assert stats["watchdog"]["name"] == "CarbonModelWorker"
    assert stats["watchdog"]["alive"] is True
