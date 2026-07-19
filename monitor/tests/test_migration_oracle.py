import copy
import json
import os
import sys
from pathlib import Path

import numpy as np
import pytest

TOOLS_DIR = Path(__file__).resolve().parents[2] / "tools"
if str(TOOLS_DIR) not in sys.path:
    sys.path.insert(0, str(TOOLS_DIR))

from migration_oracle import (
    CPU_TOLERANCE_PROFILE,
    DEFAULT_FIXTURES,
    DEFAULT_GOLDEN,
    DIRECTML_TOLERANCE_PROFILE,
    EMBEDDING_TOLERANCES,
    RERANKER_LOGIT_TOLERANCES,
    TARGET_RELEASE,
    build_static_contracts,
    compare_oracles,
    fixture_sha256,
    generate_oracle,
    load_fixtures,
    tensor_from_record,
    tensor_record,
    validate_oracle_structure,
)


def _load_golden():
    with DEFAULT_GOLDEN.open("r", encoding="utf-8") as golden_file:
        return json.load(golden_file)


def test_fixture_and_golden_target_v084_beta():
    fixtures = load_fixtures(DEFAULT_FIXTURES)
    golden = _load_golden()

    assert fixtures["target_release"] == TARGET_RELEASE
    assert golden["target_release"] == TARGET_RELEASE
    assert golden["fixture_sha256"] == fixture_sha256(fixtures)
    assert set(golden["models"]) == {"clip", "minilm", "bge", "reranker"}
    validate_oracle_structure(golden)


def test_static_python_contracts_match_committed_oracle():
    fixtures = load_fixtures(DEFAULT_FIXTURES)
    golden = _load_golden()

    actual_contracts = build_static_contracts(fixtures)
    compare_oracles(golden["contracts"], actual_contracts, "contracts")

    assert actual_contracts["clip_vector_search"]["min_similarity"] == 0.32
    for search_case in actual_contracts["search_nl"].values():
        assert search_case["requested_min_similarity"] == 0.32

    truncation_case = next(
        case for case in fixtures["minilm"]["cases"] if case["id"] == "ocr_truncation"
    )
    task_text = next(
        case["text"]
        for case in golden["contracts"]["minilm_task_texts"]
        if case["id"] == "ocr_truncation"
    )
    assert len(truncation_case["ocr_text"]) > 200
    assert task_text.endswith(truncation_case["ocr_text"][:200].strip())
    assert "TAIL_MARKER_SHOULD_NOT_APPEAR" not in task_text


def test_oracle_tensor_record_round_trip_and_checksum():
    record = tensor_record(
        np.asarray([[1.0, 2.0], [3.0, 4.0]], dtype="float32"),
        comparison="allclose",
        max_abs_error=1e-6,
    )

    assert tensor_from_record(record).tolist() == [[1.0, 2.0], [3.0, 4.0]]

    record["sha256"] = "0" * 64
    with pytest.raises(ValueError, match="checksum"):
        tensor_from_record(record)


def test_oracle_comparison_rejects_non_finite_values_and_dtype_mismatches():
    expected = tensor_record(
        np.asarray([[1.0, 0.0]], dtype="float32"),
        comparison="cosine",
        min_cosine=0.99999,
        max_abs_error=1e-4,
    )
    non_finite = tensor_record(
        np.asarray([[np.nan, 0.0]], dtype="float32"),
        comparison="cosine",
        min_cosine=0.99999,
        max_abs_error=1e-4,
    )

    with pytest.raises(AssertionError, match="non-finite"):
        compare_oracles(expected, non_finite)

    expected_tokens = tensor_record(
        np.asarray([1, 2], dtype="int64"), comparison="exact"
    )
    wrong_dtype_tokens = tensor_record(
        np.asarray([1, 2], dtype="int32"), comparison="exact"
    )

    with pytest.raises(AssertionError, match="dtype mismatch"):
        compare_oracles(expected_tokens, wrong_dtype_tokens)


def test_oracle_comparison_selects_provider_specific_tolerances():
    expected = tensor_record(
        np.asarray([[1.0, 0.0]], dtype="float32"),
        comparison="cosine",
        tolerances=EMBEDDING_TOLERANCES,
    )
    directml_acceptable = tensor_record(
        np.asarray([[0.9995, 0.0005]], dtype="float32"),
        comparison="cosine",
        tolerances=EMBEDDING_TOLERANCES,
    )

    with pytest.raises(AssertionError, match="max abs error"):
        compare_oracles(
            expected,
            directml_acceptable,
            tolerance_profile=CPU_TOLERANCE_PROFILE,
        )
    compare_oracles(
        expected,
        directml_acceptable,
        tolerance_profile=DIRECTML_TOLERANCE_PROFILE,
    )


def test_golden_token_tensors_are_exact_and_embeddings_have_release_gates():
    golden = _load_golden()

    for model in golden["models"].values():
        for token_tensor in model["tokenization"].values():
            assert token_tensor["comparison"] == "exact"
            tensor_from_record(token_tensor)

    for model_name, field in (
        ("clip", "text_embeddings"),
        ("clip", "image_embeddings"),
        ("minilm", "embeddings"),
        ("bge", "embeddings"),
    ):
        record = golden["models"][model_name][field]
        assert record["comparison"] == "cosine"
        assert record["tolerances"] == EMBEDDING_TOLERANCES
        tensor_from_record(record)

    assert golden["models"]["reranker"]["raw_logits"]["comparison"] == "allclose"
    assert (
        golden["models"]["reranker"]["raw_logits"]["tolerances"]
        == RERANKER_LOGIT_TOLERANCES
    )


def test_golden_structure_rejects_missing_tokenization_and_fingerprints():
    golden = _load_golden()

    missing_tokens = copy.deepcopy(golden)
    missing_tokens["models"]["clip"]["tokenization"] = {}
    with pytest.raises(ValueError, match="tokenization"):
        validate_oracle_structure(missing_tokens)

    missing_fingerprints = copy.deepcopy(golden)
    missing_fingerprints["models"]["minilm"]["fingerprints"] = []
    with pytest.raises(ValueError, match="fingerprints"):
        validate_oracle_structure(missing_fingerprints)


@pytest.mark.skipif(
    os.environ.get("CARBONPAPER_VALIDATE_MIGRATION_ORACLE") != "1",
    reason="large installed ONNX models are opt-in",
)
def test_installed_models_match_committed_oracle():
    fixtures = load_fixtures(DEFAULT_FIXTURES)
    golden = _load_golden()
    model_root = Path(os.environ["CARBONPAPER_ORACLE_MODEL_ROOT"])

    compare_oracles(golden, generate_oracle(fixtures, model_root))
