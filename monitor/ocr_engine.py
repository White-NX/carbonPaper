"""OCR engine module — RapidOCR initialisation and recognition.

Fix notes:
- Ensures the OCR instance is a thread-safe singleton to avoid redundant
  initialisation and memory growth.
"""
import os
import gc
import logging
import numpy as np
from typing import Optional, List, Tuple, Dict, Any
from PIL import Image, ImageDraw, ImageFont
import threading

logger = logging.getLogger(__name__)

from rapidocr_capability import PaddleOCR as RapidPaddleOCR


def _get_ppocr_base_dir() -> str:
    """Return the PP-OCR model root directory (sibling of Chinese-CLIP)."""
    clip_model_path = os.environ.get('MODEL_PATH', None)
    if clip_model_path:
        clip_model_path = os.path.abspath(clip_model_path)
        base_dir = os.path.dirname(clip_model_path)
    else:
        base_dir = os.path.abspath(
            os.path.join(
                os.environ.get('LOCALAPPDATA', os.path.expanduser('~')),
                "carbonPaper",
            )
        )
    return os.path.join(base_dir, "ppOCRmodel")


class OCREngine:
    """Thread-safe singleton OCR engine."""

    _instance: Optional['OCREngine'] = None
    _init_lock = threading.Lock()

    def __new__(cls, *args, **kwargs):
        with cls._init_lock:
            if cls._instance is None:
                cls._instance = super().__new__(cls)
                cls._instance._initialized = False
                cls._instance._inference_lock = threading.Lock()
        return cls._instance
    
    def __init__(
        self,
        use_angle_cls: bool = False,
        lang: str = "ch",
        use_gpu: bool = False,
        use_dml: bool = False,
        dml_device_id: Optional[int] = None,
        ocr_version: str = 'PP-OCRv5',
        model_size: str = "mobile",
        det_model_dir: Optional[str] = None,
        rec_model_dir: Optional[str] = None,
        cls_model_dir: Optional[str] = None,
    ):
        """
        Initialise the OCR engine.

        Args:
            use_angle_cls: Whether to enable the orientation classifier.
            lang: Language setting, defaults to Chinese.
            use_gpu: Whether to use GPU (compatibility parameter).
            use_dml: Whether to use DirectML acceleration.
            dml_device_id: DirectML device ID (None = default GPU).
            ocr_version: OCR model version.
            model_size: Model size, "mobile" or "server" (default: mobile).
            det_model_dir: Detection model directory (optional).
            rec_model_dir: Recognition model directory (optional).
            cls_model_dir: Classification model directory (optional).
        """
        # Skip if already initialised to avoid reloading the model
        if getattr(self, '_initialized', False):
            return

        logger.info("Initialising OCR (using %s, DirectML=%s)...", ocr_version, use_dml)

        # Record GPU usage flags
        self._use_gpu = bool(use_gpu)
        self._use_dml = bool(use_dml)
        self._dml_device_id = dml_device_id

        init_params = {
            'use_angle_cls': use_angle_cls,
            'lang': lang,
            # Disable document pre-processing (rotation/unwarping) to avoid coordinate mapping offsets
            'use_doc_orientation_classify': False,
            'use_doc_unwarping': False,
        }
        
        # If no model directories specified, default to PP-OCRv5 mobile and let PaddleOCR download them
        if not (det_model_dir and rec_model_dir and cls_model_dir):
            ppocr_root = _get_ppocr_base_dir()
            os.makedirs(ppocr_root, exist_ok=True)
            normalized_size = str(model_size).strip().lower()
            if normalized_size not in {"mobile", "server"}:
                logger.warning("Unknown model_size=%s, falling back to mobile", model_size)
                normalized_size = "mobile"

            # Force PP-OCRv5 mobile model names to prevent falling back to server
            if normalized_size == "mobile":
                init_params["text_detection_model_name"] = "PP-OCRv5_mobile_det"
                if lang == "en":
                    init_params["text_recognition_model_name"] = "en_PP-OCRv5_mobile_rec"
                elif lang in {"latin", "eslav", "arabic", "cyrillic", "devanagari", "korean", "th", "el", "te", "ta"}:
                    init_params["text_recognition_model_name"] = f"{lang}_PP-OCRv5_mobile_rec"
                else:
                    # Chinese/Japanese/Traditional etc. use the generic mobile rec model
                    init_params["text_recognition_model_name"] = "PP-OCRv5_mobile_rec"

            det_suffix = "det_mobile" if normalized_size == "mobile" else "det_server"
            rec_suffix = "rec_mobile" if normalized_size == "mobile" else "rec_server"

            det_model_dir = det_model_dir or os.path.join(ppocr_root, f"ch_PP-OCRv5_{det_suffix}_infer")
            rec_model_dir = rec_model_dir or os.path.join(ppocr_root, f"ch_PP-OCRv5_{rec_suffix}_infer")
            cls_model_dir = cls_model_dir or os.path.join(ppocr_root, "ch_ppocr_mobile_v2.0_cls_infer")

        # Add optional model directories (only pass if the directory exists to avoid PaddleX assertion failures)
        if det_model_dir and os.path.exists(det_model_dir):
            init_params['det_model_dir'] = det_model_dir
        elif det_model_dir:
            logger.warning("det_model_dir NOT FOUND, using default download path: %s", det_model_dir)

        if rec_model_dir and os.path.exists(rec_model_dir):
            init_params['rec_model_dir'] = rec_model_dir
        elif rec_model_dir:
            logger.warning("rec_model_dir NOT FOUND, using default download path: %s", rec_model_dir)

        if cls_model_dir and os.path.exists(cls_model_dir):
            init_params['cls_model_dir'] = cls_model_dir
        elif cls_model_dir:
            logger.warning("cls_model_dir NOT FOUND, disabling angle classification: %s", cls_model_dir)
            init_params['use_angle_cls'] = False
            
        # Use a lock to ensure only one concurrent initialisation
        with self._init_lock:
            if getattr(self, '_initialized', False):
                return
            try:
                init_params['ocr_version'] = ocr_version
                init_params['cpu_threads'] = 1
                init_params['use_dml'] = self._use_dml
                if self._dml_device_id is not None:
                    init_params['dml_device_id'] = self._dml_device_id
                self.ocr = RapidPaddleOCR(**init_params)
            except Exception as e:
                logger.error("Using %s to initialize RapidOCR FAILED: %s", ocr_version, e)
                if 'ocr_version' in init_params:
                     del init_params['ocr_version']
                try:
                    init_params['cpu_threads'] = 1
                    self.ocr = RapidPaddleOCR(**init_params)
                except Exception as e2:
                    logger.error("Retry to initialize RapidOCR FAILED: %s", e2)
                    raise e2

            self._initialized = True
            logger.info("OCR initialized successfully (DirectML=%s)", self._use_dml)

    def close(self) -> None:
        """Explicitly release the OCR instance and free memory.

        The instance must be recreated for subsequent use.
        """
        try:
            if getattr(self, 'ocr', None) is not None:
                del self.ocr
        except Exception:
            pass

        gc.collect()
        self._initialized = False
    
    def recognize(
        self,
        image_input: Any,
    ) -> List[Dict[str, Any]]:
        """
        Perform OCR on an image.

        Args:
            image_input: Image path, numpy array, or PIL Image object.

        Returns:
            List of results, each containing:
            - box: Text bounding box [[x1,y1], [x2,y2], [x3,y3], [x4,y4]]
            - text: Recognised text
            - confidence: Confidence score
        """
        # Accept PIL Image / numpy array / OpenCV image
        if isinstance(image_input, Image.Image):
            image_np = np.array(image_input)
        else:
            image_np = image_input

        # Use inference lock to prevent concurrent re-creation or races
        with self._inference_lock:
            try:
                logger.info("[OCR Engine] Using PaddleOCR.predict(), Image Size: %s", image_np.shape if hasattr(image_np, 'shape') else 'unknown')
                # PaddleOCR 3.x no longer supports passing cls at call time
                # The orientation classifier is controlled at init via use_angle_cls
                result = self.ocr.predict(image_np)
                logger.info("[OCR Engine] PaddleOCR.predict() return: %s, length: %s", type(result), len(result) if result else 'None')
            except Exception as ocr_err:
                logger.exception("[OCR Engine] PaddleOCR.predict() ERROR: %s", ocr_err)
                return []

        if not result or result[0] is None or not isinstance(result[0], (dict, list)):
            # Try to release temporary objects
            try:
                del result
            except Exception:
                pass
            gc.collect()
            return []

        ocr_results = []
        page_result = result[0]
        
        # PaddleOCR 3.x and the RapidOCR compat layer return a dict
        if isinstance(page_result, dict):
            rec_texts = page_result.get('rec_texts', [])
            rec_scores = page_result.get('rec_scores', [])
            # Use dt_polys (detection polygons, original image coordinates)
            # Do NOT use rec_polys — those are relative coords from the recogniser crop
            dt_polys = page_result.get('dt_polys', [])

            # Debug: log the format of the first box
            if len(dt_polys) > 0:
                sample = dt_polys[0]
                logger.info("[OCR Engine] dt_polys sample format: type=%s, shape=%s, len=%s", type(sample), getattr(sample, 'shape', None), len(sample) if hasattr(sample, '__len__') else 'N/A')
            
            for i, text in enumerate(rec_texts):
                coords = dt_polys[i] if i < len(dt_polys) else []
                score = rec_scores[i] if i < len(rec_scores) else 0.0
                
                # Convert to list format
                if hasattr(coords, 'tolist'):
                    coords = coords.tolist()
                
                # Ensure 4-point format [[x1,y1], [x2,y2], [x3,y3], [x4,y4]]
                # PaddleOCR 3.x may return [x1,y1,x2,y2,x3,y3,x4,y4] flat format
                if isinstance(coords, (list, tuple)):
                    if len(coords) == 8:
                        # Flat format — convert to nested
                        coords = [
                            [coords[0], coords[1]],
                            [coords[2], coords[3]],
                            [coords[4], coords[5]],
                            [coords[6], coords[7]]
                        ]
                    elif len(coords) == 4 and isinstance(coords[0], (list, tuple)) and len(coords[0]) == 2:
                        # Already in the correct format
                        pass
                    else:
                        logger.warning("[OCR Engine] Unknown coordinate format: %s", coords)
                        coords = [[0,0], [0,0], [0,0], [0,0]]
                
                ocr_results.append({
                    'box': coords,
                    'text': text,
                    'confidence': float(score)
                })
        # PaddleOCR 2.x returns list format: [[coords, (text, confidence)], ...]
        elif isinstance(page_result, list):
            for line in page_result:
                try:
                    if isinstance(line, (list, tuple)) and len(line) == 2:
                        coords, text_info = line
                        if isinstance(text_info, (list, tuple)) and len(text_info) == 2:
                            text, confidence = text_info
                        else:
                            continue
                        ocr_results.append({
                            'box': coords,
                            'text': text,
                            'confidence': confidence
                        })
                except Exception as parse_err:
                    logger.error("[OCR Engine] Reasoning OCR result line failed: %s", parse_err)
                    continue
        else:
            logger.warning("[OCR Engine] Unknown page_result format: %s", type(page_result))
        
        try:
            elapse = self.ocr.get_last_elapse()
            elapse_total = elapse[2] if isinstance(elapse, (list, tuple)) and len(elapse) > 2 else 0.0
        except Exception:
            elapse_total = 0.0
        logger.info("[OCR Engine] Reasoning Complete, got %d text block, used %.3f seconds", len(ocr_results), elapse_total)

        # Clean up temporary objects
        try:
            del result
        except Exception:
            pass
        gc.collect()

        return ocr_results
    
    def recognize_batch(
        self,
        images: List[Any],
        cls: bool = True
    ) -> List[List[Dict[str, Any]]]:
        """
        Batch OCR recognition.

        Args:
            images: List of images.
            cls: Whether to use orientation classifier.

        Returns:
            List of recognition results per image.
        """
        results = []
        for image in images:
            results.append(self.recognize(image, cls=cls))
        return results


class OCRVisualizer:
    """OCR result visualisation helper."""
    
    def __init__(self, font_path: str = 'C:/Windows/Fonts/simhei.ttf'):
        """
        Initialise the visualiser.

        Args:
            font_path: Path to a font file.
        """
        self.font = self._load_font(font_path)
    
    def _load_font(self, font_path: str, size: int = 20) -> ImageFont.FreeTypeFont:
        """Load a font."""
        try:
            if os.path.exists(font_path):
                return ImageFont.truetype(font_path, size, encoding="utf-8")
            
            # Try other common fonts
            alt_fonts = ['arial.ttf', 'msyh.ttf', 'simsun.ttc']
            for f in alt_fonts:
                try:
                    return ImageFont.truetype(f, size)
                except:
                    continue
            
            logger.warning("Unable to load font, using default")
            return ImageFont.load_default()
        except Exception as e:
            logger.error("Failed to load font: %s", e)
            return ImageFont.load_default()
    
    def draw_results(
        self,
        image: Image.Image,
        ocr_results: List[Dict[str, Any]],
        box_color: str = 'red',
        text_color: str = 'blue',
        show_confidence: bool = False
    ) -> Image.Image:
        """
        Draw OCR results on an image.

        Args:
            image: PIL Image object.
            ocr_results: OCR recognition results.
            box_color: Bounding box colour.
            text_color: Text colour.
            show_confidence: Whether to show confidence scores.

        Returns:
            Image with drawn results.
        """
        image = image.copy().convert('RGB')
        draw = ImageDraw.Draw(image)
        
        for result in ocr_results:
            box = result['box']
            text = result['text']
            confidence = result['confidence']
            
            # Draw polygon border
            xy = [tuple(point) for point in box]
            draw.polygon(xy, outline=box_color)
            
            # Draw text
            if len(xy) > 0:
                txt_pos = (xy[0][0], max(0, xy[0][1] - 25))
                display_text = f"{text} ({confidence:.2f})" if show_confidence else text
                draw.text(txt_pos, display_text, fill=text_color, font=self.font)
        
        return image
    
    def save_visualization(
        self,
        image: Image.Image,
        ocr_results: List[Dict[str, Any]],
        output_path: str,
        **kwargs
    ) -> None:
        """Save visualisation results to a file."""
        result_image = self.draw_results(image, ocr_results, **kwargs)
        result_image.save(output_path)
        logger.info("Visualisation saved to %s", output_path)


# Convenience function
def get_ocr_engine(**kwargs) -> OCREngine:
    """Get the OCR engine instance (singleton)."""
    if 'use_dml' not in kwargs:
        kwargs['use_dml'] = os.environ.get('CARBONPAPER_USE_DML', '').strip() == '1'
    if 'dml_device_id' not in kwargs:
        device_id_str = os.environ.get('CARBONPAPER_DML_DEVICE_ID', '').strip()
        if device_id_str:
            try:
                kwargs['dml_device_id'] = int(device_id_str)
            except ValueError:
                pass
    return OCREngine(**kwargs)

