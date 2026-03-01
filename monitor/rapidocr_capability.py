"""
RapidOCR to PaddleOCR 3.3 Compatibility Layer
"""

from rapidocr import RapidOCR
from typing import Union, List, Optional
import numpy as np
from PIL import Image
import logging

logger = logging.getLogger(__name__)


def _patch_ort_dml_device_id(device_id: int):
    """
    Before RapidOCR creates a session, monkey-patch onnxruntime.InferenceSession
    so that it injects the specified device_id when adding a DmlExecutionProvider.

    RapidOCR internally passes the 'DmlExecutionProvider' string directly to the providers list,
    but does not support custom provider options, so it can only intercept at a lower level.
    """
    try:
        import onnxruntime as ort
    except ImportError:
        logger.warning("onnxruntime not available, cannot patch DML device_id")
        return

    if getattr(ort.InferenceSession, "_dml_device_patched", False):
        return  # Already patched

    _original_init = ort.InferenceSession.__init__

    def _patched_init(self, *args, **kwargs):
        providers = kwargs.get("providers", None)
        if not providers and len(args) >= 2:
            # providers may be passed as a positional arg (path_or_bytes, providers, ...)
            # InferenceSession(path, providers=...) or InferenceSession(path, sess_options, providers)
            pass  # Only handle the kwargs form; positional args are rare

        if providers:
            new_providers = []
            for p in providers:
                if isinstance(p, str) and p == "DmlExecutionProvider":
                    new_providers.append(
                        ("DmlExecutionProvider", {"device_id": device_id})
                    )
                elif (
                    isinstance(p, tuple)
                    and len(p) >= 1
                    and p[0] == "DmlExecutionProvider"
                ):
                    opts = dict(p[1]) if len(p) > 1 and p[1] else {}
                    opts["device_id"] = device_id
                    new_providers.append(("DmlExecutionProvider", opts))
                else:
                    new_providers.append(p)
            kwargs["providers"] = new_providers

        _original_init(self, *args, **kwargs)

    ort.InferenceSession.__init__ = _patched_init
    ort.InferenceSession._dml_device_patched = True
    logger.info(
        "Patched onnxruntime.InferenceSession to use DML device_id=%d", device_id
    )


class PaddleOCR:
    """
    RapidOCR-to-PaddleOCR 3.3 compatibility layer.

    Supported init parameters:
    - use_angle_cls: Whether to use orientation classifier.
    - lang: Language ('ch', 'en', etc.; RapidOCR mainly supports Chinese/English).
    - show_log: Whether to show logs.
    - use_gpu: GPU acceleration (compatibility parameter).
    - use_dml: Whether to use DirectML acceleration.
    - cpu_threads: CPU thread count (unclear whether effective; may be removed).
    - use_doc_orientation_classify: Document orientation classification (compat).
    - use_doc_unwarping: Document unwarping (compat).
    - text_detection_model_name: Text detection model name (compat).
    - text_recognition_model_name: Text recognition model name (compat).
    - ocr_version: OCR model version (compat).
    """

    def __init__(
        self,
        use_angle_cls: bool = True,
        lang: str = "ch",
        use_gpu: bool = False,
        use_dml: bool = False,
        dml_device_id: Optional[int] = None,
        show_log: bool = False,
        cpu_threads: int = 2,
        use_doc_orientation_classify: bool = False,
        use_doc_unwarping: bool = False,
        text_detection_model_name: Optional[str] = None,
        text_recognition_model_name: Optional[str] = None,
        ocr_version: Optional[str] = None,
    ):
        """
        Initialise the OCR engine.

        Args:
            use_angle_cls: Whether to use text orientation classification.
            lang: Language type (compatibility parameter; handled by RapidOCR internally).
            use_gpu: Whether to use GPU (compatibility parameter).
            use_dml: Whether to use DirectML acceleration (requires onnxruntime-directml).
            dml_device_id: DirectML device ID (None = default GPU).
            show_log: Whether to show logs.
            cpu_threads: CPU thread count (unclear if effective; may be removed later).
            TODO: Remove cpu_threads parameter.
        """
        params = {
            "Global.use_cls": use_angle_cls,
            "Global.log_level": "DEBUG" if show_log else "WARNING",
        }

        # If a DML device_id is specified, patch onnxruntime before creating the engine.
        if use_dml and dml_device_id is not None:
            _patch_ort_dml_device_id(dml_device_id)

        if use_dml:
            params["EngineConfig.onnxruntime.use_dml"] = True
        self.engine = RapidOCR(params=params)

        self.lang = lang
        self.show_log = show_log
        self._last_elapse = 0.0

    def predict(
        self,
        img: Union[str, np.ndarray, bytes, Image.Image],
        det: bool = True,
        rec: bool = True,
        cls: bool = True,
    ) -> List[List[List]]:
        """
        PaddleOCR 3.3-compatible predict method; delegates to ocr().
        """
        return self.ocr(img, det=det, rec=rec, cls=cls)

    def ocr(
        self,
        img: Union[str, np.ndarray, bytes, Image.Image],
        det: bool = True,
        rec: bool = True,
        cls: bool = True,
    ) -> List[List[List]]:
        """
        Perform OCR (PaddleOCR 3.3-compatible output format).

        Args:
            img: Image path, numpy array, bytes, or PIL Image.
            det: Whether to perform text detection (compat parameter).
            rec: Whether to perform text recognition (compat parameter).
            cls: Whether to perform orientation classification (compat parameter).

        Returns:
            PaddleOCR 3.3-format result:
            [
                [  # First page / first image
                    [box, (text, score)],  # First text line
                    [box, (text, score)],  # Second text line
                    ...
                ]
            ]

            Where:
            - box: [[x1,y1], [x2,y2], [x3,y3], [x4,y4]] four corner coordinates
            - text: str, recognised text
            - score: float, confidence (0-1)
        """
        # Call RapidOCR (newer versions return a RapidOCROutput object)
        result = self.engine(img)
        self._last_elapse = result.elapse
        self._last_elapse_list = result.elapse_list

        # Convert to PaddleOCR 3.3 format
        if result.boxes is None or len(result.boxes) == 0:
            # Empty result: return [[]]
            return [[]]

        # Convert format: RapidOCROutput -> PaddleOCR [box, (text, score)]
        paddle_format = []
        for i in range(len(result.txts)):
            box = (
                result.boxes[i].tolist()
                if hasattr(result.boxes[i], "tolist")
                else result.boxes[i]
            )
            text = result.txts[i]
            score = result.scores[i]

            # Assemble into PaddleOCR format
            paddle_format.append([box, (text, score)])

        # Wrap in an outer list (simulating multi-page results)
        return [paddle_format]

    def get_last_elapse(self):
        """
        Return the elapsed time of the last OCR call.
        Returns a (det_time, cls_time, rec_time) tuple or the total elapsed time.
        """
        if hasattr(self, "_last_elapse_list") and self._last_elapse_list:
            return self._last_elapse_list
        return self._last_elapse

    def __call__(self, img, **kwargs):
        """Support direct invocation: ocr(img)"""
        return self.ocr(img, **kwargs)

