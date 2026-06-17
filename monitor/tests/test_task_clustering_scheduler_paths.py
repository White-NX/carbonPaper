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
