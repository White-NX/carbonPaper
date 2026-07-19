"""Generate and validate the M2 Python behavior oracle.

The canonical oracle is CPU-only and uses already-installed ONNX model assets.
No fixture contains user data and this module never downloads models or emits
telemetry. Future Rust parity tests consume the same JSON tensor records.
"""

from __future__ import annotations

import argparse
import base64
import copy
import hashlib
import json
import os
import platform
import sys
import zlib
from pathlib import Path
from typing import Any, Dict, Iterable, List, Mapping, Sequence

import numpy as np
from PIL import Image


REPO_ROOT = Path(__file__).resolve().parents[1]
MONITOR_DIR = REPO_ROOT / "monitor"
if str(MONITOR_DIR) not in sys.path:
    sys.path.insert(0, str(MONITOR_DIR))
DEFAULT_FIXTURES = MONITOR_DIR / "oracle" / "fixtures.json"
DEFAULT_GOLDEN = MONITOR_DIR / "oracle" / "golden-v1.json"
ORACLE_SCHEMA_VERSION = 1
TARGET_RELEASE = "v0.8.4-beta"
CPU_TOLERANCE_PROFILE = "cpu"
DIRECTML_TOLERANCE_PROFILE = "directml"
EMBEDDING_TOLERANCES = {
    CPU_TOLERANCE_PROFILE: {"min_cosine": 0.99999, "max_abs_error": 1e-4},
    DIRECTML_TOLERANCE_PROFILE: {"min_cosine": 0.999, "max_abs_error": 1e-3},
}
RERANKER_LOGIT_TOLERANCES = {
    CPU_TOLERANCE_PROFILE: {"max_abs_error": 1e-4},
    DIRECTML_TOLERANCE_PROFILE: {"max_abs_error": 1e-3},
}


def load_fixtures(path: Path = DEFAULT_FIXTURES) -> Dict[str, Any]:
    with path.open("r", encoding="utf-8") as fixture_file:
        fixtures = json.load(fixture_file)
    if fixtures.get("schema_version") != ORACLE_SCHEMA_VERSION:
        raise ValueError("Unsupported migration-oracle fixture schema")
    if fixtures.get("target_release") != TARGET_RELEASE:
        raise ValueError("Migration-oracle fixtures target the wrong release")
    return fixtures


def _canonical_json(value: Any) -> bytes:
    return json.dumps(
        value, ensure_ascii=False, sort_keys=True, separators=(",", ":")
    ).encode("utf-8")


def _sha256_bytes(value: bytes) -> str:
    return hashlib.sha256(value).hexdigest()


def fixture_sha256(fixtures: Mapping[str, Any]) -> str:
    return _sha256_bytes(_canonical_json(fixtures))


def _file_fingerprint(path: Path, model_root: Path) -> Dict[str, Any]:
    digest = hashlib.sha256()
    with path.open("rb") as model_file:
        for chunk in iter(lambda: model_file.read(1024 * 1024), b""):
            digest.update(chunk)
    try:
        relative_path = path.resolve().relative_to(model_root.resolve()).as_posix()
    except ValueError:
        relative_path = path.name
    return {
        "path": relative_path,
        "size": path.stat().st_size,
        "sha256": digest.hexdigest(),
    }


def _little_endian_contiguous(array: np.ndarray) -> np.ndarray:
    array = np.ascontiguousarray(array)
    dtype = array.dtype
    if dtype.byteorder == ">" or (dtype.byteorder == "=" and not np.little_endian):
        array = array.byteswap().view(dtype.newbyteorder("<"))
    return array


def tensor_record(
    array: np.ndarray,
    *,
    comparison: str,
    min_cosine: float | None = None,
    max_abs_error: float | None = None,
    tolerances: Mapping[str, Mapping[str, float]] | None = None,
) -> Dict[str, Any]:
    if tolerances is not None and (min_cosine is not None or max_abs_error is not None):
        raise ValueError("Use either provider tolerances or legacy scalar tolerances")
    array = _little_endian_contiguous(np.asarray(array))
    raw = array.tobytes(order="C")
    encoded = raw
    encoding = "base64"
    if len(raw) >= 4096:
        compressed = zlib.compress(raw, level=9)
        if len(compressed) < len(raw):
            encoded = compressed
            encoding = "zlib+base64"
    record: Dict[str, Any] = {
        "dtype": array.dtype.str,
        "shape": list(array.shape),
        "sha256": _sha256_bytes(raw),
        "encoding": encoding,
        "values_b64": base64.b64encode(encoded).decode("ascii"),
        "comparison": comparison,
    }
    if min_cosine is not None:
        record["min_cosine"] = min_cosine
    if max_abs_error is not None:
        record["max_abs_error"] = max_abs_error
    if tolerances is not None:
        record["tolerances"] = {
            profile: {
                name: float(value)
                for name, value in sorted(profile_limits.items())
            }
            for profile, profile_limits in sorted(tolerances.items())
        }
    return record


def tensor_from_record(record: Mapping[str, Any]) -> np.ndarray:
    encoded = base64.b64decode(record["values_b64"], validate=True)
    encoding = record.get("encoding", "base64")
    if encoding == "zlib+base64":
        raw = zlib.decompress(encoded)
    elif encoding == "base64":
        raw = encoded
    else:
        raise ValueError(f"Unsupported oracle tensor encoding: {encoding}")
    array = np.frombuffer(raw, dtype=np.dtype(record["dtype"]))
    expected_size = int(np.prod(record["shape"], dtype=np.int64))
    if array.size != expected_size:
        raise ValueError("Oracle tensor byte length does not match its shape")
    array = array.reshape(record["shape"])
    if _sha256_bytes(raw) != record["sha256"]:
        raise ValueError("Oracle tensor checksum mismatch")
    return array


def _make_image(spec: Mapping[str, Any]) -> Image.Image:
    width = int(spec["width"])
    height = int(spec["height"])
    y, x = np.indices((height, width), dtype=np.uint16)
    if spec["kind"] == "rgb_gradient":
        pixels = np.stack(
            (
                (x * 7 + y * 3) % 256,
                (x * 2 + y * 11 + 17) % 256,
                (x * 13 + y * 5 + 29) % 256,
            ),
            axis=-1,
        ).astype(np.uint8)
    elif spec["kind"] == "checkerboard":
        cell = max(1, int(spec.get("cell", 4)))
        mask = ((x // cell) + (y // cell)) % 2
        first = np.asarray([24, 92, 180], dtype=np.uint8)
        second = np.asarray([238, 192, 48], dtype=np.uint8)
        pixels = np.where(mask[..., None] == 0, first, second).astype(np.uint8)
    else:
        raise ValueError(f"Unknown oracle image kind: {spec['kind']}")
    return Image.fromarray(pixels)


def _token_records(encoded: Mapping[str, np.ndarray]) -> Dict[str, Any]:
    return {
        name: tensor_record(np.asarray(value), comparison="exact")
        for name, value in sorted(encoded.items())
    }


class _RecordingVectorStore:
    def __init__(self, results: Sequence[Mapping[str, Any]]):
        self.results = copy.deepcopy(list(results))
        self.requested_n_results: int | None = None
        self.requested_min_similarity: float | None = None

    def search_by_text(
        self, _query: str, n_results: int, min_similarity: float
    ) -> List[Dict[str, Any]]:
        self.requested_n_results = n_results
        self.requested_min_similarity = min_similarity
        return copy.deepcopy(self.results[:n_results])


class _FixedQueryVectorizer:
    def encode_text(self, _query: str) -> np.ndarray:
        return np.asarray([1.0, 0.0], dtype=np.float32)


class _RecordingCollection:
    def __init__(self, rows: Sequence[Mapping[str, Any]]):
        self.rows = list(rows)
        self.request: Dict[str, Any] | None = None

    def query(self, **kwargs) -> Dict[str, Any]:
        self.request = copy.deepcopy(kwargs)
        return {
            "ids": [[row["id"] for row in self.rows]],
            "distances": [[row["distance"] for row in self.rows]],
            "documents": [[row["document"] for row in self.rows]],
            "metadatas": [[copy.deepcopy(row["metadata"]) for row in self.rows]],
        }


def capture_clip_vector_search_contract(case: Mapping[str, Any]) -> Dict[str, Any]:
    from vector_store import DEFAULT_CLIP_MIN_SIMILARITY, VectorStore

    if float(case["min_similarity"]) != DEFAULT_CLIP_MIN_SIMILARITY:
        raise ValueError("CLIP fixture threshold differs from the live search contract")

    collection = _RecordingCollection(case["rows"])
    vector_store = VectorStore.__new__(VectorStore)
    vector_store.vectorizer = _FixedQueryVectorizer()
    vector_store.collection = collection
    vector_store.storage_client = None
    results = VectorStore.search_by_text(
        vector_store,
        query=case["query"],
        n_results=case["n_results"],
        min_similarity=case["min_similarity"],
    )
    request = collection.request or {}
    return {
        "metric": "cosine",
        "min_similarity": case["min_similarity"],
        "requested_n_results": request.get("n_results"),
        "include": request.get("include"),
        "results": results,
    }


def capture_search_contract(case: Mapping[str, Any]) -> Dict[str, Any]:
    from ocr_service import OCRService

    vector_store = _RecordingVectorStore(case["raw_results"])
    service = OCRService.__new__(OCRService)
    service.vector_store = vector_store
    results = OCRService.search_by_natural_language(
        service,
        query=case["query"],
        n_results=case["n_results"],
        offset=case["offset"],
        process_names=case.get("process_names"),
        start_time=case.get("start_time"),
        end_time=case.get("end_time"),
    )
    return {
        "requested_n_results": vector_store.requested_n_results,
        "requested_min_similarity": vector_store.requested_min_similarity,
        "results": results,
    }


def build_static_contracts(fixtures: Mapping[str, Any]) -> Dict[str, Any]:
    from task_clustering import build_task_text
    from vector_store import ChineseCLIPSingleton

    preprocessor = fixtures["clip"]["preprocessor"]
    clip = object.__new__(ChineseCLIPSingleton)
    clip._image_size = int(preprocessor["image_size"])
    clip._image_mean = np.asarray(preprocessor["image_mean"], dtype=np.float32)
    clip._image_std = np.asarray(preprocessor["image_std"], dtype=np.float32)
    clip._rescale_factor = np.float32(preprocessor["rescale_factor"])
    images = [_make_image(spec) for spec in fixtures["clip"]["images"]]
    pixels = clip.preprocess_images(images)

    task_texts = [
        {
            "id": case["id"],
            "text": build_task_text(
                case["process_name"], case["window_title"], case["ocr_text"]
            ),
        }
        for case in fixtures["minilm"]["cases"]
    ]
    search_cases = {
        case["id"]: capture_search_contract(case)
        for case in fixtures["search_contract"]["cases"]
    }
    return {
        "clip_preprocessing": {
            "settings": copy.deepcopy(preprocessor),
            "pixels": tensor_record(
                pixels,
                comparison="allclose",
                max_abs_error=1e-6,
            ),
        },
        "minilm_task_texts": task_texts,
        "clip_vector_search": capture_clip_vector_search_contract(
            fixtures["search_contract"]["clip_vector_search"]
        ),
        "search_nl": search_cases,
    }


def _require_file(path: Path, label: str) -> Path:
    if not path.is_file():
        raise FileNotFoundError(f"{label} is missing: {path}")
    return path


def _resolve_model_files(model_root: Path, reranker_variant: str) -> Dict[str, Path]:
    minilm_root = model_root / "paraphrase-multilingual-MiniLM-L12-v2"
    bge_root = model_root / "bge-small-zh-v1.5"
    reranker_root = model_root / "bge-reranker-v2-m3"
    return {
        "clip_onnx": _require_file(model_root / "onnx" / "model_q4.onnx", "CLIP ONNX"),
        "clip_tokenizer": _require_file(model_root / "tokenizer.json", "CLIP tokenizer"),
        "clip_preprocessor": _require_file(
            model_root / "preprocessor_config.json", "CLIP preprocessor"
        ),
        "minilm_onnx": _require_file(
            minilm_root / "onnx" / "model_quantized.onnx", "MiniLM ONNX"
        ),
        "minilm_tokenizer": _require_file(
            minilm_root / "tokenizer.json", "MiniLM tokenizer"
        ),
        "bge_onnx": _require_file(
            bge_root / "onnx" / "model_quantized.onnx", "BGE ONNX"
        ),
        "bge_tokenizer": _require_file(bge_root / "tokenizer.json", "BGE tokenizer"),
        "reranker_onnx": _require_file(
            reranker_root / "onnx" / f"model_{reranker_variant}.onnx",
            "reranker ONNX",
        ),
        "reranker_tokenizer": _require_file(
            reranker_root / "tokenizer.json", "reranker tokenizer"
        ),
    }


def _configure_cpu_onnx(model_root: Path) -> None:
    os.environ["CARBONPAPER_USE_ONNX"] = "1"
    os.environ["CARBONPAPER_USE_DML"] = "0"
    os.environ["CARBONPAPER_ONNX_LOAD_MODE"] = "buffer"
    os.environ["MODEL_PATH"] = str(model_root)
    os.environ["MINILM_MODEL_PATH"] = str(
        model_root / "paraphrase-multilingual-MiniLM-L12-v2"
    )
    os.environ["BGE_MODEL_PATH"] = str(model_root / "bge-small-zh-v1.5")
    os.environ["RERANKER_MODEL_PATH"] = str(model_root / "bge-reranker-v2-m3")


def _model_fingerprints(
    paths: Mapping[str, Path], model_root: Path, names: Iterable[str]
) -> List[Dict[str, Any]]:
    return [_file_fingerprint(paths[name], model_root) for name in names]


def generate_oracle(
    fixtures: Mapping[str, Any], model_root: Path
) -> Dict[str, Any]:
    _configure_cpu_onnx(model_root)
    reranker_variant = fixtures["reranker"]["variant"]
    paths = _resolve_model_files(model_root, reranker_variant)

    from classifier import TextEmbedder as BgeEmbedder
    from reranker import Reranker
    from task_clustering import TaskEmbedder, build_task_text
    from vector_store import ChineseCLIPSingleton, ImageVectorizer

    contracts = build_static_contracts(fixtures)

    clip = ChineseCLIPSingleton()
    clip.initialize()
    expected_preprocessor = fixtures["clip"]["preprocessor"]
    if clip._image_size != expected_preprocessor["image_size"]:
        raise ValueError("Installed CLIP image size differs from the fixture contract")
    if not np.allclose(clip._image_mean, expected_preprocessor["image_mean"]):
        raise ValueError("Installed CLIP image mean differs from the fixture contract")
    if not np.allclose(clip._image_std, expected_preprocessor["image_std"]):
        raise ValueError("Installed CLIP image std differs from the fixture contract")

    clip_texts = [case["text"] for case in fixtures["clip"]["texts"]]
    clip_images = [_make_image(spec) for spec in fixtures["clip"]["images"]]
    clip_tokens = clip.tokenize_texts(clip_texts)
    clip_vectorizer = ImageVectorizer()
    clip_text_embeddings = np.stack(
        [clip_vectorizer.encode_text(text) for text in clip_texts]
    ).astype(np.float32)
    clip_image_embeddings = np.stack(
        [clip_vectorizer.encode_image(image) for image in clip_images]
    ).astype(np.float32)

    minilm = TaskEmbedder()
    minilm.load()
    task_texts = [
        build_task_text(case["process_name"], case["window_title"], case["ocr_text"])
        for case in fixtures["minilm"]["cases"]
    ]
    minilm_tokens = minilm._tokenizer(
        task_texts,
        padding=True,
        truncation=True,
        max_length=fixtures["minilm"]["max_length"],
        return_tensors="np",
    )
    minilm_embeddings = minilm.encode(task_texts).astype(np.float32)

    bge = BgeEmbedder()
    bge.initialize()
    bge_texts = [case["text"] for case in fixtures["bge"]["texts"]]
    bge_tokens = bge._tokenizer(
        bge_texts,
        padding=True,
        truncation=True,
        max_length=fixtures["bge"]["max_length"],
        return_tensors="np",
    )
    bge_embeddings = bge.encode(bge_texts).astype(np.float32)

    reranker = Reranker()
    import onnxruntime as ort

    original_available_providers = ort.get_available_providers
    try:
        # Reranker.load currently prefers DirectML whenever it is installed.
        # The committed oracle must be hardware-independent, so expose only
        # the CPU provider while constructing this canonical session.
        ort.get_available_providers = lambda: ["CPUExecutionProvider"]
        reranker.load(reranker_variant)
    finally:
        ort.get_available_providers = original_available_providers
    reranker_documents = [case["text"] for case in fixtures["reranker"]["documents"]]
    reranker_pairs = [(fixtures["reranker"]["query"], doc) for doc in reranker_documents]
    reranker_tokens = reranker._tokenizer(
        reranker_pairs,
        padding=True,
        truncation=True,
        max_length=fixtures["reranker"]["max_length"],
        return_tensors="np",
    )
    reranker_logits = np.asarray(
        reranker.rerank(
            fixtures["reranker"]["query"],
            reranker_documents,
            max_length=fixtures["reranker"]["max_length"],
            variant=reranker_variant,
        ),
        dtype=np.float32,
    )

    runtime_version = ort.__version__

    return {
        "schema_version": ORACLE_SCHEMA_VERSION,
        "target_release": TARGET_RELEASE,
        "fixture_sha256": fixture_sha256(fixtures),
        "generated_by": {
            "python": platform.python_version(),
            "numpy": np.__version__,
            "onnxruntime": runtime_version,
            "provider": "CPUExecutionProvider",
        },
        "contracts": contracts,
        "models": {
            "clip": {
                "model_id": "chinese-clip-vit-base-patch16",
                "contract": {
                    "text_truncation": False,
                    "normalization": "l2",
                    "image_preprocessing": "rgb_square_bicubic_rescale_normalize",
                },
                "fingerprints": _model_fingerprints(
                    paths,
                    model_root,
                    ("clip_onnx", "clip_tokenizer", "clip_preprocessor"),
                ),
                "input_names": sorted(clip._input_meta.keys()),
                "providers": clip._session.get_providers(),
                "image_output_name": clip._image_output_name,
                "text_output_name": clip._text_output_name,
                "tokenization": _token_records(clip_tokens),
                "text_embeddings": tensor_record(
                    clip_text_embeddings,
                    comparison="cosine",
                    tolerances=EMBEDDING_TOLERANCES,
                ),
                "image_embeddings": tensor_record(
                    clip_image_embeddings,
                    comparison="cosine",
                    tolerances=EMBEDDING_TOLERANCES,
                ),
            },
            "minilm": {
                "model_id": "paraphrase-multilingual-MiniLM-L12-v2",
                "contract": {
                    "max_length": fixtures["minilm"]["max_length"],
                    "pooling": fixtures["minilm"]["pooling"],
                    "normalization": fixtures["minilm"]["normalization"],
                    "text_format": "process | title | OCR[:200]",
                },
                "fingerprints": _model_fingerprints(
                    paths, model_root, ("minilm_onnx", "minilm_tokenizer")
                ),
                "input_names": [item.name for item in minilm._model.get_inputs()],
                "output_names": [item.name for item in minilm._model.get_outputs()],
                "providers": minilm._model.get_providers(),
                "tokenization": _token_records(minilm_tokens),
                "embeddings": tensor_record(
                    minilm_embeddings,
                    comparison="cosine",
                    tolerances=EMBEDDING_TOLERANCES,
                ),
            },
            "bge": {
                "model_id": "bge-small-zh-v1.5",
                "contract": {
                    "max_length": fixtures["bge"]["max_length"],
                    "pooling": fixtures["bge"]["pooling"],
                    "normalization": fixtures["bge"]["normalization"],
                },
                "fingerprints": _model_fingerprints(
                    paths, model_root, ("bge_onnx", "bge_tokenizer")
                ),
                "input_names": [item.name for item in bge._model.get_inputs()],
                "output_names": [item.name for item in bge._model.get_outputs()],
                "providers": bge._model.get_providers(),
                "tokenization": _token_records(bge_tokens),
                "embeddings": tensor_record(
                    bge_embeddings,
                    comparison="cosine",
                    tolerances=EMBEDDING_TOLERANCES,
                ),
            },
            "reranker": {
                "model_id": "bge-reranker-v2-m3",
                "variant": reranker_variant,
                "contract": {
                    "max_length": fixtures["reranker"]["max_length"],
                    "pair_tokenization": True,
                    "score": fixtures["reranker"]["score"],
                },
                "fingerprints": _model_fingerprints(
                    paths, model_root, ("reranker_onnx", "reranker_tokenizer")
                ),
                "input_names": list(reranker._input_names),
                "output_name": reranker._output_name,
                "provider": reranker.provider,
                "tokenization": _token_records(reranker_tokens),
                "raw_logits": tensor_record(
                    reranker_logits,
                    comparison="allclose",
                    tolerances=RERANKER_LOGIT_TOLERANCES,
                ),
            },
        },
    }


def _comparison_limits(
    path: str, record: Mapping[str, Any], tolerance_profile: str
) -> Mapping[str, Any]:
    tolerances = record.get("tolerances")
    if tolerances is None:
        return record
    if not isinstance(tolerances, Mapping):
        raise AssertionError(f"{path}: tolerances must be an object")
    limits = tolerances.get(tolerance_profile)
    if not isinstance(limits, Mapping):
        raise AssertionError(
            f"{path}: missing tolerance profile {tolerance_profile!r}"
        )
    return limits


def _compare_tensor(
    path: str,
    expected: Mapping[str, Any],
    actual: Mapping[str, Any],
    tolerance_profile: str,
) -> None:
    if expected.get("dtype") != actual.get("dtype"):
        raise AssertionError(
            f"{path}: dtype mismatch ({actual.get('dtype')} != {expected.get('dtype')})"
        )
    expected_array = tensor_from_record(expected)
    actual_array = tensor_from_record(actual)
    if expected_array.shape != actual_array.shape:
        raise AssertionError(f"{path}: shape mismatch")
    if not np.all(np.isfinite(expected_array)):
        raise AssertionError(f"{path}: expected tensor contains non-finite values")
    if not np.all(np.isfinite(actual_array)):
        raise AssertionError(f"{path}: actual tensor contains non-finite values")
    mode = expected["comparison"]
    if mode == "exact":
        if not np.array_equal(expected_array, actual_array):
            raise AssertionError(f"{path}: exact tensor mismatch")
        return
    limits = _comparison_limits(path, expected, tolerance_profile)
    max_abs_error = float(limits.get("max_abs_error", 0.0))
    actual_max_abs = float(np.max(np.abs(expected_array - actual_array)))
    if actual_max_abs > max_abs_error:
        raise AssertionError(
            f"{path}: max abs error {actual_max_abs} exceeds {max_abs_error}"
        )
    if mode == "cosine":
        expected_rows = expected_array.reshape((-1, expected_array.shape[-1]))
        actual_rows = actual_array.reshape((-1, actual_array.shape[-1]))
        numerator = np.sum(expected_rows * actual_rows, axis=1)
        denominator = np.linalg.norm(expected_rows, axis=1) * np.linalg.norm(
            actual_rows, axis=1
        )
        cosine = numerator / np.clip(denominator, a_min=1e-12, a_max=None)
        if float(np.min(cosine)) < float(limits["min_cosine"]):
            raise AssertionError(f"{path}: cosine similarity below release gate")
    elif mode != "allclose":
        raise AssertionError(f"{path}: unknown comparison mode {mode}")


def compare_oracles(
    expected: Any,
    actual: Any,
    path: str = "oracle",
    *,
    tolerance_profile: str = CPU_TOLERANCE_PROFILE,
) -> None:
    if isinstance(expected, dict) and "values_b64" in expected:
        if not isinstance(actual, dict):
            raise AssertionError(f"{path}: tensor record is missing")
        _compare_tensor(path, expected, actual, tolerance_profile)
        return
    if isinstance(expected, dict):
        if not isinstance(actual, dict):
            raise AssertionError(f"{path}: expected object")
        ignored = {"generated_by"}
        expected_keys = set(expected) - ignored
        actual_keys = set(actual) - ignored
        if expected_keys != actual_keys:
            raise AssertionError(f"{path}: keys differ")
        for key in sorted(expected_keys):
            compare_oracles(
                expected[key],
                actual[key],
                f"{path}.{key}",
                tolerance_profile=tolerance_profile,
            )
        return
    if isinstance(expected, list):
        if not isinstance(actual, list) or len(expected) != len(actual):
            raise AssertionError(f"{path}: list length differs")
        for index, (expected_item, actual_item) in enumerate(zip(expected, actual)):
            compare_oracles(
                expected_item,
                actual_item,
                f"{path}[{index}]",
                tolerance_profile=tolerance_profile,
            )
        return
    if expected != actual:
        raise AssertionError(f"{path}: {actual!r} != {expected!r}")


def _require_mapping(value: Any, path: str) -> Mapping[str, Any]:
    if not isinstance(value, Mapping):
        raise ValueError(f"{path} must be an object")
    return value


def _require_nonempty_strings(value: Any, path: str) -> List[str]:
    if not isinstance(value, list) or not value:
        raise ValueError(f"{path} must be a non-empty list")
    if not all(isinstance(item, str) and item for item in value):
        raise ValueError(f"{path} must contain non-empty strings")
    return value


def _require_sha256(value: Any, path: str) -> None:
    if not isinstance(value, str) or len(value) != 64:
        raise ValueError(f"{path} must be a SHA-256 hex digest")
    try:
        int(value, 16)
    except ValueError as exc:
        raise ValueError(f"{path} must be a SHA-256 hex digest") from exc


def validate_tensor_record(record: Any, path: str) -> None:
    record = _require_mapping(record, path)
    required = {"dtype", "shape", "sha256", "encoding", "values_b64", "comparison"}
    missing = required - set(record)
    if missing:
        raise ValueError(f"{path} is missing fields: {sorted(missing)}")
    if record["comparison"] not in {"exact", "allclose", "cosine"}:
        raise ValueError(f"{path}.comparison is unsupported")
    _require_sha256(record["sha256"], f"{path}.sha256")
    array = tensor_from_record(record)
    if not np.all(np.isfinite(array)):
        raise ValueError(f"{path} contains non-finite values")

    if record["comparison"] == "exact":
        return
    if "tolerances" not in record:
        max_abs_error = record.get("max_abs_error")
        if not isinstance(max_abs_error, (int, float)) or max_abs_error < 0:
            raise ValueError(f"{path}.max_abs_error must be non-negative")
        if record["comparison"] == "cosine":
            min_cosine = record.get("min_cosine")
            if not isinstance(min_cosine, (int, float)) or not -1 <= min_cosine <= 1:
                raise ValueError(f"{path}.min_cosine must be in [-1, 1]")
        return

    tolerances = _require_mapping(record["tolerances"], f"{path}.tolerances")
    expected_profiles = {CPU_TOLERANCE_PROFILE, DIRECTML_TOLERANCE_PROFILE}
    if set(tolerances) != expected_profiles:
        raise ValueError(f"{path}.tolerances must define cpu and directml")
    for profile in sorted(expected_profiles):
        limits = _require_mapping(tolerances[profile], f"{path}.tolerances.{profile}")
        max_abs_error = limits.get("max_abs_error")
        if not isinstance(max_abs_error, (int, float)) or max_abs_error < 0:
            raise ValueError(
                f"{path}.tolerances.{profile}.max_abs_error must be non-negative"
            )
        if record["comparison"] == "cosine":
            min_cosine = limits.get("min_cosine")
            if not isinstance(min_cosine, (int, float)) or not -1 <= min_cosine <= 1:
                raise ValueError(
                    f"{path}.tolerances.{profile}.min_cosine must be in [-1, 1]"
                )


def validate_oracle_structure(oracle: Any) -> None:
    oracle = _require_mapping(oracle, "oracle")
    required_top_level = {
        "schema_version",
        "target_release",
        "fixture_sha256",
        "generated_by",
        "contracts",
        "models",
    }
    missing = required_top_level - set(oracle)
    if missing:
        raise ValueError(f"oracle is missing fields: {sorted(missing)}")
    if oracle["schema_version"] != ORACLE_SCHEMA_VERSION:
        raise ValueError("oracle has an unsupported schema version")
    if oracle["target_release"] != TARGET_RELEASE:
        raise ValueError("oracle targets the wrong release")
    _require_sha256(oracle["fixture_sha256"], "oracle.fixture_sha256")

    generated_by = _require_mapping(oracle["generated_by"], "oracle.generated_by")
    for field in ("python", "numpy", "onnxruntime", "provider"):
        if not isinstance(generated_by.get(field), str) or not generated_by[field]:
            raise ValueError(f"oracle.generated_by.{field} must be non-empty")

    contracts = _require_mapping(oracle["contracts"], "oracle.contracts")
    expected_contracts = {
        "clip_preprocessing",
        "minilm_task_texts",
        "clip_vector_search",
        "search_nl",
    }
    if set(contracts) != expected_contracts:
        raise ValueError("oracle.contracts has unexpected keys")
    clip_preprocessing = _require_mapping(
        contracts["clip_preprocessing"], "oracle.contracts.clip_preprocessing"
    )
    validate_tensor_record(
        clip_preprocessing.get("pixels"), "oracle.contracts.clip_preprocessing.pixels"
    )
    search_nl = _require_mapping(contracts["search_nl"], "oracle.contracts.search_nl")
    if not search_nl:
        raise ValueError("oracle.contracts.search_nl must not be empty")
    for case_id, case in search_nl.items():
        case = _require_mapping(case, f"oracle.contracts.search_nl.{case_id}")
        if not isinstance(case.get("requested_n_results"), int):
            raise ValueError(f"oracle.contracts.search_nl.{case_id} lacks n_results")
        if not isinstance(case.get("requested_min_similarity"), (int, float)):
            raise ValueError(
                f"oracle.contracts.search_nl.{case_id} lacks min_similarity"
            )
        if not isinstance(case.get("results"), list):
            raise ValueError(f"oracle.contracts.search_nl.{case_id}.results must be a list")

    models = _require_mapping(oracle["models"], "oracle.models")
    expected_models = {"clip", "minilm", "bge", "reranker"}
    if set(models) != expected_models:
        raise ValueError("oracle.models must contain clip, minilm, bge, and reranker")
    model_specs = {
        "clip": {
            "fingerprints": 3,
            "tensors": ("text_embeddings", "image_embeddings"),
            "required": ("providers", "image_output_name", "text_output_name"),
        },
        "minilm": {
            "fingerprints": 2,
            "tensors": ("embeddings",),
            "required": ("providers", "output_names"),
        },
        "bge": {
            "fingerprints": 2,
            "tensors": ("embeddings",),
            "required": ("providers", "output_names"),
        },
        "reranker": {
            "fingerprints": 2,
            "tensors": ("raw_logits",),
            "required": ("provider", "output_name", "variant"),
        },
    }
    expected_token_names = {"input_ids", "attention_mask", "token_type_ids"}
    for model_name, spec in model_specs.items():
        model = _require_mapping(models[model_name], f"oracle.models.{model_name}")
        if not isinstance(model.get("model_id"), str) or not model["model_id"]:
            raise ValueError(f"oracle.models.{model_name}.model_id must be non-empty")
        contract = _require_mapping(
            model.get("contract"), f"oracle.models.{model_name}.contract"
        )
        if not contract:
            raise ValueError(f"oracle.models.{model_name}.contract must not be empty")
        _require_nonempty_strings(
            model.get("input_names"), f"oracle.models.{model_name}.input_names"
        )

        fingerprints = model.get("fingerprints")
        if not isinstance(fingerprints, list) or len(fingerprints) != spec["fingerprints"]:
            raise ValueError(
                f"oracle.models.{model_name}.fingerprints has the wrong size"
            )
        for index, fingerprint in enumerate(fingerprints):
            fingerprint = _require_mapping(
                fingerprint, f"oracle.models.{model_name}.fingerprints[{index}]"
            )
            if not isinstance(fingerprint.get("path"), str) or not fingerprint["path"]:
                raise ValueError("model fingerprint path must be non-empty")
            if not isinstance(fingerprint.get("size"), int) or fingerprint["size"] <= 0:
                raise ValueError("model fingerprint size must be positive")
            _require_sha256(
                fingerprint.get("sha256"),
                f"oracle.models.{model_name}.fingerprints[{index}].sha256",
            )

        tokenization = _require_mapping(
            model.get("tokenization"), f"oracle.models.{model_name}.tokenization"
        )
        if set(tokenization) != expected_token_names:
            raise ValueError(
                f"oracle.models.{model_name}.tokenization has unexpected tensors"
            )
        for tensor_name in sorted(expected_token_names):
            validate_tensor_record(
                tokenization[tensor_name],
                f"oracle.models.{model_name}.tokenization.{tensor_name}",
            )
            if tokenization[tensor_name]["comparison"] != "exact":
                raise ValueError("token tensors must use exact comparison")

        for tensor_name in spec["tensors"]:
            validate_tensor_record(
                model.get(tensor_name),
                f"oracle.models.{model_name}.{tensor_name}",
            )
        for field in spec["required"]:
            value = model.get(field)
            if field in {"providers", "output_names"}:
                _require_nonempty_strings(value, f"oracle.models.{model_name}.{field}")
            elif not isinstance(value, str) or not value:
                raise ValueError(f"oracle.models.{model_name}.{field} must be non-empty")


def _default_model_root() -> Path:
    configured = os.environ.get("CARBONPAPER_ORACLE_MODEL_ROOT")
    if configured:
        return Path(configured)
    local_appdata = os.environ.get("LOCALAPPDATA")
    if not local_appdata:
        raise RuntimeError("LOCALAPPDATA is required to locate installed models")
    return Path(local_appdata) / "carbonpaper" / "models"


def _write_json(path: Path, value: Any) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("w", encoding="utf-8", newline="\n") as output_file:
        json.dump(value, output_file, ensure_ascii=False, indent=2, sort_keys=True)
        output_file.write("\n")


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("generate", "validate"))
    parser.add_argument("--fixtures", type=Path, default=DEFAULT_FIXTURES)
    parser.add_argument("--golden", type=Path, default=DEFAULT_GOLDEN)
    parser.add_argument("--model-root", type=Path, default=None)
    args = parser.parse_args(argv)

    fixtures = load_fixtures(args.fixtures)
    model_root = args.model_root or _default_model_root()
    actual = generate_oracle(fixtures, model_root)
    validate_oracle_structure(actual)
    if args.command == "generate":
        _write_json(args.golden, actual)
        print(f"Wrote migration oracle: {args.golden}")
        return 0

    with args.golden.open("r", encoding="utf-8") as golden_file:
        expected = json.load(golden_file)
    validate_oracle_structure(expected)
    compare_oracles(expected, actual)
    print(f"Migration oracle matches: {args.golden}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
