import storage_client as sc
import struct
import threading
import time


class FakePyWinError(Exception):
    def __init__(self, winerror, message=""):
        super().__init__(message or f"winerror={winerror}")
        self.winerror = winerror


def _install_win32(monkeypatch, *, create_file, write_file, read_file, flush_file, sleep_hook=None):
    monkeypatch.setattr(sc.pywintypes, "error", FakePyWinError)
    monkeypatch.setattr(sc.win32file, "CreateFile", create_file)
    monkeypatch.setattr(sc.win32file, "WriteFile", write_file)
    monkeypatch.setattr(sc.win32file, "ReadFile", read_file)
    monkeypatch.setattr(sc.win32file, "FlushFileBuffers", flush_file)
    monkeypatch.setattr(sc.win32file, "CloseHandle", lambda _h: None)
    monkeypatch.setattr(sc.win32pipe, "SetNamedPipeHandleState", lambda *_a, **_k: None)
    if sleep_hook is not None:
        monkeypatch.setattr(sc.time, "sleep", sleep_hook)


def _framed_reader(payload: bytes):
    stream = bytearray(struct.pack("<I", len(payload)) + payload)

    def read_file(_h, size):
        if not stream:
            return 0, b""
        n = min(size, len(stream))
        chunk = bytes(stream[:n])
        del stream[:n]
        return 0, chunk

    return read_file


def test_send_request_returns_ipc_error_when_connect_fails(monkeypatch):
    def create_file(*_args, **_kwargs):
        raise FakePyWinError(2, "file not found")

    _install_win32(
        monkeypatch,
        create_file=create_file,
        write_file=lambda _h, payload: (0, len(payload)),
        read_file=lambda _h, _s: (0, b""),
        flush_file=lambda _h: None,
    )

    client = sc.StorageClient("missing-pipe")
    result = client._send_request({"command": "status"})

    assert result["status"] == "error"
    assert "IPC error" in result["error"]


def test_send_request_retries_on_pipe_busy(monkeypatch):
    attempts = {"count": 0}
    sleeps = []

    def create_file(*_args, **_kwargs):
        attempts["count"] += 1
        if attempts["count"] < 4:
            raise FakePyWinError(231, "pipe busy")
        return object()

    response = b'{"status":"success"}'

    _install_win32(
        monkeypatch,
        create_file=create_file,
        write_file=lambda _h, payload: (0, len(payload)),
        read_file=_framed_reader(response),
        flush_file=lambda _h: None,
        sleep_hook=lambda sec: sleeps.append(sec),
    )

    client = sc.StorageClient("busy-pipe")
    result = client._send_request({"command": "status"})

    assert result["status"] == "success"
    assert attempts["count"] == 4
    assert sleeps == [0.02, 0.04, 0.08]


def test_send_request_returns_error_when_write_makes_no_progress(monkeypatch):
    _install_win32(
        monkeypatch,
        create_file=lambda *_a, **_k: object(),
        write_file=lambda _h, _payload: (0, 0),
        read_file=lambda _h, _s: (0, b""),
        flush_file=lambda _h: None,
    )

    client = sc.StorageClient("pipe")
    result = client._send_request({"command": "status"})

    assert result["status"] == "error"
    assert "no progress" in result["error"]


def test_send_request_returns_empty_response_after_pipe_end(monkeypatch):
    def read_file(_h, _s):
        raise FakePyWinError(109, "broken pipe")

    _install_win32(
        monkeypatch,
        create_file=lambda *_a, **_k: object(),
        write_file=lambda _h, payload: (0, len(payload)),
        read_file=read_file,
        flush_file=lambda _h: None,
    )

    client = sc.StorageClient("pipe")
    result = client._send_request({"command": "status"})

    assert result == {"status": "error", "error": "Empty response"}


def test_send_request_non_benign_flush_error_becomes_ipc_error(monkeypatch):
    _install_win32(
        monkeypatch,
        create_file=lambda *_a, **_k: object(),
        write_file=lambda _h, payload: (0, len(payload)),
        read_file=lambda _h, _s: (0, b""),
        flush_file=lambda _h: (_ for _ in ()).throw(FakePyWinError(5, "access denied")),
    )

    client = sc.StorageClient("pipe")
    result = client._send_request({"command": "status"})

    assert result["status"] == "error"
    assert "IPC error" in result["error"]


def test_send_request_times_out_waiting_for_semaphore():
    client = sc.StorageClient("pipe")
    assert client._semaphore.acquire(blocking=False)
    assert client._semaphore.acquire(blocking=False)
    try:
        result = client._send_request({"command": "status"}, timeout=0.01)
    finally:
        client._semaphore.release()
        client._semaphore.release()

    assert result["status"] == "error"
    assert result["code"] == "ipc_timeout"
    assert result["phase"] == "semaphore"
    assert result["command"] == "status"


def test_send_request_times_out_waiting_for_request_lock():
    client = sc.StorageClient("pipe")
    ready = threading.Event()
    release = threading.Event()

    def hold_lock():
        client._request_lock.acquire()
        ready.set()
        release.wait(timeout=1.0)
        client._request_lock.release()

    holder = threading.Thread(target=hold_lock, daemon=True)
    holder.start()
    assert ready.wait(timeout=1.0)
    try:
        result = client._send_request({"command": "status"}, timeout=0.01)
    finally:
        release.set()
        holder.join(timeout=1.0)

    assert result["status"] == "error"
    assert result["code"] == "ipc_timeout"
    assert result["phase"] == "request_lock"


def test_send_request_watchdog_closes_handle_during_blocking_read(monkeypatch):
    client = sc.StorageClient("pipe")
    close_calls = []
    fake_handle = object()

    monkeypatch.setattr(client, "_connect_persistent_handle", lambda: fake_handle)
    monkeypatch.setattr(sc, "_write_framed_json", lambda _handle, _payload: None)
    monkeypatch.setattr(sc.win32file, "FlushFileBuffers", lambda _handle: None)

    def slow_read(_handle):
        time.sleep(0.15)
        return {"status": "success"}

    monkeypatch.setattr(sc, "_read_framed_json", slow_read)
    monkeypatch.setattr(client, "_close_persistent_handle", lambda: close_calls.append(True))

    result = client._send_request({"command": "slow_read"}, timeout=0.1)

    assert result["status"] == "error"
    assert result["code"] == "ipc_timeout"
    assert result["phase"] == "watchdog"
    assert close_calls


def test_unsafe_write_command_is_not_retried_after_pipe_close(monkeypatch):
    state = {"create_calls": 0, "write_calls": 0}

    def create_file(*_args, **_kwargs):
        state["create_calls"] += 1
        return object()

    def write_file(_h, _payload):
        state["write_calls"] += 1
        raise FakePyWinError(232, "pipe is being closed")

    _install_win32(
        monkeypatch,
        create_file=create_file,
        write_file=write_file,
        read_file=lambda _h, _s: (0, b""),
        flush_file=lambda _h: None,
    )

    client = sc.StorageClient("pipe")
    result = client._send_request({"command": "save_screenshot_temp"})

    assert result["status"] == "error"
    assert "IPC error" in result["error"]
    assert state["create_calls"] == 1
    assert state["write_calls"] == 1


def test_circuit_breaker_opens_after_repeated_transport_failures(monkeypatch):
    attempts = {"create": 0}

    def create_file(*_args, **_kwargs):
        attempts["create"] += 1
        raise FakePyWinError(2, "file not found")

    _install_win32(
        monkeypatch,
        create_file=create_file,
        write_file=lambda _h, payload: (0, len(payload)),
        read_file=lambda _h, _s: (0, b""),
        flush_file=lambda _h: None,
    )

    client = sc.StorageClient("pipe")
    client._circuit_failure_threshold = 2
    client._circuit_cooldown_secs = 30.0

    first = client._send_request({"command": "get_auth_status"})
    second = client._send_request({"command": "get_auth_status"})
    third = client._send_request({"command": "get_auth_status"})

    assert first["status"] == "error"
    assert second["status"] == "error"
    assert third["status"] == "error"
    assert third["code"] == "ipc_circuit_open"
    assert attempts["create"] == 2

    snapshot = client.ipc_health_snapshot()
    assert snapshot["circuit_state"] == "open"
    assert snapshot["failure_count"] == 2
    assert snapshot["last_command"] == "get_auth_status"


def test_circuit_breaker_snapshot_reports_half_open_after_cooldown():
    client = sc.StorageClient("pipe")
    client._circuit_failure_threshold = 2
    client._circuit_cooldown_secs = 1.0
    client._record_ipc_failure("get_auth_status", "first")
    client._record_ipc_failure("get_auth_status", "second")
    client._circuit_open_until = time.monotonic() - 0.01

    snapshot = client.ipc_health_snapshot()

    assert snapshot["circuit_state"] == "half_open"
    assert snapshot["retry_after_secs"] == 0.0


def test_circuit_breaker_half_open_probe_resets_after_success(monkeypatch):
    _install_win32(
        monkeypatch,
        create_file=lambda *_a, **_k: object(),
        write_file=lambda _h, payload: (0, len(payload)),
        read_file=_framed_reader(b'{"status":"success"}'),
        flush_file=lambda _h: None,
    )

    client = sc.StorageClient("pipe")
    client._circuit_failure_threshold = 2
    client._circuit_cooldown_secs = 1.0
    client._record_ipc_failure("get_auth_status", "first")
    client._record_ipc_failure("get_auth_status", "second")
    client._circuit_open_until = time.monotonic() - 0.01

    result = client._send_request({"command": "get_auth_status"})

    assert result["status"] == "success"
    snapshot = client.ipc_health_snapshot()
    assert snapshot["circuit_state"] == "closed"
    assert snapshot["failure_count"] == 0
