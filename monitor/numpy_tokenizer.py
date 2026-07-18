"""Torch-free tokenizer adapter for ONNX inference paths."""

from __future__ import annotations

import os
import json
import threading
from typing import Iterable, Optional

import numpy as np
from tokenizers import Tokenizer


class NumpyTokenizer:
    """Small HuggingFace-tokenizers adapter returning NumPy tensors.

    The high-level ``transformers.AutoTokenizer`` imports PyTorch in the
    currently pinned dependency set even when ``return_tensors='np'``. ONNX
    workers use this adapter so tokenization cannot pull Torch DLLs into the
    process.
    """

    def __init__(self, model_dir: str):
        tokenizer_path = os.path.join(model_dir, "tokenizer.json")
        if not os.path.isfile(tokenizer_path):
            raise FileNotFoundError(f"tokenizer.json not found at {tokenizer_path}")
        self._tokenizer = Tokenizer.from_file(tokenizer_path)
        self._lock = threading.Lock()
        self._pad_token = "[PAD]"
        tokenizer_config_path = os.path.join(model_dir, "tokenizer_config.json")
        if os.path.isfile(tokenizer_config_path):
            with open(tokenizer_config_path, "r", encoding="utf-8") as config_file:
                tokenizer_config = json.load(config_file)
            configured_pad = tokenizer_config.get("pad_token")
            if isinstance(configured_pad, str) and configured_pad:
                self._pad_token = configured_pad
        pad_id = self._tokenizer.token_to_id(self._pad_token)
        self._pad_id = 0 if pad_id is None else pad_id

    def __call__(
        self,
        texts: Iterable,
        *,
        padding: bool = True,
        truncation: bool = False,
        max_length: Optional[int] = None,
        return_tensors: str = "np",
        **_kwargs,
    ) -> dict:
        if return_tensors != "np":
            raise ValueError("NumpyTokenizer only supports return_tensors='np'")
        inputs = list(texts)
        with self._lock:
            if truncation and max_length:
                self._tokenizer.enable_truncation(max_length=int(max_length))
            else:
                self._tokenizer.no_truncation()
            if padding:
                self._tokenizer.enable_padding(
                    pad_id=self._pad_id,
                    pad_token=self._pad_token,
                )
            else:
                self._tokenizer.no_padding()
            encodings = self._tokenizer.encode_batch(inputs, add_special_tokens=True)

        result = {
            "input_ids": np.asarray([encoding.ids for encoding in encodings], dtype=np.int64),
            "attention_mask": np.asarray(
                [encoding.attention_mask for encoding in encodings], dtype=np.int64
            ),
        }
        type_ids = np.asarray([encoding.type_ids for encoding in encodings], dtype=np.int64)
        if type_ids.size:
            result["token_type_ids"] = type_ids
        return result
