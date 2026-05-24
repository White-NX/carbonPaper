"""
Cross-encoder reranker for natural-language snapshot retrieval (demo).

Uses bge-reranker-v2-m3 in ONNX uint8 form (onnx-community/bge-reranker-v2-m3-ONNX).
Inference runs through onnxruntime-directml — same runtime that already powers
OCR — so it can transparently use the GPU when available without pulling in a
new dependency chain.

The reranker is a cross-encoder: it sees (query, document) jointly and emits a
single relevance logit. This is structurally different from the MiniLM
bi-encoder used for the first-stage retrieval, which is why it can disambiguate
cases like "神经网络训练" (ML) vs "神经" (neuroscience) that bi-encoder
similarity collapses.

Loaded lazily on first use; can be unloaded to reclaim ~600 MB.
"""

import os
import gc
import logging
import threading
from typing import List, Optional

import numpy as np

logger = logging.getLogger(__name__)


class RerankerNotAvailableError(Exception):
    """Raised when the reranker model files are not on disk."""
    pass


# Default candidate folder under LOCALAPPDATA/carbonpaper/models
DEFAULT_MODEL_DIRNAME = "bge-reranker-v2-m3"

# Tokenizer files required at the root of the model directory.
REQUIRED_TOKENIZER_FILES = [
    "config.json",
    "tokenizer.json",
    "tokenizer_config.json",
    "special_tokens_map.json",
]

# Supported ONNX variants (under <model_dir>/onnx/). Keep keys stable —
# the frontend / IPC layer uses these names.
ONNX_VARIANTS = {
    "fp16": os.path.join("onnx", "model_fp16.onnx"),
    "q4f16": os.path.join("onnx", "model_q4f16.onnx"),
    "int8": os.path.join("onnx", "model_quantized.onnx"),
    "uint8": os.path.join("onnx", "model_uint8.onnx"),
    "fp32": os.path.join("onnx", "model.onnx"),
}
DEFAULT_VARIANT = "uint8"


def _resolve_model_path() -> str:
    env = os.environ.get("RERANKER_MODEL_PATH")
    if env:
        return env
    return os.path.join(
        os.environ.get("LOCALAPPDATA", os.path.expanduser("~")),
        "carbonpaper",
        "models",
        DEFAULT_MODEL_DIRNAME,
    )


def _resolve_onnx_path(model_dir: str, variant: str = DEFAULT_VARIANT) -> str:
    rel = ONNX_VARIANTS.get(variant, ONNX_VARIANTS[DEFAULT_VARIANT])
    return os.path.join(model_dir, rel)


def list_available_variants() -> List[str]:
    """Return ONNX variants whose weight file exists on disk."""
    model_dir = _resolve_model_path()
    if not os.path.isdir(model_dir):
        return []
    out = []
    for name, rel in ONNX_VARIANTS.items():
        if os.path.isfile(os.path.join(model_dir, rel)):
            out.append(name)
    return out


class Reranker:
    """Singleton wrapper around bge-reranker-v2-m3 (ONNX uint8)."""

    _instance = None
    _lock = threading.RLock()

    def __new__(cls):
        with cls._lock:
            if cls._instance is None:
                cls._instance = super().__new__(cls)
                cls._instance._session = None
                cls._instance._tokenizer = None
                cls._instance._input_names = None
                cls._instance._output_name = None
                cls._instance._provider = None
                cls._instance._loaded_variant = None
        return cls._instance

    # ---- availability ----------------------------------------------------

    @staticmethod
    def is_model_available(variant: Optional[str] = None) -> bool:
        """Check whether the tokenizer + the requested ONNX variant are on disk.

        If ``variant`` is None, returns True when *any* known variant is present.
        """
        model_dir = _resolve_model_path()
        if not os.path.isdir(model_dir):
            return False
        for f in REQUIRED_TOKENIZER_FILES:
            if not os.path.isfile(os.path.join(model_dir, f)):
                return False
        if variant is None:
            return any(
                os.path.isfile(os.path.join(model_dir, rel))
                for rel in ONNX_VARIANTS.values()
            )
        return os.path.isfile(_resolve_onnx_path(model_dir, variant))

    def is_loaded(self) -> bool:
        return self._session is not None

    @property
    def provider(self) -> Optional[str]:
        return self._provider

    @property
    def loaded_variant(self) -> Optional[str]:
        return self._loaded_variant

    # ---- lifecycle -------------------------------------------------------

    def load(self, variant: str = DEFAULT_VARIANT):
        """Load (or hot-swap to) a specific ONNX variant.

        If a different variant is already loaded, the old session is dropped
        before the new one is created — this keeps RAM bounded and lets the
        UI switch between fp16 / q4f16 / int8 for side-by-side comparison.
        """
        if variant not in ONNX_VARIANTS:
            raise ValueError(f"Unknown reranker variant: {variant!r}")

        if self._session is not None and self._loaded_variant == variant:
            return

        with self._lock:
            if self._session is not None and self._loaded_variant == variant:
                return

            if not self.is_model_available(variant):
                raise RerankerNotAvailableError(
                    f"Reranker variant '{variant}' not found at "
                    f"{_resolve_onnx_path(_resolve_model_path(), variant)}"
                )

            # Drop existing session before allocating the new one — important
            # for the GPU case where two large sessions could OOM the device.
            if self._session is not None:
                logger.info(
                    "Swapping reranker variant: %s → %s",
                    self._loaded_variant, variant,
                )
                self._session = None
                self._input_names = None
                self._output_name = None
                self._loaded_variant = None
                gc.collect()

            model_dir = _resolve_model_path()
            onnx_path = _resolve_onnx_path(model_dir, variant)

            # Tokenizer reused from transformers (already a dependency for
            # MiniLM / classifier). Fast tokenizer reads tokenizer.json
            # directly, no sentencepiece file needed. Loaded once per process.
            if self._tokenizer is None:
                from transformers import AutoTokenizer
                logger.info("Loading reranker tokenizer from %s ...", model_dir)
                self._tokenizer = AutoTokenizer.from_pretrained(
                    model_dir, local_files_only=True, use_fast=True
                )

            # Prefer DirectML for GPU acceleration on Windows; fall back to CPU.
            import onnxruntime as ort
            available = ort.get_available_providers()
            preferred = [p for p in ("DmlExecutionProvider", "CPUExecutionProvider") if p in available]
            if not preferred:
                preferred = available[:1] or ["CPUExecutionProvider"]

            sess_options = ort.SessionOptions()
            sess_options.graph_optimization_level = ort.GraphOptimizationLevel.ORT_ENABLE_ALL
            sess_options.inter_op_num_threads = 1

            from logging_config import log_model_loading
            log_model_loading(f"bge-reranker-v2-m3 ({variant})")
            logger.info(
                "Loading reranker ONNX variant=%s from %s (providers=%s) ...",
                variant, onnx_path, preferred,
            )
            self._session = ort.InferenceSession(
                onnx_path, sess_options=sess_options, providers=preferred
            )
            self._provider = self._session.get_providers()[0] if self._session.get_providers() else None
            self._input_names = [i.name for i in self._session.get_inputs()]
            self._output_name = self._session.get_outputs()[0].name
            self._loaded_variant = variant
            logger.info(
                "Reranker loaded (variant=%s, provider=%s, inputs=%s, output=%s)",
                variant, self._provider, self._input_names, self._output_name,
            )

    def unload(self):
        with self._lock:
            self._session = None
            self._tokenizer = None
            self._input_names = None
            self._output_name = None
            self._provider = None
            self._loaded_variant = None
        gc.collect()
        logger.info("Reranker unloaded — memory released")

    # ---- scoring ---------------------------------------------------------

    def rerank(
        self,
        query: str,
        documents: List[str],
        max_length: int = 512,
        batch_size: int = 8,
        variant: str = DEFAULT_VARIANT,
    ) -> List[float]:
        """Score (query, doc) pairs. Returns one float per document (higher = better).

        Scores are raw logits — comparable within a single call but not
        calibrated to [0, 1]. Apply sigmoid downstream if you need that.
        """
        if not documents:
            return []

        with self._lock:
            self.load(variant)
            session = self._session
            tokenizer = self._tokenizer
            input_names = self._input_names
            output_name = self._output_name

        if not session or not tokenizer:
            raise RerankerNotAvailableError("Reranker model session is not loaded.")

        scores: List[float] = []
        for start in range(0, len(documents), batch_size):
            batch_docs = documents[start:start + batch_size]
            pairs = [(query, d) for d in batch_docs]
            encoded = tokenizer(
                pairs,
                padding=True,
                truncation=True,
                max_length=max_length,
                return_tensors="np",
            )

            # XLM-RoBERTa-based models take input_ids + attention_mask only
            # (no token_type_ids). Filter to what the ONNX graph actually expects.
            feeds = {}
            for name in input_names:
                if name in encoded:
                    arr = encoded[name]
                    # ONNX models from onnx-community typically expect int64
                    if arr.dtype != np.int64:
                        arr = arr.astype(np.int64)
                    feeds[name] = arr
                else:
                    # Some exporters keep a token_type_ids input even though
                    # the underlying model ignores it; feed zeros to be safe.
                    if name == "token_type_ids":
                        feeds[name] = np.zeros_like(encoded["input_ids"], dtype=np.int64)

            outputs = session.run([output_name], feeds)
            logits = outputs[0]
            # Output shape is typically (batch, 1); flatten.
            logits = np.asarray(logits).reshape(-1).astype(np.float32)
            scores.extend(logits.tolist())
        return scores
