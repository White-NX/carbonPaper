"""Benchmark Python ONNX BGE against the Rust semantic worker on local data.

The tool is offline. It selects rows deterministically, asks a read-only Rust
helper to decrypt them through the production CNG/AES-GCM implementation, and
keeps all plaintext in anonymous process pipes and memory. Reports contain only
row ids, aggregate lengths, category names, scores, and performance metrics.
"""

from __future__ import annotations

import argparse
import contextlib
import gc
import hashlib
import json
import os
import platform
import shutil
import statistics
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path
from typing import Any, Iterable, Mapping, Sequence

REPO_ROOT = Path(__file__).resolve().parents[1]
MONITOR_DIR = REPO_ROOT / "monitor"
TAURI_DIR = REPO_ROOT / "src-tauri"
DEFAULT_DATA_DIR = Path(r"D:\tools\carbonpaper\data")
DEFAULT_APPDATA_ROOT = (
    Path(os.environ.get("LOCALAPPDATA", Path.home() / "AppData" / "Local"))
    / "carbonpaper"
)
DEFAULT_SEED = "carbonpaper-bge-v1"
CPU_MIN_COSINE = 0.99999
CPU_MAX_ABS_ERROR = 1e-4
DIRECTML_MIN_COSINE = 0.999
DIRECTML_MAX_ABS_ERROR = 1e-3
MAX_WORKER_RESPONSE_BYTES = 64 * 1024 * 1024
SEMANTIC_WORKER_MAX_BATCH = 32
CLASSIFICATION_BRIDGE_MAX_BATCH = 512
CLASSIFICATION_BRIDGE_CHUNK_SIZE = 32


def _memory_snapshot(process: Any) -> dict[str, int]:
    try:
        info = process.memory_info()
    except Exception:
        return {}
    return {
        "private_bytes": int(getattr(info, "private", getattr(info, "vms", 0))),
        "working_set_bytes": int(getattr(info, "wset", getattr(info, "rss", 0))),
        "peak_working_set_bytes": int(
            getattr(info, "peak_wset", getattr(info, "rss", 0))
        ),
    }


class MemorySampler:
    def __init__(self, process: Any, interval: float = 0.02) -> None:
        self.process = process
        self.interval = interval
        self._stop = threading.Event()
        self._peak: dict[str, int] = {}
        self._thread = threading.Thread(
            target=self._sample,
            name="bge-memory-sampler",
            daemon=True,
        )

    def start(self) -> None:
        self._record()
        self._thread.start()

    def _record(self) -> None:
        for name, value in _memory_snapshot(self.process).items():
            self._peak[name] = max(self._peak.get(name, 0), value)

    def _sample(self) -> None:
        while not self._stop.wait(self.interval):
            self._record()

    def stop(self) -> dict[str, int]:
        self._stop.set()
        self._thread.join(timeout=2)
        self._record()
        return dict(self._peak)


def _percentile(values: Sequence[float], percentile: float) -> float:
    if not values:
        return 0.0
    ordered = sorted(float(value) for value in values)
    if len(ordered) == 1:
        return ordered[0]
    position = (len(ordered) - 1) * percentile / 100.0
    lower = int(position)
    upper = min(lower + 1, len(ordered) - 1)
    fraction = position - lower
    return ordered[lower] * (1.0 - fraction) + ordered[upper] * fraction


def summarize_values(values: Sequence[float]) -> dict[str, float | int]:
    finite = [float(value) for value in values]
    if not finite:
        return {"count": 0, "mean": 0.0, "p50": 0.0, "p95": 0.0, "p99": 0.0, "max": 0.0}
    return {
        "count": len(finite),
        "mean": statistics.fmean(finite),
        "p50": _percentile(finite, 50),
        "p95": _percentile(finite, 95),
        "p99": _percentile(finite, 99),
        "max": max(finite),
    }


def _memory_with_deltas(
    baseline: Mapping[str, int],
    after_load: Mapping[str, int],
    peak: Mapping[str, int],
) -> dict[str, Any]:
    keys = sorted(set(baseline) | set(after_load) | set(peak))
    return {
        "baseline": dict(baseline),
        "after_model_load": dict(after_load),
        "peak": dict(peak),
        "model_load_delta": {
            key: int(after_load.get(key, 0)) - int(baseline.get(key, 0)) for key in keys
        },
        "peak_delta": {
            key: int(peak.get(key, 0)) - int(baseline.get(key, 0)) for key in keys
        },
    }


def _configure_python_environment(
    model_root: Path,
    provider: str,
    dml_device_id: int,
) -> None:
    os.environ.update(
        {
            "CARBONPAPER_CLASSIFICATION_RUNTIME": "python",
            "CARBONPAPER_USE_ONNX": "1",
            "CARBONPAPER_USE_DML": "1" if provider == "directml" else "0",
            "CARBONPAPER_DML_DEVICE_ID": str(dml_device_id),
            "CARBONPAPER_ONNX_LOAD_MODE": "buffer",
            "BGE_MODEL_PATH": str(model_root / "bge-small-zh-v1.5"),
            "DO_NOT_TRACK": "1",
            "HF_HUB_DISABLE_TELEMETRY": "1",
            "HF_HUB_OFFLINE": "1",
            "TRANSFORMERS_OFFLINE": "1",
        }
    )


def _reset_text_embedder(classifier: Any) -> None:
    classifier.TextEmbedder._instance = None
    classifier.TextEmbedder._model = None
    classifier.TextEmbedder._tokenizer = None
    classifier.TextEmbedder._is_onnx = False
    classifier.TextEmbedder._initialized = False
    classifier.TextEmbedder._selected_runtime = None
    classifier.TextEmbedder._last_backend = None


def _repeat_to_size(texts: Sequence[str], size: int) -> list[str]:
    if not texts:
        raise ValueError("benchmark text corpus is empty")
    return [texts[index % len(texts)] for index in range(size)]


def _python_encode_timed(
    embedder: Any, texts: Sequence[str]
) -> tuple[Any, dict[str, float]]:
    import numpy as np

    from onnx_utils import build_transformer_inputs

    started = time.perf_counter()
    encoded = embedder._tokenizer(
        list(texts),
        padding=True,
        truncation=True,
        max_length=512,
        return_tensors="np",
    )
    preprocess_ms = (time.perf_counter() - started) * 1000.0
    inference_started = time.perf_counter()
    inputs = build_transformer_inputs(embedder._model, encoded)
    last_hidden_state = embedder._model.run(None, inputs)[0]
    vectors = np.asarray(last_hidden_state[:, 0, :], dtype=np.float32)
    norm = np.linalg.norm(vectors, axis=1, keepdims=True)
    vectors = vectors / np.clip(norm, a_min=1e-9, a_max=None)
    inference_ms = (time.perf_counter() - inference_started) * 1000.0
    return vectors, {
        "preprocess_ms": preprocess_ms,
        "inference_ms": inference_ms,
        "wall_ms": (time.perf_counter() - started) * 1000.0,
    }


def _benchmark_python_runner(payload: Mapping[str, Any]) -> dict[str, Any]:
    import psutil

    process = psutil.Process()
    process_start_baseline = _memory_snapshot(process)
    import_sampler = MemorySampler(process)
    import_sampler.start()
    import_started = time.perf_counter()
    if str(MONITOR_DIR) not in sys.path:
        sys.path.insert(0, str(MONITOR_DIR))
    _configure_python_environment(
        Path(str(payload["model_root"])),
        str(payload["provider"]),
        int(payload["dml_device_id"]),
    )
    import classifier
    import numpy as np
    import onnxruntime as ort

    module_import_ms = (time.perf_counter() - import_started) * 1000.0
    runtime_ready_baseline = _memory_snapshot(process)
    runtime_import_peak = import_sampler.stop()
    sampler = MemorySampler(process)
    sampler.start()
    _reset_text_embedder(classifier)
    embedder = classifier.TextEmbedder()
    load_started = time.perf_counter()
    embedder.initialize()
    model_load_ms = (time.perf_counter() - load_started) * 1000.0
    if not embedder._is_onnx:
        raise RuntimeError("Python benchmark unexpectedly loaded the PyTorch BGE model")
    after_load = _memory_snapshot(process)

    texts = [str(text) for text in payload["texts"]]
    first_texts = _repeat_to_size(texts, int(payload["first_batch_size"]))
    _, first = _python_encode_timed(embedder, first_texts)
    result: dict[str, Any] = {
        "module_import_ms": module_import_ms,
        "model_load_ms": model_load_ms,
        "cold_inference": first,
        "providers": list(embedder._model.get_providers()),
        "onnxruntime_version": ort.__version__,
        "numpy_version": np.__version__,
    }

    if payload.get("mode") == "full":
        chunks = []
        chunk_size = int(payload["correctness_chunk_size"])
        for offset in range(0, len(texts), chunk_size):
            vectors, _ = _python_encode_timed(
                embedder, texts[offset : offset + chunk_size]
            )
            chunks.append(vectors)
        all_vectors = np.concatenate(chunks, axis=0)
        warm_results = []
        for batch_size in payload["batch_sizes"]:
            batch = _repeat_to_size(texts, int(batch_size))
            for _ in range(int(payload["warmup"])):
                _python_encode_timed(embedder, batch)
            iterations = [
                _python_encode_timed(embedder, batch)[1]
                for _ in range(int(payload["iterations"]))
            ]
            warm_results.append(
                {
                    "batch_size": int(batch_size),
                    "wall_ms": summarize_values(
                        [item["wall_ms"] for item in iterations]
                    ),
                    "preprocess_ms": summarize_values(
                        [item["preprocess_ms"] for item in iterations]
                    ),
                    "inference_ms": summarize_values(
                        [item["inference_ms"] for item in iterations]
                    ),
                }
            )
        result["warm_batches"] = warm_results
        result["vectors"] = all_vectors.tolist()

    peak = sampler.stop()
    result["memory"] = _memory_with_deltas(runtime_ready_baseline, after_load, peak)
    result["memory"]["process_start_baseline"] = process_start_baseline
    result["memory"]["runtime_import_delta"] = {
        key: int(runtime_ready_baseline.get(key, 0))
        - int(process_start_baseline.get(key, 0))
        for key in sorted(set(process_start_baseline) | set(runtime_ready_baseline))
    }
    result["memory"]["runtime_import_peak"] = runtime_import_peak
    return result


def _run_internal_python_runner() -> int:
    try:
        payload = json.load(sys.stdin)
        result = _benchmark_python_runner(payload)
        json.dump(result, sys.stdout, ensure_ascii=True, separators=(",", ":"))
        sys.stdout.write("\n")
        sys.stdout.flush()
        return 0
    except Exception as error:  # noqa: BLE001 - isolated runner reports to parent
        print(f"Python BGE runner failed: {error}", file=sys.stderr)
        return 2


def _run_python_process(payload: Mapping[str, Any], timeout: float) -> dict[str, Any]:
    completed = subprocess.run(
        [sys.executable, str(Path(__file__).resolve()), "--_python-runner"],
        input=json.dumps(payload, ensure_ascii=False),
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        cwd=REPO_ROOT,
        timeout=timeout,
        check=False,
    )
    if completed.returncode != 0:
        diagnostics = "\n".join(completed.stderr.splitlines()[-30:])
        raise RuntimeError(
            f"Python BGE runner exited with {completed.returncode}:\n{diagnostics}"
        )
    try:
        return json.loads(completed.stdout)
    except json.JSONDecodeError as error:
        raise RuntimeError("Python BGE runner returned invalid JSON") from error


def _resolve_exporter(explicit: Path | None, build: bool) -> Path:
    if explicit:
        path = explicit.resolve()
    else:
        path = TAURI_DIR / "target" / "debug" / "examples" / "bge_benchmark_export.exe"
    if build:
        subprocess.run(
            [
                "cargo",
                "build",
                "--quiet",
                "--manifest-path",
                str(TAURI_DIR / "Cargo.toml"),
                "--example",
                "bge_benchmark_export",
            ],
            cwd=REPO_ROOT,
            check=True,
        )
    if not path.is_file():
        raise FileNotFoundError(f"BGE benchmark exporter was not found: {path}")
    return path


def _extract_samples(args: argparse.Namespace) -> dict[str, Any]:
    exporter = _resolve_exporter(args.exporter, not args.no_build_exporter)
    command = [
        str(exporter),
        "--data-dir",
        str(args.data_dir.resolve()),
        "--sample-size",
        str(args.sample_size),
        "--seed",
        args.seed,
        "--max-ocr-chars",
        str(args.max_ocr_chars),
    ]
    sample_ids = args.sample_ids
    if args.reuse_report:
        report = json.loads(args.reuse_report.read_text(encoding="utf-8"))
        sample_ids = ",".join(str(value) for value in report["dataset"]["sample_ids"])
    if sample_ids:
        command.extend(("--sample-ids", sample_ids))
    completed = subprocess.run(
        command,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        cwd=REPO_ROOT,
        timeout=args.timeout,
        check=False,
    )
    if completed.returncode != 0:
        diagnostics = completed.stderr.decode("utf-8", errors="replace")
        raise RuntimeError(
            f"BGE data exporter exited with {completed.returncode}:\n"
            + "\n".join(diagnostics.splitlines()[-30:])
        )
    try:
        return json.loads(completed.stdout)
    except json.JSONDecodeError as error:
        # Never include stdout here: it may contain a partially emitted plaintext payload.
        raise RuntimeError("BGE data exporter returned invalid JSON") from error


class BenchmarkSemanticWorker:
    def __init__(self, *args: Any, **kwargs: Any) -> None:
        from validate_rust_semantic import SemanticWorker

        self._worker = SemanticWorker(*args, **kwargs)
        self.process = self._worker.process

    def read_response(self) -> Mapping[str, Any]:
        from validate_rust_semantic import _read_exact

        stream = self.process.stdout
        assert stream is not None
        try:
            length = int.from_bytes(_read_exact(stream, 4), "little")
            if length <= 0 or length > MAX_WORKER_RESPONSE_BYTES:
                raise ValueError(f"invalid semantic response frame length: {length}")
            response = json.loads(_read_exact(stream, length))
        except Exception as error:
            diagnostics = self._worker._diagnostics()
            raise RuntimeError(
                f"failed to read semantic response: {error}{diagnostics}"
            ) from error
        if not isinstance(response, Mapping):
            raise RuntimeError("semantic response is not an object")
        return response

    def request(
        self, command: str, *, body: bytes = b"", **fields: Any
    ) -> Mapping[str, Any]:
        if self.process.poll() is not None:
            raise RuntimeError("semantic worker exited before the request")
        self._worker._request_id += 1
        request_id = self._worker._request_id
        request = {"command": command, "request_id": request_id, **fields}
        payload = json.dumps(request, ensure_ascii=False, separators=(",", ":")).encode(
            "utf-8"
        )
        stream = self.process.stdin
        assert stream is not None
        stream.write(len(payload).to_bytes(4, "little"))
        stream.write(payload)
        if body:
            stream.write(body)
        stream.flush()
        response = self.read_response()
        if response.get("status") == "error":
            raise RuntimeError(
                f"semantic request {command} failed: {response.get('kind')}: "
                f"{response.get('message')}"
            )
        if response.get("request_id") != request_id:
            raise RuntimeError("semantic response request id mismatch")
        return response

    def close(self) -> None:
        if self.process.poll() is None:
            try:
                response = self.request("shutdown")
                if response.get("status") != "shutting_down":
                    raise RuntimeError("unexpected semantic shutdown response")
                self.process.wait(timeout=10)
            except Exception:
                self.process.kill()
                self.process.wait(timeout=10)
                raise

    def kill(self) -> None:
        self._worker.kill()


@contextlib.contextmanager
def _temporary_environment(values: Mapping[str, str | None]) -> Iterable[None]:
    previous = {name: os.environ.get(name) for name in values}
    try:
        for name, value in values.items():
            if value is None:
                os.environ.pop(name, None)
            else:
                os.environ[name] = value
        yield
    finally:
        for name, value in previous.items():
            if value is None:
                os.environ.pop(name, None)
            else:
                os.environ[name] = value


def _resolve_worker_paths(args: argparse.Namespace) -> tuple[Path, Path, Path, Path]:
    from validate_rust_semantic import _default_ort_dylib, _default_worker

    worker = args.worker.resolve() if args.worker else _default_worker()
    ort_dylib = args.ort_dylib.resolve() if args.ort_dylib else _default_ort_dylib()
    models_root = args.models_root.resolve()
    onnx_models_root = args.onnx_models_root.resolve()
    if not onnx_models_root.is_dir():
        onnx_models_root = models_root
    return worker, ort_dylib, models_root, onnx_models_root


def _start_worker(
    args: argparse.Namespace,
) -> tuple[BenchmarkSemanticWorker, Mapping[str, Any], float]:
    worker_path, ort_dylib, models_root, onnx_models_root = _resolve_worker_paths(args)
    started = time.perf_counter()
    worker = BenchmarkSemanticWorker(
        worker_path,
        ort_dylib,
        models_root,
        onnx_models_root,
        args.provider,
        args.dml_device_id,
    )
    ready = worker.read_response()
    startup_ms = (time.perf_counter() - started) * 1000.0
    expected_provider = "direct_ml" if args.provider == "directml" else "cpu"
    if (
        ready.get("status") != "semantic_ready"
        or ready.get("provider") != expected_provider
    ):
        worker.kill()
        raise RuntimeError(f"unexpected semantic worker handshake: {ready!r}")
    return worker, ready, startup_ms


def _rust_embed_timed(
    worker: BenchmarkSemanticWorker,
    texts: Sequence[str],
) -> tuple[Any, dict[str, float]]:
    import numpy as np

    if not 1 <= len(texts) <= SEMANTIC_WORKER_MAX_BATCH:
        raise ValueError(
            f"Rust semantic worker batches must contain 1..={SEMANTIC_WORKER_MAX_BATCH} texts"
        )
    started = time.perf_counter()
    response = worker.request(
        "embed_text",
        model="bge_small_zh",
        texts=list(texts),
        timeout_ms=10 * 60 * 1000,
    )
    wall_ms = (time.perf_counter() - started) * 1000.0
    vectors = np.asarray(response["vectors"], dtype=np.float32)
    timings = dict(response.get("timings") or {})
    timings["wall_ms"] = wall_ms
    return vectors, {name: float(value) for name, value in timings.items()}


def _benchmark_rust_once(
    args: argparse.Namespace,
    texts: Sequence[str],
    mode: str,
) -> tuple[dict[str, Any], Any | None]:
    import numpy as np
    import psutil

    env_value = None if args.rust_intra_threads == 0 else str(args.rust_intra_threads)
    with _temporary_environment({"CARBONPAPER_ONNX_INTRA_THREADS": env_value}):
        worker, ready, startup_ms = _start_worker(args)
        process = psutil.Process(worker.process.pid)
        baseline = _memory_snapshot(process)
        sampler = MemorySampler(process)
        sampler.start()
        completed = False
        try:
            first_texts = _repeat_to_size(texts, args.batch_sizes[0])
            _, first = _rust_embed_timed(worker, first_texts)
            after_load = _memory_snapshot(process)
            result: dict[str, Any] = {
                "worker_startup_ms": startup_ms,
                "cold_inference": first,
                "worker_version": ready.get("worker_version"),
                "onnxruntime_version": ready.get("ort_version"),
                "provider": ready.get("provider"),
            }
            vectors = None
            if mode == "full":
                chunks = []
                for offset in range(0, len(texts), args.correctness_chunk_size):
                    chunk, _ = _rust_embed_timed(
                        worker, texts[offset : offset + args.correctness_chunk_size]
                    )
                    chunks.append(chunk)
                vectors = np.concatenate(chunks, axis=0)
                warm_results = []
                for batch_size in args.batch_sizes:
                    batch = _repeat_to_size(texts, batch_size)
                    for _ in range(args.warmup):
                        _rust_embed_timed(worker, batch)
                    iterations = [
                        _rust_embed_timed(worker, batch)[1]
                        for _ in range(args.iterations)
                    ]
                    names = sorted({name for item in iterations for name in item})
                    warm_results.append(
                        {
                            "batch_size": batch_size,
                            **{
                                name: summarize_values(
                                    [item.get(name, 0.0) for item in iterations]
                                )
                                for name in names
                            },
                        }
                    )
                result["warm_batches"] = warm_results
            peak = sampler.stop()
            result["memory"] = _memory_with_deltas(baseline, after_load, peak)
            completed = True
            return result, vectors
        finally:
            if not completed:
                with contextlib.suppress(Exception):
                    sampler.stop()
            if worker.process.poll() is None:
                if completed:
                    worker.close()
                else:
                    worker.kill()


def _cold_summary(runs: Sequence[Mapping[str, Any]], backend: str) -> dict[str, Any]:
    if backend == "python":
        load_values = [float(run["model_load_ms"]) for run in runs]
        startup_values = [float(run["module_import_ms"]) for run in runs]
    else:
        load_values = [
            float(run["cold_inference"].get("model_load_ms", 0.0)) for run in runs
        ]
        startup_values = [float(run["worker_startup_ms"]) for run in runs]
    inference_values = [float(run["cold_inference"]["wall_ms"]) for run in runs]
    return {
        "runs": len(runs),
        "startup_or_import_ms": summarize_values(startup_values),
        "model_load_ms": summarize_values(load_values),
        "first_inference_wall_ms": summarize_values(inference_values),
        "load_plus_first_inference_ms": summarize_values(
            [load + inference for load, inference in zip(load_values, inference_values)]
        ),
    }


def _run_backend_benchmarks(
    args: argparse.Namespace,
    texts: Sequence[str],
) -> tuple[dict[str, Any], Any, Any]:
    python_payload = {
        "mode": "full",
        "model_root": str(args.models_root.resolve()),
        "provider": args.provider,
        "dml_device_id": args.dml_device_id,
        "texts": list(texts),
        "first_batch_size": args.batch_sizes[0],
        "batch_sizes": args.batch_sizes,
        "warmup": args.warmup,
        "iterations": args.iterations,
        "correctness_chunk_size": args.correctness_chunk_size,
    }
    python_runs = [_run_python_process(python_payload, args.timeout)]
    python_vectors = python_runs[0].pop("vectors")
    for _ in range(args.cold_runs - 1):
        cold_payload = dict(python_payload)
        cold_payload["mode"] = "cold"
        python_runs.append(_run_python_process(cold_payload, args.timeout))

    rust_full, rust_vectors = _benchmark_rust_once(args, texts, "full")
    rust_runs = [rust_full]
    for _ in range(args.cold_runs - 1):
        cold, _ = _benchmark_rust_once(args, texts, "cold")
        rust_runs.append(cold)

    python_report = dict(python_runs[0])
    rust_report = dict(rust_runs[0])
    python_report["cold"] = _cold_summary(python_runs, "python")
    rust_report["cold"] = _cold_summary(rust_runs, "rust")
    return {"python": python_report, "rust": rust_report}, python_vectors, rust_vectors


def compare_embeddings(
    python_vectors: Any,
    rust_vectors: Any,
    provider: str,
) -> dict[str, Any]:
    import numpy as np

    left = np.asarray(python_vectors, dtype=np.float32)
    right = np.asarray(rust_vectors, dtype=np.float32)
    if left.shape != right.shape or left.ndim != 2:
        raise ValueError(
            f"embedding shape mismatch: Python {left.shape}, Rust {right.shape}"
        )
    numerator = np.sum(left * right, axis=1)
    denominator = np.linalg.norm(left, axis=1) * np.linalg.norm(right, axis=1)
    cosines = numerator / np.clip(denominator, a_min=1e-12, a_max=None)
    absolute = np.abs(left - right)
    pairwise_delta = np.abs((left @ left.T) - (right @ right.T))
    min_cosine = float(np.min(cosines))
    max_abs = float(np.max(absolute))
    min_gate = DIRECTML_MIN_COSINE if provider == "directml" else CPU_MIN_COSINE
    max_gate = DIRECTML_MAX_ABS_ERROR if provider == "directml" else CPU_MAX_ABS_ERROR
    return {
        "shape": list(left.shape),
        "min_cosine": min_cosine,
        "mean_cosine": float(np.mean(cosines)),
        "max_abs_error": max_abs,
        "mean_abs_error": float(np.mean(absolute)),
        "max_pairwise_cosine_delta": float(np.max(pairwise_delta)),
        "max_l2_norm_error_python": float(
            np.max(np.abs(np.linalg.norm(left, axis=1) - 1.0))
        ),
        "max_l2_norm_error_rust": float(
            np.max(np.abs(np.linalg.norm(right, axis=1) - 1.0))
        ),
        "gate": {
            "min_cosine": min_gate,
            "max_abs_error": max_gate,
            "passed": min_cosine >= min_gate and max_abs <= max_gate,
        },
    }


def _build_embedding_texts(samples: Sequence[Mapping[str, Any]]) -> list[str]:
    texts = []
    seen = set()
    for sample in samples:
        title = str(sample.get("window_title") or "").strip()
        ocr = str(sample.get("ocr_text") or "").strip()
        for value in (title, ocr[:200]):
            if value and value not in seen:
                seen.add(value)
                texts.append(value)
    return texts


class RustEmbedderAdapter:
    def __init__(self, worker: BenchmarkSemanticWorker) -> None:
        self.worker = worker

    def encode(self, texts: Sequence[str]) -> Any:
        import numpy as np

        chunks = [
            _rust_embed_timed(
                self.worker,
                texts[offset : offset + CLASSIFICATION_BRIDGE_CHUNK_SIZE],
            )[0]
            for offset in range(
                0, len(texts), CLASSIFICATION_BRIDGE_CHUNK_SIZE
            )
        ]
        if not chunks:
            return np.zeros((0, 512), dtype=np.float32)
        return np.concatenate(chunks, axis=0)

    def encode_single(self, text: str) -> Any:
        return self.encode([text])[0]


def _run_classifier_service(
    classifier: Any,
    anchors_path: Path,
    embedder: Any,
    samples: Sequence[Mapping[str, Any]],
) -> dict[str, Any]:
    service = classifier.ClassificationService(str(anchors_path))
    service.embedder = embedder
    started = time.perf_counter()
    service._ensure_index()
    anchor_index_ms = (time.perf_counter() - started) * 1000.0
    results = []
    latencies = []
    for sample in samples:
        title = str(sample.get("window_title") or "")
        ocr_text = str(sample.get("ocr_text") or "")
        process_name = str(sample.get("process_name") or "")
        classify_started = time.perf_counter()
        category, confidence = service.classify(title, ocr_text, process_name)
        latencies.append((time.perf_counter() - classify_started) * 1000.0)
        debug = service.classify_debug(title, ocr_text, process_name)
        results.append(
            {
                "screenshot_id": int(sample["screenshot_id"]),
                "category": category,
                "confidence": float(confidence),
                "confidence_rounded": round(float(confidence), 4),
                "debug_category": debug.get("category"),
                "used_ocr": bool(debug.get("used_ocr", False)),
                "local_veto_active": bool(debug.get("local_veto_active", False)),
                "top_order": [
                    item.get("category") for item in debug.get("top_scores", [])
                ],
            }
        )
    return {
        "anchor_index_ms": anchor_index_ms,
        "anchor_count": int(len(service.anchor_matrix)),
        "latency_ms": summarize_values(latencies),
        "rows": results,
    }


def compare_classifications(
    python_result: Mapping[str, Any],
    rust_result: Mapping[str, Any],
) -> dict[str, Any]:
    python_rows = {int(row["screenshot_id"]): row for row in python_result["rows"]}
    rust_rows = {int(row["screenshot_id"]): row for row in rust_result["rows"]}
    if python_rows.keys() != rust_rows.keys():
        raise ValueError("Python and Rust classification sample ids differ")
    rows = []
    confidence_deltas = []
    category_matches = 0
    branch_matches = 0
    top_order_matches = 0
    threshold_crossings = {"0.38": 0, "0.50": 0, "0.55": 0}
    for screenshot_id in python_rows:
        left = python_rows[screenshot_id]
        right = rust_rows[screenshot_id]
        delta = abs(float(left["confidence"]) - float(right["confidence"]))
        confidence_deltas.append(delta)
        category_match = left["category"] == right["category"]
        branch_match = (
            left["used_ocr"] == right["used_ocr"]
            and left["local_veto_active"] == right["local_veto_active"]
        )
        top_order_match = left["top_order"] == right["top_order"]
        category_matches += int(category_match)
        branch_matches += int(branch_match)
        top_order_matches += int(top_order_match)
        crossed = []
        for threshold in (0.38, 0.50, 0.55):
            if (float(left["confidence"]) >= threshold) != (
                float(right["confidence"]) >= threshold
            ):
                key = f"{threshold:.2f}"
                threshold_crossings[key] += 1
                crossed.append(key)
        rows.append(
            {
                "screenshot_id": screenshot_id,
                "python_category": left["category"],
                "rust_category": right["category"],
                "python_confidence": left["confidence_rounded"],
                "rust_confidence": right["confidence_rounded"],
                "confidence_abs_delta": delta,
                "category_match": category_match,
                "branch_match": branch_match,
                "top_order_match": top_order_match,
                "threshold_crossings": crossed,
            }
        )
    count = len(rows)
    summary = {
        "sample_count": count,
        "category_agreement": category_matches / count if count else 0.0,
        "branch_agreement": branch_matches / count if count else 0.0,
        "top_order_agreement": top_order_matches / count if count else 0.0,
        "confidence_abs_delta": summarize_values(confidence_deltas),
        "threshold_crossings": threshold_crossings,
        "passed": bool(count)
        and category_matches == count
        and branch_matches == count
        and not any(threshold_crossings.values()),
    }
    return {
        "summary": summary,
        "python_timing": {
            "anchor_index_ms": python_result["anchor_index_ms"],
            "anchor_count": python_result["anchor_count"],
            "latency_ms": python_result["latency_ms"],
        },
        "rust_timing": {
            "anchor_index_ms": rust_result["anchor_index_ms"],
            "anchor_count": rust_result["anchor_count"],
            "latency_ms": rust_result["latency_ms"],
        },
        "rows": rows,
    }


def _classification_operational_contract(anchor_count: int) -> dict[str, Any]:
    if anchor_count < 0:
        raise ValueError("anchor count cannot be negative")

    bridge_request_compatible = 1 <= anchor_count <= CLASSIFICATION_BRIDGE_MAX_BATCH
    worker_chunk_compatible = (
        1 <= CLASSIFICATION_BRIDGE_CHUNK_SIZE <= SEMANTIC_WORKER_MAX_BATCH
    )
    if CLASSIFICATION_BRIDGE_CHUNK_SIZE > 0:
        required_worker_request_count = (
            anchor_count + CLASSIFICATION_BRIDGE_CHUNK_SIZE - 1
        ) // CLASSIFICATION_BRIDGE_CHUNK_SIZE
        max_worker_request_batch = min(
            anchor_count, CLASSIFICATION_BRIDGE_CHUNK_SIZE
        )
    else:
        required_worker_request_count = 0
        max_worker_request_batch = 0
    return {
        "classification_bridge_max_batch": CLASSIFICATION_BRIDGE_MAX_BATCH,
        "classification_bridge_chunk_size": CLASSIFICATION_BRIDGE_CHUNK_SIZE,
        "semantic_worker_max_batch": SEMANTIC_WORKER_MAX_BATCH,
        "anchor_count": anchor_count,
        "required_worker_request_count": required_worker_request_count,
        "max_worker_request_batch": max_worker_request_batch,
        "production_bridge_request_compatible": bridge_request_compatible,
        "production_worker_chunk_compatible": worker_chunk_compatible,
        "production_batch_contract_compatible": (
            bridge_request_compatible and worker_chunk_compatible
        ),
    }


def _run_classification_comparison(
    args: argparse.Namespace,
    samples: Sequence[Mapping[str, Any]],
) -> dict[str, Any]:
    if str(MONITOR_DIR) not in sys.path:
        sys.path.insert(0, str(MONITOR_DIR))
    _configure_python_environment(
        args.models_root.resolve(), args.provider, args.dml_device_id
    )
    import classifier

    _reset_text_embedder(classifier)
    python_embedder = classifier.TextEmbedder()
    python_embedder.initialize()
    env_value = None if args.rust_intra_threads == 0 else str(args.rust_intra_threads)
    with tempfile.TemporaryDirectory(prefix="carbonpaper-bge-benchmark-") as temp_dir:
        temp_root = Path(temp_dir)
        python_anchors = temp_root / "python" / "anchors.json"
        rust_anchors = temp_root / "rust" / "anchors.json"
        python_anchors.parent.mkdir()
        rust_anchors.parent.mkdir()
        shutil.copy2(args.data_dir / "anchors.json", python_anchors)
        shutil.copy2(args.data_dir / "anchors.json", rust_anchors)
        python_result = _run_classifier_service(
            classifier, python_anchors, python_embedder, samples
        )
        with _temporary_environment({"CARBONPAPER_ONNX_INTRA_THREADS": env_value}):
            worker, _, _ = _start_worker(args)
            completed = False
            try:
                rust_result = _run_classifier_service(
                    classifier,
                    rust_anchors,
                    RustEmbedderAdapter(worker),
                    samples,
                )
                completed = True
            finally:
                if worker.process.poll() is None:
                    if completed:
                        worker.close()
                    else:
                        worker.kill()
    result = compare_classifications(python_result, rust_result)
    anchor_count = int(rust_result["anchor_count"])
    result["operational_contract"] = _classification_operational_contract(
        anchor_count
    )
    del python_result, rust_result, python_embedder
    _reset_text_embedder(classifier)
    gc.collect()
    return result


def _length_summary(values: Sequence[str]) -> dict[str, float | int]:
    return summarize_values([float(len(value)) for value in values])


def _model_fingerprint(model_root: Path) -> dict[str, Any]:
    candidates = (
        model_root / "bge-small-zh-v1.5" / "model_int8.onnx",
        model_root / "bge-small-zh-v1.5" / "onnx" / "model_quantized.onnx",
    )
    model_path = next((path for path in candidates if path.is_file()), None)
    if model_path is None:
        raise FileNotFoundError("BGE ONNX model was not found under the model root")
    digest = hashlib.sha256()
    with model_path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return {
        "relative_path": model_path.relative_to(model_root).as_posix(),
        "size_bytes": model_path.stat().st_size,
        "sha256": digest.hexdigest(),
    }


def _performance_comparison(performance: Mapping[str, Any]) -> list[dict[str, Any]]:
    python_batches = {
        int(item["batch_size"]): item for item in performance["python"]["warm_batches"]
    }
    rust_batches = {
        int(item["batch_size"]): item for item in performance["rust"]["warm_batches"]
    }
    rows = []
    for batch_size in sorted(python_batches.keys() & rust_batches.keys()):
        python_p50 = float(python_batches[batch_size]["wall_ms"]["p50"])
        rust_p50 = float(rust_batches[batch_size]["wall_ms"]["p50"])
        rows.append(
            {
                "batch_size": batch_size,
                "python_wall_p50_ms": python_p50,
                "rust_wall_p50_ms": rust_p50,
                "rust_speedup": python_p50 / rust_p50 if rust_p50 else None,
            }
        )
    return rows


def _format_bytes(value: float | int) -> str:
    size = float(value)
    for unit in ("B", "KiB", "MiB", "GiB"):
        if abs(size) < 1024.0 or unit == "GiB":
            return f"{size:.2f} {unit}"
        size /= 1024.0
    return f"{size:.2f} GiB"


def _markdown_report(report: Mapping[str, Any]) -> str:
    numeric = report["numeric_parity"]
    classification = report.get("classification")
    lines = [
        "# BGE Python/Rust benchmark",
        "",
        f"- Corpus SHA-256: `{report['dataset']['corpus_sha256']}`",
        f"- Samples: {report['dataset']['sample_count']}",
        f"- Embedding inputs: {report['dataset']['embedding_input_count']}",
        f"- Provider: `{report['environment']['provider']}`",
        f"- Numeric gate: {'PASS' if numeric['gate']['passed'] else 'FAIL'}",
        "",
        "## Numeric parity",
        "",
        "| Metric | Value |",
        "| --- | ---: |",
        f"| Minimum cosine | {numeric['min_cosine']:.9f} |",
        f"| Maximum absolute error | {numeric['max_abs_error']:.9g} |",
        f"| Maximum pairwise cosine delta | {numeric['max_pairwise_cosine_delta']:.9g} |",
        "",
        "## Warm inference",
        "",
        "| Batch | Python p50 | Rust p50 | Rust speedup |",
        "| ---: | ---: | ---: | ---: |",
    ]
    for row in report["performance_comparison"]:
        speedup = row["rust_speedup"]
        lines.append(
            f"| {row['batch_size']} | {row['python_wall_p50_ms']:.3f} ms | "
            f"{row['rust_wall_p50_ms']:.3f} ms | "
            f"{speedup:.3f}x |"
        )
    lines.extend(
        [
            "",
            "## Memory",
            "",
            "| Backend | Model-load private delta | Peak private delta |",
            "| --- | ---: | ---: |",
        ]
    )
    for backend in ("python", "rust"):
        memory = report["performance"][backend]["memory"]
        lines.append(
            f"| {backend.title()} | "
            f"{_format_bytes(memory['model_load_delta']['private_bytes'])} | "
            f"{_format_bytes(memory['peak_delta']['private_bytes'])} |"
        )
    if classification:
        summary = classification["summary"]
        operational = classification["operational_contract"]
        lines.extend(
            [
                "",
                "## Classification",
                "",
                "| Metric | Value |",
                "| --- | ---: |",
                f"| Category agreement | {summary['category_agreement']:.4%} |",
                f"| OCR/veto branch agreement | {summary['branch_agreement']:.4%} |",
                f"| Top-order agreement | {summary['top_order_agreement']:.4%} |",
                f"| Confidence delta p95 | {summary['confidence_abs_delta']['p95']:.9g} |",
                f"| Confidence delta max | {summary['confidence_abs_delta']['max']:.9g} |",
                f"| Threshold crossings | {sum(summary['threshold_crossings'].values())} |",
                f"| Classification gate | {'PASS' if summary['passed'] else 'FAIL'} |",
                f"| Production batch contract | {'PASS' if operational['production_batch_contract_compatible'] else 'FAIL'} |",
                "",
                f"The production classification bridge accepts up to {operational['classification_bridge_max_batch']} texts "
                f"and submits chunks of at most {operational['classification_bridge_chunk_size']} to a semantic worker "
                f"whose request limit is {operational['semantic_worker_max_batch']}. This run's anchor index contains "
                f"{operational['anchor_count']} texts and requires {operational['required_worker_request_count']} worker "
                f"requests; the largest contains {operational['max_worker_request_batch']} texts.",
            ]
        )
    lines.extend(
        [
            "",
            "Plaintext titles, OCR text, process names, embeddings, and keys are not stored in this report.",
            "",
        ]
    )
    return "\n".join(lines)


def assert_report_has_no_plaintext_payloads(report: Mapping[str, Any]) -> None:
    forbidden_keys = {
        "window_title",
        "ocr_text",
        "process_name",
        "vectors",
        "embeddings",
    }

    def visit(value: Any, location: str) -> None:
        if isinstance(value, Mapping):
            for key, child in value.items():
                if key in forbidden_keys:
                    raise ValueError(
                        f"plaintext or vector field reached report at {location}.{key}"
                    )
                visit(child, f"{location}.{key}")
        elif isinstance(value, list):
            for index, child in enumerate(value):
                visit(child, f"{location}[{index}]")

    visit(report, "report")


def _parse_batch_sizes(raw: str) -> list[int]:
    try:
        values = [int(value.strip()) for value in raw.split(",") if value.strip()]
    except ValueError as error:
        raise argparse.ArgumentTypeError(
            "batch sizes must be comma-separated integers"
        ) from error
    if not values or any(
        value <= 0 or value > SEMANTIC_WORKER_MAX_BATCH for value in values
    ):
        raise argparse.ArgumentTypeError(
            f"batch sizes must be between 1 and {SEMANTIC_WORKER_MAX_BATCH}"
        )
    return list(dict.fromkeys(values))


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--data-dir", type=Path, default=DEFAULT_DATA_DIR)
    parser.add_argument("--sample-size", type=int, default=100)
    parser.add_argument("--seed", default=DEFAULT_SEED)
    parser.add_argument("--sample-ids", help="Comma-separated screenshot ids to replay")
    parser.add_argument(
        "--reuse-report", type=Path, help="Replay sample ids from a prior JSON report"
    )
    parser.add_argument("--max-ocr-chars", type=int, default=4096)
    parser.add_argument(
        "--models-root", type=Path, default=DEFAULT_APPDATA_ROOT / "models"
    )
    parser.add_argument(
        "--onnx-models-root", type=Path, default=DEFAULT_APPDATA_ROOT / "models-onnx"
    )
    parser.add_argument("--provider", choices=("cpu", "directml"), default="cpu")
    parser.add_argument("--dml-device-id", type=int, default=0)
    parser.add_argument(
        "--rust-intra-threads",
        type=int,
        default=1,
        help="Use 1 for a fair comparison with Python; 0 uses the Rust production default",
    )
    parser.add_argument(
        "--batch-sizes", type=_parse_batch_sizes, default=[1, 8, 16, 32]
    )
    parser.add_argument("--warmup", type=int, default=3)
    parser.add_argument("--iterations", type=int, default=10)
    parser.add_argument("--cold-runs", type=int, default=3)
    parser.add_argument("--correctness-chunk-size", type=int, default=32)
    parser.add_argument("--worker", type=Path)
    parser.add_argument("--ort-dylib", type=Path)
    parser.add_argument("--exporter", type=Path)
    parser.add_argument("--no-build-exporter", action="store_true")
    parser.add_argument("--skip-classification", action="store_true")
    parser.add_argument("--extract-only", action="store_true")
    parser.add_argument("--output-dir", type=Path)
    parser.add_argument("--timeout", type=float, default=10 * 60)
    args = parser.parse_args()
    if args.sample_size <= 0:
        parser.error("--sample-size must be positive")
    if args.max_ocr_chars < 200:
        parser.error("--max-ocr-chars must be at least 200")
    if args.dml_device_id < 0 or args.rust_intra_threads < 0:
        parser.error("device id and thread count cannot be negative")
    if args.warmup < 0 or args.iterations <= 0 or args.cold_runs <= 0:
        parser.error(
            "warmup must be non-negative; iterations and cold-runs must be positive"
        )
    if not 1 <= args.correctness_chunk_size <= SEMANTIC_WORKER_MAX_BATCH:
        parser.error(
            f"--correctness-chunk-size must be between 1 and {SEMANTIC_WORKER_MAX_BATCH}"
        )
    if args.sample_ids and args.reuse_report:
        parser.error("use only one of --sample-ids and --reuse-report")
    return args


def _clear_plaintext(samples: list[dict[str, Any]], texts: list[str]) -> None:
    for sample in samples:
        for key in ("window_title", "process_name", "ocr_text"):
            sample[key] = ""
    samples.clear()
    for index in range(len(texts)):
        texts[index] = ""
    texts.clear()
    gc.collect()


def run(args: argparse.Namespace) -> tuple[Path, Path] | None:
    if not args.data_dir.is_dir():
        raise FileNotFoundError(f"data directory does not exist: {args.data_dir}")
    print("Selecting and decrypting the reproducible local sample in memory...")
    export = _extract_samples(args)
    samples = list(export["samples"])
    texts = _build_embedding_texts(samples)
    try:
        dataset = {
            "seed": export["selection"]["seed"],
            "corpus_sha256": export["corpus_sha256"],
            "sample_count": len(samples),
            "sample_ids": list(export["selection"]["selected_ids"]),
            "eligible_rows": export["selection"]["eligible_rows"],
            "skipped_rows": export["selection"]["skipped_rows"],
            "database_size_bytes": export["database"]["size_bytes"],
            "database_modified_unix_ns": export["database"]["modified_unix_ns"],
            "embedding_input_count": len(texts),
            "title_length": _length_summary(
                [str(sample.get("window_title") or "") for sample in samples]
            ),
            "ocr_length": _length_summary(
                [str(sample.get("ocr_text") or "") for sample in samples]
            ),
            "embedding_input_length": _length_summary(texts),
        }
        if args.extract_only:
            print(
                f"Extracted {len(samples)} samples; corpus SHA-256 "
                f"{export['corpus_sha256']}. No plaintext was written to disk."
            )
            return None
        if not texts:
            raise RuntimeError("decrypted sample did not produce any BGE input text")

        print("Benchmarking isolated Python ONNX and Rust worker runtimes...")
        performance, python_vectors, rust_vectors = _run_backend_benchmarks(args, texts)
        numeric = compare_embeddings(python_vectors, rust_vectors, args.provider)
        del python_vectors, rust_vectors

        classification = None
        if not args.skip_classification:
            print(
                "Comparing production classifier categories, confidence, and branches..."
            )
            classification = _run_classification_comparison(args, samples)

        report: dict[str, Any] = {
            "schema_version": 1,
            "generated_at_unix_ms": int(time.time() * 1000),
            "environment": {
                "platform": platform.platform(),
                "python": platform.python_version(),
                "logical_cpu_count": os.cpu_count(),
                "provider": args.provider,
                "dml_device_id": args.dml_device_id,
                "rust_intra_threads": args.rust_intra_threads,
                "model": _model_fingerprint(args.models_root.resolve()),
            },
            "configuration": {
                "batch_sizes": args.batch_sizes,
                "warmup": args.warmup,
                "iterations": args.iterations,
                "cold_runs": args.cold_runs,
                "correctness_chunk_size": args.correctness_chunk_size,
                "max_ocr_chars": args.max_ocr_chars,
            },
            "dataset": dataset,
            "numeric_parity": numeric,
            "performance": performance,
            "performance_comparison": _performance_comparison(performance),
            "classification": classification,
            "release_gate_passed": bool(numeric["gate"]["passed"])
            and (
                classification is None
                or (
                    bool(classification["summary"]["passed"])
                    and bool(
                        classification["operational_contract"][
                            "production_batch_contract_compatible"
                        ]
                    )
                )
            ),
            "privacy": {
                "plaintext_written_to_disk": False,
                "report_contains_embeddings": False,
                "report_contains_titles_ocr_or_process_names": False,
            },
        }
        output_dir = args.output_dir or args.data_dir.parent / "bge-benchmark-results"
        output_dir.mkdir(parents=True, exist_ok=True)
        stamp = time.strftime("%Y%m%d-%H%M%S")
        json_path = output_dir / f"bge-benchmark-{stamp}.json"
        markdown_path = output_dir / f"bge-benchmark-{stamp}.md"
        assert_report_has_no_plaintext_payloads(report)
        json_path.write_text(
            json.dumps(report, ensure_ascii=False, indent=2) + "\n", encoding="utf-8"
        )
        markdown_path.write_text(_markdown_report(report), encoding="utf-8")
        return json_path, markdown_path
    finally:
        _clear_plaintext(samples, texts)


def main() -> int:
    if sys.argv[1:] == ["--_python-runner"]:
        return _run_internal_python_runner()
    try:
        result = run(parse_args())
    except (
        FileNotFoundError,
        RuntimeError,
        ValueError,
        subprocess.SubprocessError,
    ) as error:
        print(f"BGE benchmark failed: {error}", file=sys.stderr)
        return 1
    if result:
        json_path, markdown_path = result
        print(f"JSON report: {json_path}")
        print(f"Markdown report: {markdown_path}")
        print(
            "Decrypted database samples were kept in memory only and have been released."
        )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
