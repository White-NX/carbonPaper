import json

import numpy as np

import classifier


class FakeEmbedder:
    def encode(self, texts):
        return np.array([self._vector_for(text) for text in texts], dtype=np.float32)

    def encode_single(self, text):
        return self.encode([text])[0]

    @staticmethod
    def _vector_for(text):
        if "Python" in text or "编程" in text or "VS Code" in text:
            return [1.0, 0.0, 0.0, 0.0]
        if "电影" in text or "音乐" in text:
            return [0.0, 1.0, 0.0, 0.0]
        return [0.0, 0.0, 1.0, 0.0]


def _write_anchors(path):
    path.write_text(
        json.dumps(
            {
                "编程开发": [
                    {"text": "Python异步编程原理", "weight": 1.0, "scope": "global"}
                ],
                "影音娱乐": [
                    {"text": "电影剪辑混剪精彩片段", "weight": 1.0, "scope": "global"}
                ],
            },
            ensure_ascii=False,
        ),
        encoding="utf-8",
    )


def test_classify_builds_lazy_index_before_empty_index_guard(tmp_path, monkeypatch):
    monkeypatch.setattr(classifier, "TextEmbedder", FakeEmbedder)
    anchors_path = tmp_path / "anchors.json"
    _write_anchors(anchors_path)

    service = classifier.ClassificationService(str(anchors_path))
    assert service.anchor_matrix is None
    assert service._index_built is False

    category, confidence = service.classify("Python异步编程原理")

    assert category == "编程开发"
    assert confidence >= service.CLASSIFY_MIN_THRESHOLD
    assert service.anchor_matrix is not None
    assert service._index_built is True


def test_classify_debug_builds_lazy_index_before_empty_index_guard(tmp_path, monkeypatch):
    monkeypatch.setattr(classifier, "TextEmbedder", FakeEmbedder)
    anchors_path = tmp_path / "anchors.json"
    _write_anchors(anchors_path)

    service = classifier.ClassificationService(str(anchors_path))
    assert service.anchor_matrix is None

    debug = service.classify_debug("Python异步编程原理")

    assert debug["category"] == "编程开发"
    assert debug.get("reason") != "empty_anchor_index"
    assert service.anchor_matrix is not None
