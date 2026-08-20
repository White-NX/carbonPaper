import storage_client as sc


def _capture_requests(client, responses):
    requests = []

    def fake_send(request, timeout=sc.DEFAULT_REVERSE_IPC_TIMEOUT_SECS):
        requests.append((request, timeout))
        response = responses.pop(0)
        return response(request) if callable(response) else response

    client._send_request = fake_send
    return requests


def test_storage_client_clustering_and_category_payload_contract():
    client = sc.StorageClient("test-pipe")
    requests = _capture_requests(client, [
        {"status": "success", "data": {"screenshots": [], "total": 0}},
        {"status": "success", "data": {"updated": True}},
    ])

    assert client.list_screenshots_for_clustering(10.0, 20.0, 3, 9) == {
        "status": "success",
        "data": {"screenshots": [], "total": 0},
    }
    assert client.update_screenshot_category(9, "Development", 0.75) is True
    assert requests == [
        ({
            "command": "list_screenshots_for_clustering",
            "start_ts": 10.0,
            "end_ts": 20.0,
            "offset": 3,
            "limit": 9,
        }, sc.DEFAULT_REVERSE_IPC_TIMEOUT_SECS),
        ({
            "command": "update_screenshot_category",
            "screenshot_id": 9,
            "category": "Development",
            "category_confidence": 0.75,
        }, sc.DEFAULT_REVERSE_IPC_TIMEOUT_SECS),
    ]


def test_storage_client_postprocess_payload_contract():
    client = sc.StorageClient("test-pipe")
    requests = _capture_requests(client, [
        {"status": "success", "data": {"updated": True}},
        {"status": "success", "data": {"updated": True}},
    ])

    assert client.set_ocr_postprocess_status(12, "pending", "foreground_busy") is True
    assert client.record_ocr_postprocess_retry(12, "failed") is True
    assert requests == [
        ({
            "command": "set_ocr_postprocess_status",
            "screenshot_id": 12,
            "status": "pending",
            "error": "foreground_busy",
        }, sc.DEFAULT_REVERSE_IPC_TIMEOUT_SECS),
        ({
            "command": "record_ocr_postprocess_retry",
            "screenshot_id": 12,
            "error": "failed",
        }, sc.DEFAULT_REVERSE_IPC_TIMEOUT_SECS),
    ]


def test_storage_client_bge_bridge_payload_and_retry_contract():
    client = sc.StorageClient("test-pipe")
    requests = _capture_requests(client, [
        {"status": "success", "data": {"dimensions": 2, "vectors": [[0.0, 1.0]]}},
    ])

    assert client.embed_bge_texts(["alpha"]) == {
        "dimensions": 2,
        "vectors": [[0.0, 1.0]],
    }
    assert requests == [
        ({"command": "bge_embed_texts", "texts": ["alpha"]}, 150),
    ]
    assert "bge_embed_texts" in sc.READ_RETRY_COMMANDS


def test_retired_python_backend_commands_are_absent():
    for retired_method in (
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
