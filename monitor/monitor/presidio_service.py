"""Presidio PII detection service — lazy-loaded singleton.

Provides batch text analysis using Microsoft Presidio with
spaCy NLP backends.  Supports Chinese and English models (sm / trf),
switchable at runtime with automatic fallback.
"""

import gc
import hashlib
import importlib
import logging
import time
from functools import lru_cache
from typing import Dict, List, Optional, Tuple

logger = logging.getLogger(__name__)

# Entity types supported across both languages
ALL_ENTITY_TYPES = [
    "PERSON",
    "PHONE_NUMBER",
    "EMAIL_ADDRESS",
    "CN_ID_CARD",
    "CN_BANK_CARD",
    "ADDRESS",
    # English built-ins
    "CREDIT_CARD",
    "US_SSN",
    "IBAN_CODE",
    "IP_ADDRESS",
]

_MODEL_PREFERENCE = {
    "zh": ["zh_core_web_trf", "zh_core_web_sm"],
    "en": ["en_core_web_trf", "en_core_web_sm"],
}

# Entity types that have registered recognizers per language.
# Requesting types without a recognizer causes noisy warnings.
_LANG_ENTITIES = {
    "zh": {"PERSON", "PHONE_NUMBER", "EMAIL_ADDRESS", "CN_ID_CARD", "CN_BANK_CARD", "ADDRESS"},
    "en": {"PERSON", "PHONE_NUMBER", "EMAIL_ADDRESS", "CREDIT_CARD", "US_SSN", "IBAN_CODE", "IP_ADDRESS"},
}

# ── OCR text normalisation ────────────────────────────────

# Fullwidth digits U+FF10–FF19 → ASCII 0–9
_FULLWIDTH_DIGIT_TABLE = str.maketrans(
    "\uff10\uff11\uff12\uff13\uff14\uff15\uff16\uff17\uff18\uff19",
    "0123456789",
)

# Characters to strip when flanked by digits
_STRIP_BETWEEN_DIGITS = set(" -\u2013\u2014")  # space, hyphen, en-dash, em-dash


def normalize_ocr_text(text: str) -> Tuple[str, List[int]]:
    """Normalise OCR noise so regex recognizers can match digit sequences.

    Returns ``(normalized_text, norm_to_orig)`` where
    ``norm_to_orig[i]`` gives the position in the *original* text that
    corresponds to normalised position *i*.  Length is ``len(norm) + 1``
    (sentinel for ``end`` indices).
    """
    # Step 1: fullwidth → halfwidth (1:1, no length change)
    text = text.translate(_FULLWIDTH_DIGIT_TABLE)

    # Step 2: strip spaces/dashes between digits, building offset map
    out_chars: list[str] = []
    norm_to_orig: list[int] = []
    n = len(text)
    i = 0
    while i < n:
        ch = text[i]
        if ch in _STRIP_BETWEEN_DIGITS and i > 0 and i < n - 1:
            # Look back/forward for digits (skip consecutive strippable chars)
            left_is_digit = text[i - 1].isdigit()
            # Scan forward past consecutive strippable chars
            j = i
            while j < n and text[j] in _STRIP_BETWEEN_DIGITS:
                j += 1
            right_is_digit = j < n and text[j].isdigit()
            if left_is_digit and right_is_digit:
                # Skip all strippable chars between digits
                i = j
                continue
        out_chars.append(ch)
        norm_to_orig.append(i)
        i += 1

    # Sentinel: maps to one-past-end of original text
    norm_to_orig.append(n)

    return "".join(out_chars), norm_to_orig


def _remap_entities(
    entities: tuple,
    norm_to_orig: List[int],
) -> tuple:
    """Map entity start/end from normalised positions back to original text."""
    return tuple(
        {
            **ent,
            "start": norm_to_orig[ent["start"]],
            "end": norm_to_orig[ent["end"]],
        }
        for ent in entities
    )


class PresidioService:
    """Lazy-loaded Presidio PII detection service (singleton)."""

    _instance: Optional["PresidioService"] = None

    def __new__(cls):
        if cls._instance is None:
            cls._instance = super().__new__(cls)
            cls._instance._analyzer = None
            cls._instance._current_lang = None
            cls._instance._current_model = None
            cls._instance._initialized = False
            cls._instance._last_request_time = 0.0
            cls._instance._idle_timeout = 300  # 5 minutes
        return cls._instance

    @classmethod
    def get_instance(cls) -> "PresidioService":
        return cls()

    # ── initialisation ───────────────────────────────────

    def initialize(self, language: str) -> None:
        """Load spaCy model and create AnalyzerEngine for *language*.

        Tries models in preference order (trf first, then sm).

        Args:
            language: Frontend i18n code (``'zh-CN'``, ``'en'``, etc.).
        """
        lang_code = "zh" if language.startswith("zh") else "en"
        if self._analyzer and self._current_lang == lang_code:
            return  # already loaded for this language

        candidates = _MODEL_PREFERENCE.get(lang_code, ["en_core_web_sm"])
        model_name = None
        for candidate in candidates:
            try:
                importlib.import_module(candidate.replace("-", "_"))
                model_name = candidate
                break
            except ImportError:
                logger.debug("Presidio: model %s not available, trying next", candidate)

        if model_name is None:
            raise ImportError(
                f"No spaCy model available for lang={lang_code}. "
                f"Tried: {candidates}"
            )

        logger.info("Presidio: initializing for lang=%s model=%s", lang_code, model_name)

        try:
            from presidio_analyzer import AnalyzerEngine
            from presidio_analyzer.nlp_engine import SpacyNlpEngine, NlpEngineProvider

            # Build NLP engine config
            nlp_config = {
                "nlp_engine_name": "spacy",
                "models": [{"lang_code": lang_code, "model_name": model_name}],
            }
            provider = NlpEngineProvider(nlp_configuration=nlp_config)
            nlp_engine = provider.create_engine()

            # Create analyzer with default + custom recognizers
            self._analyzer = AnalyzerEngine(
                nlp_engine=nlp_engine,
                supported_languages=[lang_code],
            )

            # Register custom Chinese recognizers when using zh
            if lang_code == "zh":
                from .presidio_zh_recognizers import get_zh_recognizers
                for recognizer in get_zh_recognizers():
                    self._analyzer.registry.add_recognizer(recognizer)
                logger.info("Presidio: registered %d Chinese recognizers", 5)

            self._current_lang = lang_code
            self._current_model = model_name
            self._initialized = True
            self._last_request_time = time.monotonic()
            # Clear the LRU cache when language changes
            self._analyze_cached.cache_clear()
            logger.info("Presidio: ready (lang=%s)", lang_code)

        except ImportError as e:
            logger.error("Presidio: missing dependency — %s", e)
            raise
        except Exception as e:
            logger.error("Presidio: initialization failed — %s", e)
            raise

    # ── analysis ─────────────────────────────────────────

    def analyze(
        self,
        texts: List[str],
        entity_types: Optional[List[str]] = None,
    ) -> List[List[Dict]]:
        """Batch-analyze *texts* and return PII entities per text.

        Each text is normalised (fullwidth→halfwidth, strip inter-digit
        spaces) before analysis.  Returned start/end offsets reference the
        *original* text.

        Returns:
            ``[[{entity_type, start, end, score}, ...], ...]``
        """
        if not self._initialized or self._analyzer is None:
            raise RuntimeError("PresidioService not initialized; call initialize() first")

        self._last_request_time = time.monotonic()

        results: List[List[Dict]] = []
        for text in texts:
            norm_text, norm_to_orig = normalize_ocr_text(text)
            entities = self._analyze_cached(
                norm_text,
                tuple(entity_types) if entity_types else None,
            )
            entities = _remap_entities(entities, norm_to_orig)
            results.append(entities)
        return results

    @lru_cache(maxsize=512)
    def _analyze_cached(
        self,
        text: str,
        entity_types: Optional[tuple] = None,
    ) -> tuple:
        """Cached single-text analysis. Returns tuple of dicts for hashability."""
        try:
            # Filter entity types to those available for the current language
            # to avoid "doesn't have the corresponding recognizer" warnings.
            available = _LANG_ENTITIES.get(self._current_lang, set())
            if entity_types:
                filtered = [e for e in entity_types if e in available]
            else:
                filtered = list(available)

            results = self._analyzer.analyze(
                text=text,
                language=self._current_lang,
                entities=filtered if filtered else None,
                score_threshold=0.3,
            )
            return tuple(
                {
                    "entity_type": r.entity_type,
                    "start": r.start,
                    "end": r.end,
                    "score": round(r.score, 4),
                }
                for r in results
            )
        except Exception as e:
            logger.warning("Presidio analysis error: %s", e)
            return ()

    # ── language switching ───────────────────────────────

    def switch_language(self, language: str) -> None:
        """Hot-swap the spaCy model to match *language*."""
        lang_code = "zh" if language.startswith("zh") else "en"
        if self._current_lang == lang_code:
            return
        logger.info("Presidio: switching language from %s to %s", self._current_lang, lang_code)
        # Re-initialize with new language (old model will be GC'd)
        self._analyzer = None
        self._current_model = None
        self._initialized = False
        self.initialize(language)

    # ── unload / idle ─────────────────────────────────────

    def unload(self) -> None:
        """Release analyzer and spaCy model to free memory (~400MB for trf)."""
        if not self._initialized and self._analyzer is None:
            logger.debug("Presidio: already unloaded, skipping")
            return
        model = self._current_model or "unknown"
        logger.info("Presidio: unloading model %s", model)
        self._analyzer = None
        self._current_model = None
        self._initialized = False
        self._analyze_cached.cache_clear()
        gc.collect()
        logger.info("Presidio: model unloaded, gc.collect() done")

    def check_idle_and_unload(self) -> bool:
        """Unload if idle for longer than ``_idle_timeout`` seconds.

        Returns ``True`` if model was unloaded.
        """
        if not self._initialized:
            return False
        elapsed = time.monotonic() - self._last_request_time
        if elapsed >= self._idle_timeout:
            logger.info(
                "Presidio: idle for %.0fs (timeout=%ds), unloading",
                elapsed, self._idle_timeout,
            )
            self.unload()
            return True
        return False

    # ── status ───────────────────────────────────────────

    def get_status(self) -> Dict:
        """Return current service status."""
        return {
            "loaded": self._initialized,
            "language": self._current_lang,
            "model": self._current_model or "none",
        }
