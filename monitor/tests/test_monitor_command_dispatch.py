import monitor as mm


def _snapshot_globals():
    return {
        "_auth_token": mm._auth_token,
        "_last_seq_no": mm._last_seq_no,
        "_seen_seq_nos": set(mm._seen_seq_nos),
        "_storage_pipe": mm._storage_pipe,
    }


def _restore_globals(snapshot):
    for key, value in snapshot.items():
        setattr(mm, key, value)
    mm._seen_seq_nos.clear()
    mm._seen_seq_nos.update(snapshot["_seen_seq_nos"])
    mm.paused_event.clear()
    mm.stop_event.clear()


def test_classification_dispatch_is_retired():
    snapshot = _snapshot_globals()
    try:
        mm._auth_token = None
        for command in ("enqueue_ocr_postprocess", "classify", "classify_debug", "add_anchor", "get_anchors", "remove_local_anchors_by_process"):
            result = mm._handle_command_impl({"command": command})
            assert "unknown command" in result["error"].lower()
    finally:
        _restore_globals(snapshot)


def test_auth_token_and_sequence_number_guard():
    snapshot = _snapshot_globals()
    try:
        mm._auth_token = "secret-token"
        mm._last_seq_no = 8
        mm._seen_seq_nos.clear()
        mm._seen_seq_nos.add(8)
        auth_fail = mm._handle_command_impl({"command": "status", "_auth_token": "wrong", "_seq_no": 9})
        seq_fail = mm._handle_command_impl({"command": "status", "_auth_token": "secret-token", "_seq_no": 8})
        ok = mm._handle_command_impl({"command": "status", "_auth_token": "secret-token", "_seq_no": 10})
    finally:
        _restore_globals(snapshot)

    assert "Authentication failed" in auth_fail["error"]
    assert "Replayed or expired sequence number" in seq_fail["error"]
    assert "error" not in ok


def test_retired_index_health_command_is_rejected():
    snapshot = _snapshot_globals()
    try:
        result = mm._handle_command_impl({"command": "index_health"})
    finally:
        _restore_globals(snapshot)

    assert result == {"error": "unknown command"}
