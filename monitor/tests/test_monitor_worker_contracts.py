from pathlib import Path

import pytest

import monitor as mm


def _snapshot_monitor_globals():
    return {
        "_auth_token": mm._auth_token,
        "_last_seq_no": mm._last_seq_no,
        "_storage_pipe": mm._storage_pipe,
    }


def _restore_monitor_globals(snapshot):
    for key, value in snapshot.items():
        setattr(mm, key, value)
    mm.paused_event.clear()
    mm.stop_event.clear()


def test_monitor_cannot_restart_the_retired_classification_worker():
    source = Path(mm.__file__).read_text(encoding="utf-8")
    for retired in ("worker_process", "_model_worker", "_classifier", "classify_debug", "enqueue_ocr_postprocess"):
        assert retired not in source
    assert not (Path(mm.__file__).parent / "worker_process.py").exists()
    assert not (Path(mm.__file__).parents[1] / "classifier.py").exists()


def test_legacy_vector_export_commands_are_gone():
    # The Chroma copy that these served is retired; Rust settles the index
    # sentinels by explicit discard now and never asks Python for vectors.
    source = Path(mm.__file__).read_text(encoding="utf-8")
    for retired in ("LegacyVectorExporter", "_clip_exporter", "_minilm_exporter", "chromadb", "_vectors_export"):
        assert retired not in source
    assert not (Path(mm.__file__).parents[1] / "legacy_vector_export.py").exists()
    assert not (Path(mm.__file__).parents[1] / "collection_export.py").exists()
    snapshot = _snapshot_monitor_globals()
    try:
        mm._auth_token = None
        mm._last_seq_no = -1
        for command in ("start_clip_vectors_export", "export_task_vectors_page", "finish_clip_vectors_export"):
            assert mm._handle_command_impl({"command": command, "export_id": "x"}) == {"error": "unknown command"}
    finally:
        _restore_monitor_globals(snapshot)
