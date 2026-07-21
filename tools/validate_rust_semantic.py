"""Validate the isolated Rust semantic worker against the committed M2 oracle.

This command is intentionally local and offline. It uses already-installed model
assets, talks to ``carbonpaper-semantic-worker.exe`` over the framed ML protocol,
and never downloads models or sends telemetry.
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import threading
from pathlib import Path
from typing import Any, Mapping, Sequence

import numpy as np

from migration_oracle import (
    CPU_TOLERANCE_PROFILE,
    DEFAULT_FIXTURES,
    DEFAULT_GOLDEN,
    DIRECTML_TOLERANCE_PROFILE,
    _compare_tensor,
    _make_image,
    load_fixtures,
    tensor_from_record,
    tensor_record,
    validate_oracle_structure,
)


REPO_ROOT = Path(__file__).resolve().parents[1]
DEFAULT_APPDATA_ROOT = Path(
    os.environ.get("LOCALAPPDATA", Path.home() / "AppData" / "Local")
) / "carbonpaper"
PROTOCOL_VERSION = 3
REQUEST_TIMEOUT_MS = 10 * 60 * 1000


def _first_file(candidates: Sequence[Path], label: str) -> Path:
    for candidate in candidates:
        if candidate.is_file():
            return candidate.resolve()
    rendered = "\n  - ".join(str(candidate) for candidate in candidates)
    raise FileNotFoundError(f"{label} was not found. Checked:\n  - {rendered}")


def _default_worker() -> Path:
    return _first_file(
        (
            REPO_ROOT / "src-tauri" / "pre-bundle" / "carbonpaper-semantic-worker.exe",
            REPO_ROOT
            / "src-tauri"
            / "semantic-worker"
            / "target"
            / "release"
            / "carbonpaper-semantic-worker.exe",
            REPO_ROOT
            / "src-tauri"
            / "semantic-worker"
            / "target"
            / "debug"
            / "carbonpaper-semantic-worker.exe",
        ),
        "Rust semantic worker",
    )


def _default_ort_dylib() -> Path:
    configured = os.environ.get("CARBONPAPER_ORT_DYLIB_PATH")
    candidates = [] if not configured else [Path(configured)]
    candidates.extend(
        (
            REPO_ROOT
            / "src-tauri"
            / "pre-bundle"
            / "onnxruntime"
            / "1.24.2"
            / "onnxruntime.dll",
            DEFAULT_APPDATA_ROOT
            / "models-onnx"
            / "runtime"
            / "1.24.2"
            / "onnxruntime.dll",
            DEFAULT_APPDATA_ROOT
            / ".venv"
            / "Lib"
            / "site-packages"
            / "onnxruntime"
            / "capi"
            / "onnxruntime.dll",
        )
    )
    return _first_file(candidates, "ONNX Runtime DLL")


def _read_exact(stream: Any, length: int) -> bytes:
    chunks = bytearray()
    while len(chunks) < length:
        chunk = stream.read(length - len(chunks))
        if not chunk:
            raise EOFError(
                f"semantic worker closed its response stream after {len(chunks)} of "
                f"{length} bytes"
            )
        chunks.extend(chunk)
    return bytes(chunks)


class SemanticWorker:
    def __init__(
        self,
        executable: Path,
        ort_dylib: Path,
        models_root: Path,
        onnx_models_root: Path,
        provider: str,
        dml_device_id: int,
    ) -> None:
        command = [
            str(executable),
            "--models-root",
            str(models_root),
            "--onnx-models-root",
            str(onnx_models_root),
            "--ort-dylib",
            str(ort_dylib),
        ]
        if provider == DIRECTML_TOLERANCE_PROFILE:
            command.extend(("--directml", "--dml-device-id", str(dml_device_id)))

        environment = os.environ.copy()
        environment.update(
            {
                "DO_NOT_TRACK": "1",
                "HF_HUB_DISABLE_TELEMETRY": "1",
                "HF_HUB_OFFLINE": "1",
                "TRANSFORMERS_OFFLINE": "1",
            }
        )
        environment["PATH"] = os.pathsep.join(
            (str(ort_dylib.parent), environment.get("PATH", ""))
        )
        self.process = subprocess.Popen(
            command,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            cwd=REPO_ROOT,
            env=environment,
        )
        self._stderr_lines: list[str] = []
        self._request_id = 0
        self._stderr_thread = threading.Thread(
            target=self._drain_stderr,
            name="semantic-oracle-stderr",
            daemon=True,
        )
        self._stderr_thread.start()

    def _drain_stderr(self) -> None:
        assert self.process.stderr is not None
        for raw_line in iter(self.process.stderr.readline, b""):
            self._stderr_lines.append(raw_line.decode("utf-8", errors="replace").rstrip())

    def _diagnostics(self) -> str:
        if not self._stderr_lines:
            return ""
        return "\nworker stderr:\n" + "\n".join(self._stderr_lines[-30:])

    def read_response(self) -> Mapping[str, Any]:
        assert self.process.stdout is not None
        try:
            length = int.from_bytes(_read_exact(self.process.stdout, 4), "little")
            if length <= 0 or length > 1024 * 1024:
                raise ValueError(f"invalid semantic response frame length: {length}")
            response = json.loads(_read_exact(self.process.stdout, length))
        except Exception as error:
            raise RuntimeError(f"failed to read semantic response: {error}{self._diagnostics()}") from error
        if not isinstance(response, Mapping):
            raise RuntimeError(f"semantic response is not an object: {response!r}")
        return response

    def request(self, command: str, *, body: bytes = b"", **fields: Any) -> Mapping[str, Any]:
        if self.process.poll() is not None:
            raise RuntimeError(
                f"semantic worker exited with {self.process.returncode}{self._diagnostics()}"
            )
        self._request_id += 1
        request_id = self._request_id
        request = {"command": command, "request_id": request_id, **fields}
        payload = json.dumps(request, ensure_ascii=False, separators=(",", ":")).encode(
            "utf-8"
        )
        assert self.process.stdin is not None
        self.process.stdin.write(len(payload).to_bytes(4, "little"))
        self.process.stdin.write(payload)
        if body:
            self.process.stdin.write(body)
        self.process.stdin.flush()
        response = self.read_response()
        if response.get("status") == "error":
            raise RuntimeError(
                f"semantic request {command} failed: {response.get('kind')}: "
                f"{response.get('message')}{self._diagnostics()}"
            )
        if response.get("request_id") != request_id:
            raise RuntimeError(
                f"semantic response request_id mismatch: {response.get('request_id')} != "
                f"{request_id}"
            )
        return response

    def close(self) -> None:
        if self.process.poll() is None:
            try:
                response = self.request("shutdown")
                if response.get("status") != "shutting_down":
                    raise RuntimeError(f"unexpected shutdown response: {response!r}")
                self.process.wait(timeout=10)
            except Exception:
                self.process.kill()
                self.process.wait(timeout=10)
                raise
        if self.process.returncode not in (0, None):
            raise RuntimeError(
                f"semantic worker exited with {self.process.returncode}{self._diagnostics()}"
            )

    def kill(self) -> None:
        if self.process.poll() is None:
            self.process.kill()
            self.process.wait(timeout=10)


def _actual_record(array: np.ndarray, expected: Mapping[str, Any]) -> Mapping[str, Any]:
    return tensor_record(
        np.asarray(array),
        comparison=str(expected["comparison"]),
        tolerances=expected.get("tolerances"),
        min_cosine=expected.get("min_cosine"),
        max_abs_error=expected.get("max_abs_error"),
    )


def _report_tensor(
    label: str,
    expected: Mapping[str, Any],
    actual_array: np.ndarray,
    tolerance_profile: str,
) -> None:
    actual = _actual_record(actual_array, expected)
    _compare_tensor(label, expected, actual, tolerance_profile)
    expected_array = tensor_from_record(expected)
    if expected["comparison"] == "exact":
        mismatches = int(np.count_nonzero(expected_array != actual_array))
        print(f"PASS {label}: exact, mismatches={mismatches}")
        return
    max_abs = float(np.max(np.abs(expected_array - actual_array)))
    if expected["comparison"] == "cosine":
        expected_rows = expected_array.reshape((-1, expected_array.shape[-1]))
        actual_rows = actual_array.reshape((-1, actual_array.shape[-1]))
        numerator = np.sum(expected_rows * actual_rows, axis=1)
        denominator = np.linalg.norm(expected_rows, axis=1) * np.linalg.norm(
            actual_rows, axis=1
        )
        cosine = numerator / np.clip(denominator, a_min=1e-12, a_max=None)
        print(
            f"PASS {label}: max_abs={max_abs:.9g}, min_cosine={float(np.min(cosine)):.9g}"
        )
    else:
        print(f"PASS {label}: max_abs={max_abs:.9g}")


def _validate_tokenization(
    worker: SemanticWorker,
    label: str,
    model: str,
    texts: Sequence[str],
    expected: Mapping[str, Any],
    text_pairs: Sequence[str] | None = None,
) -> None:
    response = worker.request(
        "inspect_tokenization",
        model=model,
        texts=list(texts),
        text_pairs=None if text_pairs is None else list(text_pairs),
    )
    if response.get("status") != "tokenization_complete" or response.get("model") != model:
        raise RuntimeError(f"unexpected tokenization response: {response!r}")
    shape = (int(response["batch"]), int(response["sequence"]))
    for name in ("input_ids", "attention_mask", "token_type_ids"):
        values = np.asarray(response[name], dtype=np.int64).reshape(shape)
        _report_tensor(f"{label}.tokenization.{name}", expected[name], values, "cpu")


def _validate_embedding(
    worker: SemanticWorker,
    label: str,
    model: str,
    texts: Sequence[str],
    expected: Mapping[str, Any],
    tolerance_profile: str,
) -> None:
    response = worker.request(
        "embed_text",
        model=model,
        texts=list(texts),
        timeout_ms=REQUEST_TIMEOUT_MS,
    )
    if response.get("status") != "embedding_complete" or response.get("model") != model:
        raise RuntimeError(f"unexpected embedding response: {response!r}")
    vectors = np.asarray(response["vectors"], dtype=np.float32)
    _report_tensor(label, expected, vectors, tolerance_profile)


def validate(args: argparse.Namespace) -> None:
    fixtures = load_fixtures(args.fixtures)
    golden = json.loads(args.golden.read_text(encoding="utf-8"))
    validate_oracle_structure(golden)
    worker_path = args.worker.resolve() if args.worker else _default_worker()
    ort_dylib = args.ort_dylib.resolve() if args.ort_dylib else _default_ort_dylib()
    models_root = args.models_root.resolve()
    onnx_models_root = args.onnx_models_root.resolve()
    if not models_root.is_dir():
        raise FileNotFoundError(f"models root is missing: {models_root}")
    if not onnx_models_root.is_dir():
        raise FileNotFoundError(f"ONNX models root is missing: {onnx_models_root}")

    print(f"Worker: {worker_path}")
    print(f"ONNX Runtime: {ort_dylib}")
    print(f"Provider/tolerances: {args.provider}")
    worker = SemanticWorker(
        worker_path,
        ort_dylib,
        models_root,
        onnx_models_root,
        args.provider,
        args.dml_device_id,
    )
    completed = False
    try:
        ready = worker.read_response()
        expected_provider = "direct_ml" if args.provider == DIRECTML_TOLERANCE_PROFILE else "cpu"
        if (
            ready.get("status") != "semantic_ready"
            or ready.get("protocol_version") != PROTOCOL_VERSION
            or ready.get("provider") != expected_provider
        ):
            raise RuntimeError(f"invalid semantic worker handshake: {ready!r}")
        print(
            f"Ready: worker={ready.get('worker_version')}, ORT={ready.get('ort_version')}, "
            f"provider={ready.get('provider')}"
        )

        models = golden["models"]
        supported_models = set(ready.get("supported_models", []))
        if args.provider == DIRECTML_TOLERANCE_PROFILE:
            expected_supported = {"chinese_clip", "bge_small_zh"}
            if supported_models != expected_supported:
                raise RuntimeError(
                    "DirectML supported-model set differs from the reviewed parity gate: "
                    f"{sorted(supported_models)}"
                )
            print(
                "SKIP minilm/reranker on DirectML: worker advertises explicit CPU fallback "
                "because their quantized kernels exceed the reviewed numeric gate"
            )
        else:
            minilm_texts = [
                case["text"] for case in golden["contracts"]["minilm_task_texts"]
            ]
            _validate_tokenization(
                worker,
                "minilm",
                "minilm_l12",
                minilm_texts,
                models["minilm"]["tokenization"],
            )
            _validate_embedding(
                worker,
                "minilm.embeddings",
                "minilm_l12",
                minilm_texts,
                models["minilm"]["embeddings"],
                args.provider,
            )

        bge_texts = [case["text"] for case in fixtures["bge"]["texts"]]
        _validate_tokenization(
            worker,
            "bge",
            "bge_small_zh",
            bge_texts,
            models["bge"]["tokenization"],
        )
        _validate_embedding(
            worker,
            "bge.embeddings",
            "bge_small_zh",
            bge_texts,
            models["bge"]["embeddings"],
            args.provider,
        )

        clip_texts = [case["text"] for case in fixtures["clip"]["texts"]]
        _validate_tokenization(
            worker,
            "clip",
            "chinese_clip",
            clip_texts,
            models["clip"]["tokenization"],
        )
        _validate_embedding(
            worker,
            "clip.text_embeddings",
            "chinese_clip",
            clip_texts,
            models["clip"]["text_embeddings"],
            args.provider,
        )

        image_body = bytearray()
        image_inputs = []
        for image_spec in fixtures["clip"]["images"]:
            image = _make_image(image_spec).convert("RGB")
            raw = image.tobytes()
            image_inputs.append(
                {
                    "width": image.width,
                    "height": image.height,
                    "stride": image.width * 3,
                    "offset": len(image_body),
                    "body_len": len(raw),
                }
            )
            image_body.extend(raw)
        response = worker.request(
            "embed_image",
            model="chinese_clip",
            images=image_inputs,
            body_len=len(image_body),
            timeout_ms=REQUEST_TIMEOUT_MS,
            body=bytes(image_body),
        )
        if response.get("status") != "embedding_complete":
            raise RuntimeError(f"unexpected image embedding response: {response!r}")
        _report_tensor(
            "clip.image_embeddings",
            models["clip"]["image_embeddings"],
            np.asarray(response["vectors"], dtype=np.float32),
            args.provider,
        )

        if args.provider == CPU_TOLERANCE_PROFILE:
            query = str(fixtures["reranker"]["query"])
            documents = [case["text"] for case in fixtures["reranker"]["documents"]]
            _validate_tokenization(
                worker,
                "reranker",
                "bge_reranker_v2_m3",
                [query] * len(documents),
                models["reranker"]["tokenization"],
                text_pairs=documents,
            )
            response = worker.request(
                "rerank",
                model="bge_reranker_v2_m3",
                query=query,
                documents=documents,
                timeout_ms=REQUEST_TIMEOUT_MS,
            )
            if response.get("status") != "rerank_complete":
                raise RuntimeError(f"unexpected reranker response: {response!r}")
            _report_tensor(
                "reranker.raw_logits",
                models["reranker"]["raw_logits"],
                np.asarray(response["scores"], dtype=np.float32),
                args.provider,
            )
        completed = True
    finally:
        if completed:
            worker.close()
        else:
            worker.kill()
    print("Rust semantic worker matches the committed M2 oracle.")


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--provider",
        choices=(CPU_TOLERANCE_PROFILE, DIRECTML_TOLERANCE_PROFILE),
        default=CPU_TOLERANCE_PROFILE,
    )
    parser.add_argument("--dml-device-id", type=int, default=0)
    parser.add_argument("--worker", type=Path)
    parser.add_argument("--ort-dylib", type=Path)
    parser.add_argument(
        "--models-root", type=Path, default=DEFAULT_APPDATA_ROOT / "models"
    )
    parser.add_argument(
        "--onnx-models-root", type=Path, default=DEFAULT_APPDATA_ROOT / "models-onnx"
    )
    parser.add_argument("--fixtures", type=Path, default=DEFAULT_FIXTURES)
    parser.add_argument("--golden", type=Path, default=DEFAULT_GOLDEN)
    args = parser.parse_args()
    if args.dml_device_id < 0:
        parser.error("--dml-device-id must be non-negative")
    if args.onnx_models_root == DEFAULT_APPDATA_ROOT / "models-onnx" and not args.onnx_models_root.is_dir():
        args.onnx_models_root = args.models_root
    return args


if __name__ == "__main__":
    try:
        validate(parse_args())
    except (AssertionError, FileNotFoundError, RuntimeError, ValueError) as error:
        print(f"Rust semantic oracle validation failed: {error}", file=sys.stderr)
        raise SystemExit(1) from error
