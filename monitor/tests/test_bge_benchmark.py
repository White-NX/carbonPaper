import importlib.util
from pathlib import Path

import numpy as np

ROOT_DIR = Path(__file__).resolve().parents[2]
SCRIPT_PATH = ROOT_DIR / "tools" / "benchmark_bge.py"
SPEC = importlib.util.spec_from_file_location("benchmark_bge", SCRIPT_PATH)
benchmark_bge = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
SPEC.loader.exec_module(benchmark_bge)


def test_embedding_text_corpus_is_stable_deduplicated_and_bounds_ocr():
    long_ocr = "x" * 300
    samples = [
        {"window_title": " title ", "ocr_text": long_ocr},
        {"window_title": "title", "ocr_text": "short OCR"},
        {"window_title": "", "ocr_text": ""},
    ]

    texts = benchmark_bge._build_embedding_texts(samples)

    assert texts == ["title", "x" * 200, "short OCR"]


def test_embedding_comparison_enforces_cpu_gate():
    python_vectors = np.asarray([[1.0, 0.0], [0.0, 1.0]], dtype=np.float32)
    rust_vectors = python_vectors.copy()

    result = benchmark_bge.compare_embeddings(python_vectors, rust_vectors, "cpu")

    assert result["gate"]["passed"] is True
    assert result["min_cosine"] == 1.0
    assert result["max_abs_error"] == 0.0


def _classification_result(rows):
    return {
        "anchor_index_ms": 1.0,
        "anchor_count": 12,
        "latency_ms": benchmark_bge.summarize_values([2.0]),
        "rows": rows,
    }


def test_classification_comparison_reports_branch_and_threshold_crossings():
    python = _classification_result(
        [
            {
                "screenshot_id": 7,
                "category": "编程开发",
                "confidence": 0.3799,
                "confidence_rounded": 0.3799,
                "used_ocr": False,
                "local_veto_active": False,
                "top_order": ["编程开发", "网页浏览"],
            }
        ]
    )
    rust = _classification_result(
        [
            {
                "screenshot_id": 7,
                "category": "编程开发",
                "confidence": 0.3801,
                "confidence_rounded": 0.3801,
                "used_ocr": True,
                "local_veto_active": False,
                "top_order": ["编程开发", "网页浏览"],
            }
        ]
    )

    result = benchmark_bge.compare_classifications(python, rust)

    assert result["summary"]["category_agreement"] == 1.0
    assert result["summary"]["branch_agreement"] == 0.0
    assert result["summary"]["threshold_crossings"]["0.38"] == 1
    assert result["summary"]["passed"] is False
    assert "window_title" not in result["rows"][0]
    assert "ocr_text" not in result["rows"][0]


def test_classification_comparison_passes_identical_results():
    rows = [
        {
            "screenshot_id": 11,
            "category": "网页浏览",
            "confidence": 0.62,
            "confidence_rounded": 0.62,
            "used_ocr": False,
            "local_veto_active": True,
            "top_order": ["网页浏览", "阅读资讯"],
        }
    ]

    result = benchmark_bge.compare_classifications(
        _classification_result(rows), _classification_result([dict(rows[0])])
    )

    assert result["summary"]["passed"] is True
    assert result["summary"]["confidence_abs_delta"]["max"] == 0.0


def test_classification_batch_contract_accepts_multi_chunk_anchor_index():
    contract = benchmark_bge._classification_operational_contract(203)

    assert contract["classification_bridge_max_batch"] == 512
    assert contract["classification_bridge_chunk_size"] == 32
    assert contract["semantic_worker_max_batch"] == 32
    assert contract["required_worker_request_count"] == 7
    assert contract["max_worker_request_batch"] == 32
    assert contract["production_bridge_request_compatible"] is True
    assert contract["production_worker_chunk_compatible"] is True
    assert contract["production_batch_contract_compatible"] is True
    assert "production_single_request_compatible" not in contract


def test_rust_classifier_adapter_uses_production_worker_chunks(monkeypatch):
    request_sizes = []

    def fake_embed(_worker, texts):
        request_sizes.append(len(texts))
        return np.ones((len(texts), 2), dtype=np.float32), {}

    monkeypatch.setattr(benchmark_bge, "_rust_embed_timed", fake_embed)

    vectors = benchmark_bge.RustEmbedderAdapter(object()).encode(
        [f"anchor-{index}" for index in range(65)]
    )

    assert request_sizes == [32, 32, 1]
    assert vectors.shape == (65, 2)


def test_classification_batch_contract_rejects_bridge_overflow_and_chunk_drift(
    monkeypatch,
):
    overflow = benchmark_bge._classification_operational_contract(513)

    assert overflow["production_bridge_request_compatible"] is False
    assert overflow["production_worker_chunk_compatible"] is True
    assert overflow["production_batch_contract_compatible"] is False

    monkeypatch.setattr(benchmark_bge, "CLASSIFICATION_BRIDGE_CHUNK_SIZE", 33)
    drifted = benchmark_bge._classification_operational_contract(203)

    assert drifted["production_bridge_request_compatible"] is True
    assert drifted["production_worker_chunk_compatible"] is False
    assert drifted["production_batch_contract_compatible"] is False


def test_report_privacy_guard_rejects_plaintext_and_vector_fields():
    benchmark_bge.assert_report_has_no_plaintext_payloads(
        {"dataset": {"sample_ids": [1]}, "numeric": {"shape": [1, 512]}}
    )

    for field in ("window_title", "ocr_text", "process_name", "vectors", "embeddings"):
        try:
            benchmark_bge.assert_report_has_no_plaintext_payloads(
                {"nested": [{field: "sensitive"}]}
            )
        except ValueError as error:
            assert field in str(error)
        else:
            raise AssertionError(f"privacy guard accepted {field}")
