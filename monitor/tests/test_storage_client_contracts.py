import storage_client as sc


def _capture_requests(client, responses):
    requests = []

    def fake_send(request):
        requests.append(request)
        response = responses.pop(0)
        return response(request) if callable(response) else response

    client._send_request = fake_send
    return requests


def test_storage_client_screenshot_lifecycle_payload_contract():
    client = sc.StorageClient("test-pipe")
    requests = _capture_requests(
        client,
        [
            {"status": "success", "data": {"id": 42}},
            {"status": "success", "data": {"committed": True}},
            {"status": "success", "data": {"aborted": True}},
        ],
    )

    assert client.save_screenshot_temp(
        b"image-bytes",
        image_hash="hash-1",
        width=800,
        height=600,
        window_title="Editor",
        process_name="code.exe",
        metadata={"source": "contract-test"},
    ) == {"id": 42}
    assert client.commit_screenshot("42", [{"text": "hello", "confidence": 0.9}]) == {"committed": True}
    assert client.abort_screenshot("43", reason="ocr failed") == {"aborted": True}

    assert requests[0] == {
        "command": "save_screenshot_temp",
        "image_data": "aW1hZ2UtYnl0ZXM=",
        "image_hash": "hash-1",
        "width": 800,
        "height": 600,
        "window_title": "Editor",
        "process_name": "code.exe",
        "metadata": {"source": "contract-test"},
    }
    assert requests[1] == {
        "command": "commit_screenshot",
        "screenshot_id": "42",
        "ocr_results": [{"text": "hello", "confidence": 0.9}],
    }
    assert requests[2] == {
        "command": "abort_screenshot",
        "screenshot_id": "43",
        "reason": "ocr failed",
    }


def test_storage_client_clustering_and_category_payload_contract():
    client = sc.StorageClient("test-pipe")
    requests = _capture_requests(
        client,
        [
            {"status": "success", "data": {"screenshots": []}},
            {"status": "success", "data": {"updated": True}},
        ],
    )

    assert client.get_screenshots_with_ocr_by_ids([7, "8"]) == {"screenshots": []}
    assert client.update_screenshot_category(9, "Development", 0.75) is True

    assert requests == [
        {
            "command": "get_screenshots_with_ocr_by_ids",
            "ids": [7, 8],
        },
        {
            "command": "update_screenshot_category",
            "screenshot_id": 9,
            "category": "Development",
            "category_confidence": 0.75,
        },
    ]


def test_storage_client_smart_cluster_reverse_ipc_payload_contract():
    client = sc.StorageClient("test-pipe")
    requests = _capture_requests(
        client,
        [
            {"status": "success", "data": {"clusters": [{"id": 1}]}},
            {"status": "success", "data": {"ok": True}},
            {"status": "success", "data": {"ids": [11, 12]}},
            {"status": "success", "data": {"ok": True, "deleted": 2}},
            {"status": "success", "data": {"count": 4}},
            {"status": "success", "data": {"ok": True}},
        ],
    )

    assert client.smart_cluster_list_enabled() == [{"id": 1}]
    assert client.smart_cluster_enqueue_pending(10) is True
    assert client.smart_cluster_peek_pending(limit=2) == [11, 12]
    assert client.smart_cluster_delete_pending([11, "12"]) is True
    assert client.smart_cluster_count_pending() == 4
    assert client.smart_cluster_record_assignment(3, 10, 0.88) is True

    assert requests == [
        {"command": "smart_cluster_list_enabled"},
        {"command": "smart_cluster_enqueue_pending", "screenshot_id": 10},
        {"command": "smart_cluster_peek_pending", "limit": 2},
        {"command": "smart_cluster_delete_pending", "ids": [11, 12]},
        {"command": "smart_cluster_count_pending"},
        {
            "command": "smart_cluster_record_assignment",
            "smart_cluster_id": 3,
            "screenshot_id": 10,
            "rerank_score": 0.88,
        },
    ]
