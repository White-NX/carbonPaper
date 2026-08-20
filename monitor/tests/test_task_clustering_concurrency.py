import threading
import time

import numpy as np
import pytest

import task_clustering as tc


class FakeCollection:
    """Minimal stand-in for a ChromaDB collection.

    `rows` controls how many hot vectors the clustering fetch sees, which is
    what decides whether `run_clustering` reaches the engine or falls into the
    backfill branch.
    """

    def __init__(self, rows=12):
        self.rows = rows
        self.upsert_calls = 0
        self.upserted_ids = []
        self.add_calls = 0
        self.deleted_ids = []

    def get(self, ids=None, where=None, include=None):
        if ids is not None:
            return {"ids": []}
        if include and "embeddings" in include:
            n = self.rows
            now = time.time()
            return {
                "ids": [str(i) for i in range(1, n + 1)],
                "embeddings": [[0.0] * tc.EMBEDDING_DIM for _ in range(n)],
                "metadatas": [
                    {
                        "screenshot_id": i,
                        "timestamp": now,
                        "process_name": "code.exe",
                        "window_title": "Editor",
                        "category": "Development",
                        "layer": "hot",
                    }
                    for i in range(1, n + 1)
                ],
            }
        return {"ids": []}

    def count(self):
        return self.rows

    def upsert(self, ids, embeddings, metadatas, documents=None):
        self.upsert_calls += 1
        self.upserted_ids.extend(ids)

    def add(self, ids, embeddings, metadatas, documents=None):
        self.add_calls += 1

    def delete(self, ids):
        self.deleted_ids.extend(ids)


class FakeClient:
    def __init__(self, collection):
        self._collection = collection

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


class BlockingEngine:
    """Parks inside the engine run, which is where a real run spends longest.

    PaCMAP + HDBSCAN operate on numpy arrays the fetch already materialised, so
    the engine touches nothing shared — which is exactly why the manager lock
    must not be held across it.
    """

    def __init__(self):
        self.entered = threading.Event()
        self.release = threading.Event()
        self.runs = 0

    def run(self, vectors, ids, metadatas, min_cluster_size, min_samples):
        self.runs += 1
        self.entered.set()
        self.release.wait(timeout=10.0)
        return {"clusters": [], "noise_ids": []}


def _manager(monkeypatch, collection, engine):
    monkeypatch.setattr(tc.TaskEmbedder, "is_model_available", staticmethod(lambda: True))
    manager = tc.HotColdManager(FakeClient(collection))
    manager._embedder = FakeEmbedder()
    manager._engine = engine
    return manager


def _mirror_record(doc_id="42"):
    return {
        "id": doc_id,
        "embedding": [0.0] * tc.EMBEDDING_DIM,
        "timestamp": time.time(),
        "process_name": "code.exe",
        "window_title": "Editor",
        "category": "Development",
        "document": "code.exe | Editor | hello",
    }


def test_mirror_completes_while_clustering_is_inside_the_engine(monkeypatch):
    """The Rust vector mirror must not wait for a clustering run to finish.

    `upsert_task_vectors` executes inside a named-pipe handler thread, and the
    IPC server hands out only 8 of those. When `run_clustering` held the manager
    lock across its whole body, each idle pass parked one more handler on that
    lock for the rest of the run, and once the pool was gone every command on
    the forward pipe — `status`, `search_nl`, the OCR post-process enqueue —
    got "IPC server busy".

    Serialising was never the harmful part; blocking for minutes was. So this
    asserts the bound, not the ordering.
    """
    collection = FakeCollection()
    engine = BlockingEngine()
    manager = _manager(monkeypatch, collection, engine)

    run_error = []
    mirror_error = []
    mirror_done = threading.Event()

    def run_worker():
        try:
            manager.run_clustering(auto_compress=False)
        except Exception as exc:  # pragma: no cover
            run_error.append(exc)

    def mirror_worker():
        try:
            manager.upsert_task_vectors([_mirror_record()])
        except Exception as exc:  # pragma: no cover
            mirror_error.append(exc)
        finally:
            mirror_done.set()

    run_thread = threading.Thread(target=run_worker, daemon=True)
    run_thread.start()
    assert engine.entered.wait(timeout=5.0), "run_clustering never reached the engine"

    threading.Thread(target=mirror_worker, daemon=True).start()

    assert mirror_done.wait(timeout=2.0), (
        "the mirror blocked while clustering was inside the engine — a pipe "
        "handler thread would be parked here for the length of the run"
    )
    assert mirror_error == []
    assert collection.upsert_calls == 1
    assert collection.upserted_ids == ["42"]

    # The run really was still in the engine while the mirror went through.
    assert not engine.release.is_set()
    assert engine.runs == 1

    engine.release.set()
    run_thread.join(timeout=5.0)
    assert not run_thread.is_alive(), "run_clustering did not finish in time"
    assert run_error == []


def test_second_clustering_run_is_refused_rather_than_queued(monkeypatch):
    """Two concurrent runs would double peak memory, so the guard refuses.

    It has to refuse rather than block: a manual run arrives over IPC, and
    parking that handler until a scheduled run finishes is the same pool
    exhaustion by another route.
    """
    collection = FakeCollection()
    engine = BlockingEngine()
    manager = _manager(monkeypatch, collection, engine)

    first_thread = threading.Thread(
        target=lambda: manager.run_clustering(auto_compress=False), daemon=True
    )
    first_thread.start()
    assert engine.entered.wait(timeout=5.0)

    started = time.monotonic()
    second = manager.run_clustering(auto_compress=False)
    elapsed = time.monotonic() - started

    assert second["status"] == "already_running"
    assert elapsed < 1.0, "the second run waited instead of being refused"
    assert engine.runs == 1, "the refused run must not reach the engine"

    engine.release.set()
    first_thread.join(timeout=5.0)
    assert not first_thread.is_alive()


def test_compress_to_cold_holds_the_lock_across_expiry_read_and_delete(monkeypatch):
    """The expiry read and delete are a pair and used to be covered by the
    run-wide lock. With that gone they need the manager lock explicitly, or a
    mirror re-upserting one of those ids in between would have it deleted right
    back out again.

    The probe has to run on another thread: the manager lock is an RLock, so a
    non-blocking acquire from the thread already holding it always succeeds and
    would prove nothing.
    """
    collection = FakeCollection()
    manager = _manager(monkeypatch, collection, BlockingEngine())

    outcomes = []
    real_get = collection.get

    def probe():
        acquired = manager._lock.acquire(blocking=False)
        outcomes.append(acquired)
        if acquired:
            manager._lock.release()

    def spying_get(ids=None, where=None, include=None):
        # Only the expiry read carries a `where` with no `include`.
        if where is not None and not include:
            prober = threading.Thread(target=probe, daemon=True)
            prober.start()
            prober.join(timeout=2.0)
        return real_get(ids=ids, where=where, include=include)

    collection.get = spying_get
    manager.compress_to_cold([])

    assert outcomes, "the expiry read did not run"
    assert outcomes == [False], (
        "the expiry read/delete pair ran without the manager lock, so a mirror "
        "can slip an id back in between them"
    )


class _FakeTokenizer:
    def __init__(self, seq_len=4):
        self.seq_len = seq_len

    def __call__(self, texts, **_kwargs):
        n = len(texts)
        # The tokenizer call is where the window has to be, not the forward
        # pass: the old `encode` read `self._model` *after* tokenising, so an
        # unload landing here is what turned the next line into
        # `'NoneType' object has no attribute ...`.
        time.sleep(0.02)
        return {
            "input_ids": np.zeros((n, self.seq_len), dtype=np.int64),
            "attention_mask": np.ones((n, self.seq_len), dtype=np.int64),
        }


class _FakeSession:
    def __init__(self, seq_len=4):
        self.seq_len = seq_len

    def get_inputs(self):  # pragma: no cover - build_transformer_inputs is stubbed
        return []

    def run(self, _outputs, feeds):
        n = feeds["n"]
        time.sleep(0.005)
        return [np.zeros((n, self.seq_len, tc.EMBEDDING_DIM), dtype=np.float32)]


def test_encode_survives_an_unload_landing_mid_pass(monkeypatch):
    """`run_clustering` unloads the model in its `finally` — worth ~479 MB on
    the ONNX backend — and that unload no longer happens behind a lock that
    keeps other users away.

    Before `_acquire_runtime`, `encode` read `_tokenizer` and `_model` as
    separate unguarded attribute loads. Against the real model
    an interleaved unload failed the very first encode with
    `'NoneType' object has no attribute 'get_inputs'`.
    """
    monkeypatch.setattr(
        "onnx_utils.build_transformer_inputs",
        lambda session, encoded: {"n": len(encoded["attention_mask"])},
    )

    def fake_load(self):
        with self._lock:
            if self._model is None:
                self._tokenizer = _FakeTokenizer()
                self._model = _FakeSession()

    monkeypatch.setattr(tc.TaskEmbedder, "load", fake_load)
    monkeypatch.setattr(tc.gc, "collect", lambda *a, **k: 0)

    emb = tc.TaskEmbedder()
    emb.unload()

    texts = ["code.exe | Editor | hello"] * 3
    failures = []
    encodes = [0]
    stop = threading.Event()

    def encoder():
        while not stop.is_set():
            try:
                out = emb.encode(list(texts))
                assert out.shape == (len(texts), tc.EMBEDDING_DIM)
                encodes[0] += 1
            except Exception as exc:
                failures.append(exc)
                return

    def unloader():
        while not stop.is_set():
            emb.unload()
            time.sleep(0.001)

    threads = [
        threading.Thread(target=encoder, daemon=True),
        threading.Thread(target=unloader, daemon=True),
    ]
    for t in threads:
        t.start()
    time.sleep(1.5)
    stop.set()
    for t in threads:
        t.join(timeout=5.0)

    assert not failures, f"unload tore the model down under a running encode: {failures[0]!r}"
    assert encodes[0] > 0, "the encoder never completed a pass"

    emb.unload()


def test_upsert_reports_busy_instead_of_parking_a_handler_thread(monkeypatch):
    """Belt and braces for the invariant above.

    Once the clustering lock is narrowed nothing holds the manager lock long
    enough for this to fire. If something ever does, a mirror must report it and
    let Rust treat the batch as a best-effort miss, not sit in a pipe handler.
    """
    collection = FakeCollection()
    manager = _manager(monkeypatch, collection, BlockingEngine())
    monkeypatch.setattr(tc, "MANAGER_LOCK_BUSY_TIMEOUT_SECS", 0.1)

    holding = threading.Event()
    release = threading.Event()

    def hog():
        with manager._lock:
            holding.set()
            release.wait(timeout=5.0)

    hog_thread = threading.Thread(target=hog, daemon=True)
    hog_thread.start()
    assert holding.wait(timeout=2.0)

    started = time.monotonic()
    with pytest.raises(tc.ManagerBusyError):
        manager.upsert_task_vectors([_mirror_record()])
    elapsed = time.monotonic() - started

    assert elapsed < 2.0, "the mirror waited far past its timeout"
    assert collection.upsert_calls == 0

    release.set()
    hog_thread.join(timeout=5.0)


def test_unload_collections_drops_the_handles(monkeypatch):
    """It does not free the HNSW index — measured at 0.0 MB on chromadb 1.5.1 —
    but it must still leave the manager able to rebuild a handle on demand.
    """
    collection = FakeCollection()
    manager = _manager(monkeypatch, collection, BlockingEngine())

    assert manager.hot_collection is collection
    assert hasattr(manager, "_hot_collection")
    manager.unload_collections()
    assert not hasattr(manager, "_hot_collection")
    assert manager.hot_collection is collection


def test_scheduler_can_recover_after_failed_run(monkeypatch):
    monkeypatch.setattr(tc.TaskEmbedder, "is_model_available", staticmethod(lambda: True))

    class FlakyManager:
        def __init__(self):
            self.calls = 0

        def run_clustering(self, auto_compress=True, **_kwargs):
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


def test_scheduler_does_not_consume_its_interval_on_a_refused_run(monkeypatch):
    """A run refused by the clustering guard is not a completed run. Recording
    it as one would push the next scheduled attempt out by a whole interval.
    """
    monkeypatch.setattr(tc.TaskEmbedder, "is_model_available", staticmethod(lambda: True))

    class RefusingManager:
        def run_clustering(self, auto_compress=True, **_kwargs):
            return {"clusters": [], "noise_ids": [], "status": "already_running"}

    scheduler = tc.ClusteringScheduler(RefusingManager())
    saves = []
    scheduler._save_config = lambda: saves.append(True)
    before = scheduler._last_run

    assert scheduler._do_run() is False
    assert scheduler._last_run == before
    assert saves == []
    assert scheduler.get_config()["running"] is False
