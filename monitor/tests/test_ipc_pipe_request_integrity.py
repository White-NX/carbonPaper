import json
import struct

import pytest

import monitor.ipc_pipe as ipc_pipe


class FakePyWinError(Exception):
    def __init__(self, winerror, message=""):
        super().__init__(message or f"winerror={winerror}")
        self.winerror = winerror


def test_read_framed_json_message_handles_chunked_body(monkeypatch):
    body = json.dumps({"command": "search_nl", "query": "中文"}, ensure_ascii=False).encode("utf-8")
    chunks = [(0, struct.pack("<I", len(body))), (234, body[:3]), (0, body[3:])]

    def fake_read(_handle, _size):
        if not chunks:
            return 0, b""
        return chunks.pop(0)

    monkeypatch.setattr(ipc_pipe.pywintypes, "error", FakePyWinError)
    monkeypatch.setattr(ipc_pipe.win32file, "ReadFile", fake_read)

    payload = ipc_pipe._read_framed_json_message(object(), chunk_size=3)

    parsed = json.loads(payload)
    assert parsed["command"] == "search_nl"
    assert parsed["query"] == "\u4e2d\u6587"


def test_read_framed_json_message_rejects_oversize(monkeypatch):
    def fake_read(_handle, _size):
        return 0, struct.pack("<I", 6)

    monkeypatch.setattr(ipc_pipe.pywintypes, "error", FakePyWinError)
    monkeypatch.setattr(ipc_pipe.win32file, "ReadFile", fake_read)

    with pytest.raises(ValueError, match="Invalid IPC v2 frame length"):
        ipc_pipe._read_framed_json_message(object(), max_bytes=5)


def test_read_framed_json_message_rejects_incomplete_body(monkeypatch):
    chunks = [(0, struct.pack("<I", 10)), (0, b"abc")]

    def fake_read(_handle, _size):
        if not chunks:
            return 0, b""
        return chunks.pop(0)

    monkeypatch.setattr(ipc_pipe.pywintypes, "error", FakePyWinError)
    monkeypatch.setattr(ipc_pipe.win32file, "ReadFile", fake_read)

    with pytest.raises(RuntimeError, match="Incomplete IPC frame"):
        ipc_pipe._read_framed_json_message(object())


def test_read_framed_json_message_unexpected_status_code(monkeypatch):
    chunks = [(0, struct.pack("<I", 4)), (5, b"data")]

    def fake_read(_handle, _size):
        return chunks.pop(0)

    monkeypatch.setattr(ipc_pipe.pywintypes, "error", FakePyWinError)
    monkeypatch.setattr(ipc_pipe.win32file, "ReadFile", fake_read)

    with pytest.raises(RuntimeError, match="unexpected status code"):
        ipc_pipe._read_framed_json_message(object())
