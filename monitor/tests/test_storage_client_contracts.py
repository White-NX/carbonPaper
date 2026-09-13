import storage_client as sc
import pytest


def _capture_requests(client, responses):
    requests = []

    def fake_send(request, timeout=sc.DEFAULT_REVERSE_IPC_TIMEOUT_SECS):
        requests.append((request, timeout))
        response = responses.pop(0)
        return response(request) if callable(response) else response

    client._send_request = fake_send
    return requests


def test_storage_client_clustering_payload_contract():
    client = sc.StorageClient("test-pipe")
    requests = _capture_requests(client, [{"status": "success", "data": {"screenshots": [], "total": 0}}])
    assert client.list_screenshots_for_clustering(10.0, 20.0, 3, 9)["status"] == "success"
    assert requests == [({"command": "list_screenshots_for_clustering", "start_ts": 10.0,
                         "end_ts": 20.0, "offset": 3, "limit": 9}, sc.DEFAULT_REVERSE_IPC_TIMEOUT_SECS)]


def test_retired_python_backend_commands_are_absent():
    for retired_method in (
        "embed_bge_texts", "complete_staged_postprocess", "defer_staged_postprocess",
        "update_screenshot_category", "set_ocr_postprocess_status", "record_ocr_postprocess_retry",
        "get_temp_image_bytes",
        "get_screenshots_with_ocr_by_ids",
        "smart_cluster_list_enabled",
        "smart_cluster_enqueue_pending",
        "smart_cluster_peek_pending",
        "smart_cluster_delete_pending",
        "smart_cluster_count_pending",
        "smart_cluster_record_assignment",
        "record_classification_python_fallback",
        "record_classification_python_inference",
        "save_screenshot",
    ):
        assert not hasattr(sc.StorageClient, retired_method), retired_method

    for retired_command in (
        "bge_embed_texts", "complete_staged_postprocess", "defer_staged_postprocess",
        "update_screenshot_category", "set_ocr_postprocess_status", "record_ocr_postprocess_retry",
        "get_temp_image",
        "get_screenshots_with_ocr_by_ids",
        "smart_cluster_list_enabled",
        "smart_cluster_enqueue_pending",
        "smart_cluster_peek_pending",
        "smart_cluster_delete_pending",
        "smart_cluster_count_pending",
        "smart_cluster_record_assignment",
        "classification_record_python_fallback",
        "classification_record_python_inference",
        "save_screenshot",
    ):
        assert retired_command not in sc.IDEMPOTENT_RETRY_COMMANDS
        assert retired_command not in sc.READ_RETRY_COMMANDS
