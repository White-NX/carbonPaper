import json

import numpy as np
from tokenizers import Tokenizer
from tokenizers.models import WordLevel
from tokenizers.pre_tokenizers import Whitespace
from tokenizers.processors import TemplateProcessing

from numpy_tokenizer import NumpyTokenizer


def _write_tokenizer(tmp_path):
    tokenizer = Tokenizer(
        WordLevel(
            vocab={"<pad>": 1, "<s>": 0, "</s>": 2, "hello": 3, "world": 4},
            unk_token="<pad>",
        )
    )
    tokenizer.pre_tokenizer = Whitespace()
    tokenizer.post_processor = TemplateProcessing(
        single="<s> $A </s>",
        pair="<s> $A </s> </s> $B </s>",
        special_tokens=[("<s>", 0), ("</s>", 2)],
    )
    tokenizer.save(str(tmp_path / "tokenizer.json"))
    (tmp_path / "tokenizer_config.json").write_text(
        json.dumps({"pad_token": "<pad>"}), encoding="utf-8"
    )


def test_numpy_tokenizer_uses_configured_padding_and_numpy_output(tmp_path):
    _write_tokenizer(tmp_path)
    encoded = NumpyTokenizer(str(tmp_path))(
        ["hello world", "hello"], padding=True, return_tensors="np"
    )

    assert encoded["input_ids"].dtype == np.int64
    assert encoded["input_ids"].tolist() == [[0, 3, 4, 2], [0, 3, 2, 1]]
    assert encoded["attention_mask"].tolist() == [[1, 1, 1, 1], [1, 1, 1, 0]]


def test_numpy_tokenizer_supports_sentence_pairs_and_truncation(tmp_path):
    _write_tokenizer(tmp_path)
    encoded = NumpyTokenizer(str(tmp_path))(
        [("hello world", "hello world")],
        padding=True,
        truncation=True,
        max_length=7,
        return_tensors="np",
    )

    assert encoded["input_ids"].shape == (1, 7)
    assert encoded["attention_mask"].sum() == 7
