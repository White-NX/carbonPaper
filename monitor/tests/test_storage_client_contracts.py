import storage_client as sc


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
