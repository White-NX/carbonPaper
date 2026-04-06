import threading
import time

import numpy as np

import task_clustering as tc


class FakeCollection:
    def __init__(self, run_entered, run_release):
        self._run_entered = run_entered
        self._run_release = run_release
        self._first_range_get = True
        self.add_calls = 0

    def get(self, ids=None, where=None, include=None):
        if self._first_range_get and where is not None and include and "embeddings" in include:
            self._first_range_get = False
            self._run_entered.set()
            self._run_release.wait(timeout=3.0)
            return {"ids": [], "embeddings": [], "metadatas": []}
        if ids is not None:
            return {"ids": []}
        return {"ids": [], "embeddings": [], "metadatas": []}

    def add(self, ids, embeddings, metadatas, documents=None):
        self.add_calls += len(ids)

    def delete(self, ids):
        return None

    def upsert(self, ids, embeddings, metadatas):
        return None

    def count(self):
        return 0


class FakeClient:
    def __init__(self, collection):
        self._collection = collection
        self._collections = {
            "task_vectors": collection,
            "task_centroids": collection,
        }

    def get_or_create_collection(self, name, metadata=None):
        return self._collection


class FakeEmbedder:
    def load(self):
        return None

    def unload(self):
        return None

    def encode_single(self, _text):
        return np.zeros(tc.EMBEDDING_DIM, dtype=np.float32)

    def encode(self, texts):
        return np.zeros((len(texts), tc.EMBEDDING_DIM), dtype=np.float32)


class FakeEngine:
    def run(self, vectors, ids, metadatas, min_cluster_size, min_samples):
        return {"clusters": [], "noise_ids": []}


def test_run_clustering_does_not_deadlock_add_snapshot(monkeypatch):
    monkeypatch.setattr(tc.TaskEmbedder, "is_model_available", staticmethod(lambda: True))

    run_entered = threading.Event()
    run_release = threading.Event()
    collection = FakeCollection(run_entered, run_release)
    manager = tc.HotColdManager(FakeClient(collection))
    manager._embedder = FakeEmbedder()
    manager._engine = FakeEngine()

    run_error = []
    add_error = []

    def run_worker():
        try:
            manager.run_clustering(auto_compress=False)
        except Exception as exc:  # pragma: no cover
            run_error.append(exc)

    def add_worker():
        try:
            manager.add_snapshot(
                screenshot_id=42,
                process_name="code.exe",
                window_title="Editor",
                ocr_text="hello",
                timestamp=time.time(),
                category="Development",
            )
        except Exception as exc:  # pragma: no cover
            add_error.append(exc)

    run_thread = threading.Thread(target=run_worker, daemon=True)
    run_thread.start()

    assert run_entered.wait(timeout=1.0), "run_clustering did not reach blocking point"

    add_thread = threading.Thread(target=add_worker, daemon=True)
    add_thread.start()

    # add_snapshot should be blocked by clustering lock before release.
    time.sleep(0.15)
    assert add_thread.is_alive(), "add_snapshot should block while clustering holds lock"

    run_release.set()

    run_thread.join(timeout=2.0)
    add_thread.join(timeout=2.0)

    assert not run_thread.is_alive(), "run_clustering did not finish in time"
    assert not add_thread.is_alive(), "add_snapshot remained blocked"
    assert run_error == []
    assert add_error == []
    assert collection.add_calls == 1


def test_scheduler_can_recover_after_failed_run(monkeypatch):
    monkeypatch.setattr(tc.TaskEmbedder, "is_model_available", staticmethod(lambda: True))

    class FlakyManager:
        def __init__(self):
            self.calls = 0

        def run_clustering(self, auto_compress=True):
            self.calls += 1
            if self.calls == 1:
                raise RuntimeError("boom")
            return {"clusters": [], "noise_ids": [], "status": "success"}

    manager = FlakyManager()
    scheduler = tc.ClusteringScheduler(manager)
    scheduler._save_config = lambda: None

    first = scheduler._do_run()
    second = scheduler._do_run()

    assert first is False
    assert second is True
    assert scheduler.get_last_result()["status"] == "success"
    assert scheduler.get_config()["running"] is False
