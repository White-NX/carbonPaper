import smart_cluster_worker as scw


def test_tick_skips_malformed_idle_state_without_crashing():
    class StorageClient:
        def __init__(self):
            self.list_calls = 0

        def smart_cluster_count_pending(self):
            return 1

        def get_idle_state(self):
            return None

        def smart_cluster_list_enabled(self):
            self.list_calls += 1
            return []

    storage = StorageClient()
    worker = scw.SmartClusterWorker()
    worker._storage_client = storage

    try:
        assert worker._tick(force=False) is False
        assert storage.list_calls == 0
    finally:
        worker._storage_client = None
