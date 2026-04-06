import monitor.ipc_pipe as ipc_pipe


def test_authorized_when_matches_expected_pid():
    assert ipc_pipe._is_authorized_client_pid(200, expected_pid=200, curr_ppid=100) is True


def test_authorized_when_matches_parent_pid_fallback():
    assert ipc_pipe._is_authorized_client_pid(100, expected_pid=200, curr_ppid=100) is True


def test_rejected_when_matches_neither_expected_nor_parent():
    assert ipc_pipe._is_authorized_client_pid(300, expected_pid=200, curr_ppid=100) is False


def test_authorized_with_parent_fallback_when_expected_missing():
    assert ipc_pipe._is_authorized_client_pid(100, expected_pid=None, curr_ppid=100) is True


def test_client_handler_allows_authorized_pid_and_executes_handler(monkeypatch):
    server = object.__new__(ipc_pipe._NamedPipeServer)
    state = {"called": False, "request": None, "writes": [], "closed": False}

    def fake_handler(req):
        state["called"] = True
        state["request"] = req
        return {"ok": True}

    server.handler = fake_handler

    monkeypatch.setenv("CARBON_PARENT_PID", "4242")
    monkeypatch.setattr(ipc_pipe.win32pipe, "GetNamedPipeClientProcessId", lambda _h: 4242)
    monkeypatch.setattr(ipc_pipe.os, "getppid", lambda: 1111)
    monkeypatch.setattr(
        ipc_pipe,
        "_read_complete_json_message",
        lambda _h: '{"command":"status","_auth_token":"t","_seq_no":1}',
    )

    def fake_write(_h, data):
        state["writes"].append(data)

    monkeypatch.setattr(ipc_pipe.win32file, "WriteFile", fake_write)
    monkeypatch.setattr(ipc_pipe.win32file, "FlushFileBuffers", lambda _h: None)
    monkeypatch.setattr(
        ipc_pipe.win32file,
        "CloseHandle",
        lambda _h: state.__setitem__("closed", True),
    )

    server._client_handler(object())

    assert state["called"] is True
    assert state["request"] == {"command": "status", "_auth_token": "t", "_seq_no": 1}
    assert state["closed"] is True

    decoded_writes = [
        chunk.decode("utf-8") if isinstance(chunk, (bytes, bytearray)) else str(chunk)
        for chunk in state["writes"]
    ]
    assert any('"ok": true' in item for item in decoded_writes)
    assert all("Access denied" not in item for item in decoded_writes)
