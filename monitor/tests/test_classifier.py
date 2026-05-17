"""Tests for classifier process-prior bonus and OCR local-veto gate.

Covers the behavior added in commit e493793:
- ``ClassificationService._apply_process_prior`` and the
  ``PROCESS_CATEGORY_PRIOR`` table.
- The OCR-fallback veto gate (``LOCAL_VETO_THRESHOLD``) inside
  ``classify()`` and ``classify_debug()``.
- The intentional behavior that the process-prior bonus *can* rescue a
  blended score that would otherwise fall below
  ``CLASSIFY_MIN_THRESHOLD``.

Mocking strategy: the BGE text embedder is replaced by a lightweight
fake, and ``_score_embedding`` is monkey-patched on the service
instance to return per-call canned scores. This makes it possible to
engineer precise scenarios for the gate logic without depending on
real embedding math.
"""

import json
import sys
from pathlib import Path
from unittest import mock

import numpy as np
import pytest

ROOT_DIR = Path(__file__).resolve().parents[2]
MONITOR_DIR = ROOT_DIR / "monitor"
if str(MONITOR_DIR) not in sys.path:
    sys.path.insert(0, str(MONITOR_DIR))

import classifier  # noqa: E402


# The exact bonus magnitude is hard-coded here rather than imported, so this
# file works whether PROCESS_PRIOR_BONUS lives at module level or as a class
# attribute on ClassificationService.
EXPECTED_BONUS = 0.12


class _FakeEmbedder:
    """Returns a constant unit vector — enough for ``_build_index`` to populate
    ``anchor_matrix`` and for ``self.embedder.encode_single(...)`` to succeed.
    The actual vectors don't matter because the tests below also stub
    ``_score_embedding``."""

    def encode(self, texts):
        return np.array([[1.0, 0.0, 0.0, 0.0]] * len(texts), dtype=np.float32)

    def encode_single(self, text):
        return self.encode([text])[0]


def _write_min_anchors(path: Path) -> None:
    """Write a tiny anchors.json so ``_build_index`` has data to chew on
    without pulling in the full default-anchor catalogue."""
    path.write_text(
        json.dumps(
            {
                "编程开发": [{"text": "code", "weight": 1.0, "scope": "global"}],
                "社交通讯": [{"text": "chat", "weight": 1.0, "scope": "global"}],
                "影音娱乐": [{"text": "video", "weight": 1.0, "scope": "global"}],
            },
            ensure_ascii=False,
        ),
        encoding="utf-8",
    )


@pytest.fixture
def stub_service(tmp_path, monkeypatch):
    """A ClassificationService that doesn't load the BGE model.

    - ``TextEmbedder`` is swapped for ``_FakeEmbedder``.
    - ``_ensure_default_global_anchors`` is no-op'd so the minimal anchors
      file we wrote isn't backfilled with the production defaults.
    - ``_ensure_index()`` is forced so the test fixture is ready to use.
    """
    monkeypatch.setattr(classifier, "TextEmbedder", _FakeEmbedder)
    monkeypatch.setattr(
        classifier.ClassificationService,
        "_ensure_default_global_anchors",
        lambda self: None,
    )
    anchors_path = tmp_path / "anchors.json"
    _write_min_anchors(anchors_path)
    svc = classifier.ClassificationService(str(anchors_path))
    svc._ensure_index()
    return svc


def _scores(p=0.0, s=0.0, v=0.0):
    """{category: score} dict in the order written by ``_write_min_anchors``."""
    return {"编程开发": p, "社交通讯": s, "影音娱乐": v}


# ===========================================================================
# _apply_process_prior — pure unit tests
# ===========================================================================


class TestApplyProcessPrior:
    def test_adds_bonus_to_mapped_category(self):
        scores = {"社交通讯": 0.40, "编程开发": 0.50}
        out = classifier.ClassificationService._apply_process_prior(scores, "qq.exe")
        assert out["社交通讯"] == pytest.approx(0.40 + EXPECTED_BONUS)
        assert out["编程开发"] == 0.50

    def test_no_op_when_process_unknown(self):
        scores = {"社交通讯": 0.40, "编程开发": 0.50}
        out = classifier.ClassificationService._apply_process_prior(scores, "unknownproc.exe")
        assert out == scores

    def test_no_op_when_mapped_category_missing(self):
        # qq.exe maps to 社交通讯, but if that category is absent the bonus
        # should not appear out of nowhere.
        scores = {"编程开发": 0.50}
        out = classifier.ClassificationService._apply_process_prior(scores, "qq.exe")
        assert out == scores

    def test_returns_new_dict_does_not_mutate_input(self):
        scores = {"社交通讯": 0.40}
        original = dict(scores)
        out = classifier.ClassificationService._apply_process_prior(scores, "qq.exe")
        assert out is not scores
        assert scores == original

    def test_case_insensitive_process_name(self):
        scores = {"社交通讯": 0.40}
        out = classifier.ClassificationService._apply_process_prior(scores, "QQ.EXE")
        assert out["社交通讯"] == pytest.approx(0.40 + EXPECTED_BONUS)

    def test_strips_whitespace_in_process_name(self):
        scores = {"社交通讯": 0.40}
        out = classifier.ClassificationService._apply_process_prior(scores, "  qq.exe  ")
        assert out["社交通讯"] == pytest.approx(0.40 + EXPECTED_BONUS)

    def test_handles_empty_process_name(self):
        scores = {"社交通讯": 0.40}
        out = classifier.ClassificationService._apply_process_prior(scores, "")
        assert out == scores

    def test_handles_none_process_name(self):
        scores = {"社交通讯": 0.40}
        out = classifier.ClassificationService._apply_process_prior(scores, None)
        assert out == scores


# ===========================================================================
# PROCESS_CATEGORY_PRIOR table sanity
# ===========================================================================


class TestProcessCategoryPriorTable:
    def test_all_keys_are_lowercase(self):
        for proc in classifier.PROCESS_CATEGORY_PRIOR:
            assert proc == proc.lower(), f"Process key not lowercase: {proc!r}"

    def test_all_keys_end_with_exe(self):
        for proc in classifier.PROCESS_CATEGORY_PRIOR:
            assert proc.endswith(".exe"), f"Process key missing .exe suffix: {proc!r}"

    def test_all_values_are_non_empty_strings(self):
        for proc, cat in classifier.PROCESS_CATEGORY_PRIOR.items():
            assert isinstance(cat, str) and cat, f"Bad value for {proc!r}: {cat!r}"

    def test_all_mapped_categories_are_known(self):
        """Every mapped category must appear in DEFAULT_ANCHORS — otherwise the
        bonus silently no-ops because the category isn't in the scoring dict."""
        for proc, cat in classifier.PROCESS_CATEGORY_PRIOR.items():
            assert cat in classifier.DEFAULT_ANCHORS, (
                f"{proc!r} maps to unknown category {cat!r} "
                f"(not in DEFAULT_ANCHORS)"
            )


# ===========================================================================
# OCR local-veto gate
# ===========================================================================


class TestLocalVetoGate:
    def test_strong_local_agreement_skips_ocr_blend(self, stub_service):
        """Local channel ≥ LOCAL_VETO_THRESHOLD and matches blended winner →
        OCR fallback is skipped (only the 2 title-channel score calls run)."""
        # diversity=1 → boost=LOCAL_BOOST_MONO=0.15
        # blended[编程开发] = 0.30 + 0.15*0.6 = 0.39  (< TITLE_CONF 0.55, gate fires)
        # local_best = 编程开发 == blended_best, local_best_score = 0.6 ≥ 0.5 → veto
        title_local = _scores(p=0.6)
        title_global = _scores(p=0.30)
        # If OCR were blended, it would tilt the winner to 影音娱乐 — must NOT happen.
        ocr_local = _scores(v=0.7)
        ocr_global = _scores(v=0.5)

        mock_score = mock.MagicMock(
            side_effect=[title_local, title_global, ocr_local, ocr_global]
        )
        stub_service._score_embedding = mock_score

        category, _ = stub_service.classify("dummy title", ocr_text="dummy ocr", process_name="")

        assert mock_score.call_count == 2, (
            f"OCR should be vetoed; got {mock_score.call_count} score calls (expected 2)"
        )
        assert category == "编程开发"

    def test_local_disagrees_lets_ocr_proceed(self, stub_service):
        """local_best != blended_best → veto NOT active, OCR blend proceeds (4 calls)."""
        # title_local: 社交通讯=0.65 (strong but NOT for the blended winner)
        # title_global: 编程开发=0.45 (drives blended winner = 编程开发)
        title_local = _scores(s=0.65)
        title_global = _scores(p=0.45)
        ocr_local = _scores(v=0.0)
        ocr_global = _scores(v=0.0)

        mock_score = mock.MagicMock(
            side_effect=[title_local, title_global, ocr_local, ocr_global]
        )
        stub_service._score_embedding = mock_score

        stub_service.classify("dummy", ocr_text="ocr text", process_name="")
        assert mock_score.call_count == 4

    def test_local_below_threshold_lets_ocr_proceed(self, stub_service):
        """Local-best matches blended_best but local-best-score < LOCAL_VETO_THRESHOLD →
        veto inactive, OCR blend proceeds (4 calls)."""
        # title_local: 编程开发=0.40 (below 0.5 veto threshold)
        title_local = _scores(p=0.40)
        title_global = _scores(p=0.30)
        ocr_local = _scores(v=0.0)
        ocr_global = _scores(v=0.0)

        mock_score = mock.MagicMock(
            side_effect=[title_local, title_global, ocr_local, ocr_global]
        )
        stub_service._score_embedding = mock_score

        stub_service.classify("dummy", ocr_text="ocr text", process_name="")
        assert mock_score.call_count == 4

    def test_empty_ocr_text_skips_blend_even_without_veto(self, stub_service):
        """OCR blend is also gated on ``ocr_text`` being non-empty — sanity check
        that absent OCR doesn't get past the gate independently of the veto."""
        title_local = _scores()
        title_global = _scores(p=0.30)
        mock_score = mock.MagicMock(side_effect=[title_local, title_global])
        stub_service._score_embedding = mock_score

        stub_service.classify("dummy", ocr_text="", process_name="")
        assert mock_score.call_count == 2


# ===========================================================================
# Process-prior threshold rescue (documented intentional behavior)
# ===========================================================================


class TestProcessPriorRescue:
    def test_prior_rescues_score_below_classify_min(self, stub_service):
        """A blended score below CLASSIFY_MIN_THRESHOLD is lifted above it by
        the prior bonus when the process is in PROCESS_CATEGORY_PRIOR. This is
        intentional — see the review discussion on M5/M6 in the PR review."""
        # blended[社交通讯] = 0.27 (< CLASSIFY_MIN 0.38)
        # qq.exe → +0.12 → 0.39 ≥ 0.38 → category set
        stub_service._score_embedding = mock.MagicMock(
            side_effect=lambda emb, process_name="", channel="all", include_debug=False: (
                _scores(s=0.27) if channel == "global" else _scores()
            )
        )

        category, score = stub_service.classify(
            "dummy", ocr_text="", process_name="qq.exe"
        )
        assert category == "社交通讯"
        assert score == pytest.approx(0.27 + EXPECTED_BONUS)

    def test_prior_does_not_rescue_when_score_plus_bonus_still_below_min(self, stub_service):
        """If score + bonus is still under CLASSIFY_MIN, returns 未分类."""
        # blended[社交通讯] = 0.20 (+0.12 = 0.32 < 0.38)
        stub_service._score_embedding = mock.MagicMock(
            side_effect=lambda emb, process_name="", channel="all", include_debug=False: (
                _scores(s=0.20, p=0.20) if channel == "global" else _scores()
            )
        )

        category, _ = stub_service.classify(
            "dummy", ocr_text="", process_name="qq.exe"
        )
        assert category == "未分类"

    def test_prior_with_unknown_process_does_not_modify_outcome(self, stub_service):
        """An unknown process name → no bonus, blended winner stands."""
        stub_service._score_embedding = mock.MagicMock(
            side_effect=lambda emb, process_name="", channel="all", include_debug=False: (
                _scores(p=0.50) if channel == "global" else _scores()
            )
        )

        category, score = stub_service.classify(
            "dummy", ocr_text="", process_name="unknown.exe"
        )
        assert category == "编程开发"
        assert score == pytest.approx(0.50)

    def test_prior_in_classify_debug_records_applied_category(self, stub_service):
        """classify_debug exposes the applied prior category in its debug payload."""
        stub_service._score_embedding = mock.MagicMock(
            side_effect=lambda emb, process_name="", channel="all", include_debug=False: (
                _scores(s=0.50) if channel == "global" else _scores()
            )
        )

        debug = stub_service.classify_debug(
            "dummy", ocr_text="", process_name="qq.exe"
        )
        assert debug["category"] == "社交通讯"
        assert debug["process_prior_applied"] == "社交通讯"

    def test_prior_in_classify_debug_records_unknown_as_none(self, stub_service):
        """classify_debug records None for processes not in the prior map."""
        stub_service._score_embedding = mock.MagicMock(
            side_effect=lambda emb, process_name="", channel="all", include_debug=False: (
                _scores(p=0.50) if channel == "global" else _scores()
            )
        )

        debug = stub_service.classify_debug(
            "dummy", ocr_text="", process_name="unknown.exe"
        )
        assert debug["process_prior_applied"] is None
