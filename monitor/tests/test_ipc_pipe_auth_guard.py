import monitor.ipc_pipe as ipc_pipe
import struct


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
        "_read_framed_json_message",
        lambda _h: '{"command":"status","_auth_token":"t","_seq_no":1}',
    )

    def fake_write(_h, data):
        state["writes"].append(data)
        return 0, len(data)

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

    wire = b"".join(state["writes"])
    frame_len = struct.unpack("<I", wire[:4])[0]
    decoded = wire[4:4 + frame_len].decode("utf-8")
    assert '"ok": true' in decoded
    assert "Access denied" not in decoded


def test_client_handler_reuses_connection_when_keepalive(monkeypatch):
    server = object.__new__(ipc_pipe._NamedPipeServer)
    state = {"requests": [], "writes": [], "closed": False}

    def fake_handler(req):
        state["requests"].append(req["command"])
        return {"ok": req["command"]}

    payloads = iter([
        '{"command":"status","_auth_token":"t","_seq_no":1,"_ipc_keepalive":true}',
        '{"command":"pause","_auth_token":"t","_seq_no":2,"_ipc_keepalive":false}',
    ])

    server.handler = fake_handler
    server.stop_event = type("StopEvent", (), {"is_set": lambda self: False})()

    monkeypatch.setenv("CARBON_PARENT_PID", "4242")
    monkeypatch.setattr(ipc_pipe.win32pipe, "GetNamedPipeClientProcessId", lambda _h: 4242)
    monkeypatch.setattr(ipc_pipe.os, "getppid", lambda: 1111)
    monkeypatch.setattr(ipc_pipe, "_read_framed_json_message", lambda _h: next(payloads))

    def fake_write(_h, data):
        state["writes"].append(data)
        return 0, len(data)

    monkeypatch.setattr(ipc_pipe.win32file, "WriteFile", fake_write)
    monkeypatch.setattr(ipc_pipe.win32file, "FlushFileBuffers", lambda _h: None)
    monkeypatch.setattr(
        ipc_pipe.win32file,
        "CloseHandle",
        lambda _h: state.__setitem__("closed", True),
    )

    server._client_handler(object())

    assert state["requests"] == ["status", "pause"]
    assert state["closed"] is True
    wire = b"".join(state["writes"])
    first_len = struct.unpack("<I", wire[:4])[0]
    second_start = 4 + first_len
    second_len = struct.unpack("<I", wire[second_start:second_start + 4])[0]
    assert b'"ok": "status"' in wire[4:4 + first_len]
    assert b'"ok": "pause"' in wire[second_start + 4:second_start + 4 + second_len]


def test_client_handler_caps_keepalive_requests(monkeypatch):
    server = object.__new__(ipc_pipe._NamedPipeServer)
    state = {"requests": 0}
    server.handler = lambda _req: state.__setitem__("requests", state["requests"] + 1) or {"ok": True}
    server.stop_event = type("StopEvent", (), {"is_set": lambda self: False})()
    server.max_requests_per_connection = 2

    monkeypatch.setenv("CARBON_PARENT_PID", "4242")
    monkeypatch.setattr(ipc_pipe.win32pipe, "GetNamedPipeClientProcessId", lambda _h: 4242)
    monkeypatch.setattr(ipc_pipe.os, "getppid", lambda: 1111)
    monkeypatch.setattr(
        ipc_pipe,
        "_read_framed_json_message",
        lambda _h: '{"command":"status","_ipc_keepalive":true}',
    )
    monkeypatch.setattr(ipc_pipe.win32file, "WriteFile", lambda _h, data: (0, len(data)))
    monkeypatch.setattr(ipc_pipe.win32file, "FlushFileBuffers", lambda _h: None)
    monkeypatch.setattr(ipc_pipe.win32file, "CloseHandle", lambda _h: None)

    server._client_handler(object())

    assert state["requests"] == 2
