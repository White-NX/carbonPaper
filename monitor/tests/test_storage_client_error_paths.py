import storage_client as sc


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
        read_file=lambda _h, _s: (0, response),
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
