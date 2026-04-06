import pytest
import json

import monitor.ipc_pipe as ipc_pipe


class FakePyWinError(Exception):
    def __init__(self, winerror, message=""):
        super().__init__(message or f"winerror={winerror}")
        self.winerror = winerror


def test_read_complete_json_message_handles_more_data(monkeypatch):
    utf8_tail = bytes([0xE4, 0xB8, 0xAD, 0xE6, 0x96, 0x87, 0x22, 0x7D])
    chunks = [
        (234, b'{"command":"search_nl","query":"'),
        (0, utf8_tail),
    ]

    def fake_read(_handle, _size):
        if not chunks:
            return 0, b""
        return chunks.pop(0)

    monkeypatch.setattr(ipc_pipe.pywintypes, "error", FakePyWinError)
    monkeypatch.setattr(ipc_pipe.win32file, "ReadFile", fake_read)

    payload = ipc_pipe._read_complete_json_message(object(), chunk_size=16)

    parsed = json.loads(payload)
    assert parsed["command"] == "search_nl"
    assert parsed["query"] == "\u4e2d\u6587"


def test_read_complete_json_message_rejects_oversize(monkeypatch):
    def fake_read(_handle, _size):
        return 0, b"123456"

    monkeypatch.setattr(ipc_pipe.pywintypes, "error", FakePyWinError)
    monkeypatch.setattr(ipc_pipe.win32file, "ReadFile", fake_read)

    with pytest.raises(ValueError, match="Request too large"):
        ipc_pipe._read_complete_json_message(object(), max_bytes=5)


def test_read_complete_json_message_handles_broken_pipe_after_data(monkeypatch):
    state = {"called": 0}

    def fake_read(_handle, _size):
        state["called"] += 1
        if state["called"] == 1:
            return 234, b'{"command":"status"'
        raise FakePyWinError(109, "broken pipe")

    monkeypatch.setattr(ipc_pipe.pywintypes, "error", FakePyWinError)
    monkeypatch.setattr(ipc_pipe.win32file, "ReadFile", fake_read)

    payload = ipc_pipe._read_complete_json_message(object())

    assert payload == '{"command":"status"'


def test_read_complete_json_message_unexpected_status_code(monkeypatch):
    def fake_read(_handle, _size):
        return 5, b"data"

    monkeypatch.setattr(ipc_pipe.pywintypes, "error", FakePyWinError)
    monkeypatch.setattr(ipc_pipe.win32file, "ReadFile", fake_read)

    with pytest.raises(RuntimeError, match="unexpected status code"):
        ipc_pipe._read_complete_json_message(object())
