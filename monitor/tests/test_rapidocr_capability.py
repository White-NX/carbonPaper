from pathlib import Path

import rapidocr_capability


class _FakeRapidOCR:
    def __init__(self, params=None):
        self.params = params or {}


def test_bundled_ppocrv5_paths_are_forwarded(monkeypatch, tmp_path: Path):
    monkeypatch.setattr(rapidocr_capability, "RapidOCR", _FakeRapidOCR)
    det = tmp_path / "det.onnx"
    rec = tmp_path / "rec.onnx"
    keys = tmp_path / "dict.txt"

    wrapper = rapidocr_capability.PaddleOCR(
        use_angle_cls=False,
        ocr_version="PP-OCRv5",
        det_model_path=str(det),
        rec_model_path=str(rec),
        rec_keys_path=str(keys),
    )

    params = wrapper.engine.params
    assert params["Global.use_cls"] is False
    assert params["Det.model_path"] == str(det)
    assert params["Rec.model_path"] == str(rec)
    assert params["Rec.rec_keys_path"] == str(keys)
    assert params["Det.ocr_version"].value == "PP-OCRv5"
    assert params["Rec.ocr_version"].value == "PP-OCRv5"


def test_directml_options_remain_compatible_with_bundled_models(monkeypatch):
    monkeypatch.setattr(rapidocr_capability, "RapidOCR", _FakeRapidOCR)
    wrapper = rapidocr_capability.PaddleOCR(
        use_dml=True,
        det_model_path="det.onnx",
        rec_model_path="rec.onnx",
        rec_keys_path="dict.txt",
    )

    assert wrapper.engine.params["EngineConfig.onnxruntime.use_dml"] is True
