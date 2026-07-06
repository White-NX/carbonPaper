import numpy as np

import task_clustering as tc


def test_scheduler_skips_when_model_not_available(monkeypatch):
    class Manager:
        def __init__(self):
            self.calls = 0

        def run_clustering(
            self,
            auto_compress=True,
            clustering_mode="auto",
            manual=False,
            allow_full_low_memory=False,
        ):
            self.calls += 1
            return {"status": "success"}

    manager = Manager()
    scheduler = tc.ClusteringScheduler(manager)
    monkeypatch.setattr(tc.TaskEmbedder, "is_model_available", staticmethod(lambda: False))

    result = scheduler._do_run()

    assert result is False
    assert manager.calls == 0
    assert scheduler.get_config()["running"] is False


def test_scheduler_skips_when_system_not_idle_before_model_check(monkeypatch):
    class Manager:
        def __init__(self):
            self.calls = 0

        def run_clustering(
            self,
            auto_compress=True,
            clustering_mode="auto",
            manual=False,
            allow_full_low_memory=False,
        ):
            self.calls += 1
            return {"status": "success"}

    class StorageClient:
        def __init__(self):
            self.calls = 0

        def get_idle_state(self):
            self.calls += 1
            return {"is_idle": False, "idle_secs": 2, "fullscreen_exclusive": False}

    manager = Manager()
    storage_client = StorageClient()
    scheduler = tc.ClusteringScheduler(manager, storage_client=storage_client)
    monkeypatch.setattr(
        tc.TaskEmbedder,
        "is_model_available",
        staticmethod(lambda: (_ for _ in ()).throw(AssertionError("model check should wait for idle"))),
    )

    result = scheduler._do_run()

    assert result is False
    assert storage_client.calls == 1
    assert manager.calls == 0
    assert scheduler.get_config()["running"] is False


def test_scheduler_skips_when_idle_state_is_malformed(monkeypatch):
    class Manager:
        def __init__(self):
            self.calls = 0

        def run_clustering(
            self,
            auto_compress=True,
            clustering_mode="auto",
            manual=False,
            allow_full_low_memory=False,
        ):
            self.calls += 1
            return {"status": "success"}

    class StorageClient:
        def get_idle_state(self):
            return None

    manager = Manager()
    scheduler = tc.ClusteringScheduler(manager, storage_client=StorageClient())
    monkeypatch.setattr(
        tc.TaskEmbedder,
        "is_model_available",
        staticmethod(lambda: (_ for _ in ()).throw(AssertionError("model check should wait for valid idle state"))),
    )

    result = scheduler._do_run()

    assert result is False
    assert manager.calls == 0
    assert scheduler.get_config()["running"] is False


def test_run_now_returns_already_running():
    class Manager:
        def run_clustering(
            self,
            auto_compress=True,
            start_time=None,
            end_time=None,
            clustering_mode="auto",
            manual=False,
            allow_full_low_memory=False,
        ):
            return {"status": "success"}

    scheduler = tc.ClusteringScheduler(Manager())
    scheduler._running = True

    result = scheduler.run_now()
    assert result == {"status": "already_running"}


def test_run_now_updates_last_run_only_for_default_mode(monkeypatch):
    class Manager:
        def __init__(self):
            self.calls = []

        def run_clustering(
            self,
            start_time=None,
            end_time=None,
            auto_compress=True,
            clustering_mode="auto",
            manual=False,
            allow_full_low_memory=False,
        ):
            self.calls.append((
                start_time,
                end_time,
                auto_compress,
                clustering_mode,
                manual,
                allow_full_low_memory,
            ))
            return {"status": "success"}

    manager = Manager()
    scheduler = tc.ClusteringScheduler(manager)

    save_calls = {"count": 0}
    scheduler._save_config = lambda: save_calls.__setitem__("count", save_calls["count"] + 1)

    res_default = scheduler.run_now()
    first_last_run = scheduler.get_config()["last_run"]

    res_range = scheduler.run_now(start_time=100.0, end_time=200.0)
    second_last_run = scheduler.get_config()["last_run"]

    assert res_default["status"] == "success"
    assert res_range["status"] == "success"
    assert manager.calls == [
        (None, None, True, "auto", False, False),
        (100.0, 200.0, False, "auto", True, False),
    ]
    assert first_last_run > 0
    assert second_last_run == first_last_run
    assert save_calls["count"] == 1


def test_manual_auto_clustering_without_range_prompts_for_large_input(monkeypatch):
    manager = tc.HotColdManager(None)
    threshold = tc.MANUAL_CLUSTERING_PROMPT_THRESHOLD

    monkeypatch.setattr(
        manager,
        "estimate_clustering_inputs",
        lambda start_time=None, end_time=None: {
            "count": threshold,
            "memory": {"low_memory": False},
        },
    )

    result = manager.run_clustering(clustering_mode="auto", manual=True)

    assert result["status"] == "needs_user_choice"
    assert result["n_total"] == threshold
    assert result["reason"] == "large_range"


def test_manual_auto_clustering_rechecks_prompt_after_backfill(monkeypatch):
    manager = tc.HotColdManager(None)
    threshold = 3
    vectors = np.zeros((threshold + 1, tc.EMBEDDING_DIM), dtype=np.float32)
    ids = [str(i) for i in range(threshold + 1)]
    metas = [{"timestamp": float(i)} for i in range(threshold + 1)]

    class Embedder:
        def load(self):
            return None

        def unload(self):
            return None

    class Engine:
        def run(self, *_args, **_kwargs):
            raise AssertionError("full clustering should wait for user choice")

        def run_sampled_assignment(self, *_args, **_kwargs):
            raise AssertionError("batched clustering should wait for user choice")

    monkeypatch.setattr(tc, "MANUAL_CLUSTERING_PROMPT_THRESHOLD", threshold)
    monkeypatch.setattr(tc, "memory_status_for_clustering", lambda _count: {"low_memory": False})
    monkeypatch.setattr(
        manager,
        "estimate_clustering_inputs",
        lambda start_time=None, end_time=None: {
            "count": threshold - 1,
            "memory": {"low_memory": False},
        },
    )
    monkeypatch.setattr(manager, "get_hot_vectors", lambda: (vectors[:0], [], []))
    monkeypatch.setattr(manager, "_backfill_from_screenshots", lambda start_time=None, end_time=None: threshold + 1)
    monkeypatch.setattr(manager, "get_all_hot_vectors", lambda: (vectors, ids, metas))
    manager._embedder = Embedder()
    manager._engine = Engine()

    result = manager.run_clustering(clustering_mode="auto", manual=True)

    assert result["status"] == "needs_user_choice"
    assert result["n_total"] == threshold + 1
    assert result["estimate"]["count"] == threshold + 1
    assert result["reason"] == "large_range"
