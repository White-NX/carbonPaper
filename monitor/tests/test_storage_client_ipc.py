import json

import storage_client as sc


class FakePyWinError(Exception):
    def __init__(self, winerror, message=""):
        super().__init__(message or f"winerror={winerror}")
        self.winerror = winerror


def install_pipe_mocks(monkeypatch, read_chunks, flush_error=None):
    state = {
        "write_calls": 0,
        "written_bytes": 0,
        "close_calls": 0,
        "read_calls": 0,
        "set_mode_calls": 0,
    }
    chunk_iter = iter(read_chunks)

    def fake_create_file(*_args, **_kwargs):
        return object()

    def fake_set_mode(*_args, **_kwargs):
        state["set_mode_calls"] += 1

    def fake_write_file(_handle, payload):
        state["write_calls"] += 1
        written = min(len(payload), 13)
        state["written_bytes"] += written
        return 0, written

    def fake_flush(_handle):
        if flush_error is not None:
            raise flush_error

    def fake_read_file(_handle, _size):
        state["read_calls"] += 1
        try:
            chunk = next(chunk_iter)
        except StopIteration:
            return 0, b""
        if isinstance(chunk, Exception):
            raise chunk
        return 0, chunk

    def fake_close_handle(_handle):
        state["close_calls"] += 1

    monkeypatch.setattr(sc.pywintypes, "error", FakePyWinError)
    monkeypatch.setattr(sc.win32file, "CreateFile", fake_create_file)
    monkeypatch.setattr(sc.win32pipe, "SetNamedPipeHandleState", fake_set_mode)
    monkeypatch.setattr(sc.win32file, "WriteFile", fake_write_file)
    monkeypatch.setattr(sc.win32file, "FlushFileBuffers", fake_flush)
    monkeypatch.setattr(sc.win32file, "ReadFile", fake_read_file)
    monkeypatch.setattr(sc.win32file, "CloseHandle", fake_close_handle)

    return state


def test_send_request_handles_utf8_split_and_partial_write(monkeypatch):
    response_obj = {"status": "success", "data": {"value": "中文"}}
    response_bytes = json.dumps(response_obj, ensure_ascii=False).encode("utf-8")
    split_at = response_bytes.index("中文".encode("utf-8")) + 1

    benign_flush_error = FakePyWinError(109, "broken pipe after response")
    state = install_pipe_mocks(
        monkeypatch,
        [response_bytes[:split_at], response_bytes[split_at:]],
        flush_error=benign_flush_error,
    )

    client = sc.StorageClient("test-pipe")
    request = {"command": "probe", "payload": "x" * (70 * 1024)}
    expected_request_len = len(json.dumps(request).encode("utf-8"))

    result = client._send_request(request)

    assert result == response_obj
    assert state["set_mode_calls"] == 1
    assert state["close_calls"] == 1
    assert state["write_calls"] > 1
    assert state["written_bytes"] == expected_request_len


def test_decrypt_many_retries_once_after_invalid_json(monkeypatch):
    client = sc.StorageClient("test-pipe")
    requests = []
    responses = [
        {"status": "error", "error": "Invalid JSON response: truncated"},
        {"status": "success", "data": {"decrypted_list": ["plain-a", "plain-b"]}},
    ]

    def fake_send(req):
        requests.append(req)
        return responses.pop(0)

    monkeypatch.setattr(client, "_send_request", fake_send)

    result = client.decrypt_many_from_chromadb(["ENC:a", "ENC:b"])

    assert result == ["plain-a", "plain-b"]
    assert len(requests) == 2


def test_decrypt_many_returns_nones_when_response_shape_mismatch(monkeypatch):
    client = sc.StorageClient("test-pipe")

    def fake_send(_req):
        return {
            "status": "success",
            "data": {"decrypted_list": ["only-one-value"]},
        }

    monkeypatch.setattr(client, "_send_request", fake_send)

    result = client.decrypt_many_from_chromadb(["ENC:a", "ENC:b"])

    assert result == [None, None]


def test_is_session_valid_reads_response_flag(monkeypatch):
    client = sc.StorageClient("test-pipe")

    monkeypatch.setattr(
        client,
        "_send_request",
        lambda _req: {"status": "success", "data": {"session_valid": True}},
    )
    assert client.is_session_valid() is True

    monkeypatch.setattr(
        client,
        "_send_request",
        lambda _req: {"status": "success", "data": {"session_valid": False}},
    )
    assert client.is_session_valid() is False


def test_list_screenshots_for_clustering_uses_expected_payload(monkeypatch):
    client = sc.StorageClient("test-pipe")
    captured = {}

    def fake_send(req):
        captured.update(req)
        return {"status": "success", "data": {"screenshots": [], "total": 0}}

    monkeypatch.setattr(client, "_send_request", fake_send)
    result = client.list_screenshots_for_clustering(
        start_ts=10.5,
        end_ts=20.5,
        offset=3,
        limit=9,
    )

    assert captured == {
        "command": "list_screenshots_for_clustering",
        "start_ts": 10.5,
        "end_ts": 20.5,
        "offset": 3,
        "limit": 9,
    }
    assert result["status"] == "success"
