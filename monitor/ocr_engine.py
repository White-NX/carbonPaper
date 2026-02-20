"""OCR引擎模块 - RapidOCR 初始化和识别功能

修复说明:
- 确保 OCR 实例为单例且线程安全，避免重复初始化导致内存增长。
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
    """获取 PP-OCR 模型根目录（与 Chinese-CLIP 同级）。"""
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
    """OCR 引擎封装类（线程安全单例）"""

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
        ocr_version: str = 'PP-OCRv5',
        model_size: str = "mobile",
        det_model_dir: Optional[str] = None,
        rec_model_dir: Optional[str] = None,
        cls_model_dir: Optional[str] = None,
    ):
        """
        初始化OCR引擎

        Args:
            use_angle_cls: 是否启用方向分类器
            lang: 语言设置，默认中文
            use_gpu: 是否使用GPU（兼容参数）
            use_dml: 是否使用 DirectML 加速
            ocr_version: OCR模型版本
            model_size: 模型尺寸，"mobile" 或 "server"，默认 mobile
            det_model_dir: 检测模型目录（可选）
            rec_model_dir: 识别模型目录（可选）
            cls_model_dir: 分类模型目录（可选）
        """
        # 如果已经初始化则直接返回，避免重复加载模型
        if getattr(self, '_initialized', False):
            return

        logger.info("正在初始化 OCR (使用 %s, DirectML=%s)...", ocr_version, use_dml)

        # 记录是否使用 GPU
        self._use_gpu = bool(use_gpu)
        self._use_dml = bool(use_dml)

        init_params = {
            'use_angle_cls': use_angle_cls,
            'lang': lang,
            # 禁用文档预处理（旋转/去畸变），避免坐标映射偏移
            'use_doc_orientation_classify': False,
            'use_doc_unwarping': False,
        }
        
        # 如果未指定模型目录，默认使用 PP-OCRv5 mobile，并让 PaddleOCR 自行下载
        if not (det_model_dir and rec_model_dir and cls_model_dir):
            ppocr_root = _get_ppocr_base_dir()
            os.makedirs(ppocr_root, exist_ok=True)
            normalized_size = str(model_size).strip().lower()
            if normalized_size not in {"mobile", "server"}:
                logger.warning("未知的 model_size=%s，回退为 mobile", model_size)
                normalized_size = "mobile"

            # 强制使用 PP-OCRv5 mobile 模型名称，避免默认回落到 server
            if normalized_size == "mobile":
                init_params["text_detection_model_name"] = "PP-OCRv5_mobile_det"
                if lang == "en":
                    init_params["text_recognition_model_name"] = "en_PP-OCRv5_mobile_rec"
                elif lang in {"latin", "eslav", "arabic", "cyrillic", "devanagari", "korean", "th", "el", "te", "ta"}:
                    init_params["text_recognition_model_name"] = f"{lang}_PP-OCRv5_mobile_rec"
                else:
                    # 中文/日文/繁体等默认使用通用 mobile rec
                    init_params["text_recognition_model_name"] = "PP-OCRv5_mobile_rec"

            det_suffix = "det_mobile" if normalized_size == "mobile" else "det_server"
            rec_suffix = "rec_mobile" if normalized_size == "mobile" else "rec_server"

            det_model_dir = det_model_dir or os.path.join(ppocr_root, f"ch_PP-OCRv5_{det_suffix}_infer")
            rec_model_dir = rec_model_dir or os.path.join(ppocr_root, f"ch_PP-OCRv5_{rec_suffix}_infer")
            cls_model_dir = cls_model_dir or os.path.join(ppocr_root, "ch_ppocr_mobile_v2.0_cls_infer")

        # 添加可选的模型目录（仅在目录存在时传入，避免 PaddleX 断言失败）
        if det_model_dir and os.path.exists(det_model_dir):
            init_params['det_model_dir'] = det_model_dir
        elif det_model_dir:
            logger.warning("det_model_dir 不存在，将使用默认下载路径: %s", det_model_dir)

        if rec_model_dir and os.path.exists(rec_model_dir):
            init_params['rec_model_dir'] = rec_model_dir
        elif rec_model_dir:
            logger.warning("rec_model_dir 不存在，将使用默认下载路径: %s", rec_model_dir)

        if cls_model_dir and os.path.exists(cls_model_dir):
            init_params['cls_model_dir'] = cls_model_dir
        elif cls_model_dir:
            logger.warning("cls_model_dir 不存在，将禁用方向分类器: %s", cls_model_dir)
            init_params['use_angle_cls'] = False
            
        # 使用锁确保并发下只会初始化一次
        with self._init_lock:
            if getattr(self, '_initialized', False):
                return
            try:
                init_params['ocr_version'] = ocr_version
                init_params['cpu_threads'] = 1
                init_params['use_dml'] = self._use_dml
                self.ocr = RapidPaddleOCR(**init_params)
            except Exception as e:
                logger.error("使用 %s 初始化 RapidOCR 失败: %s", ocr_version, e)
                if 'ocr_version' in init_params:
                     del init_params['ocr_version']
                try:
                    init_params['cpu_threads'] = 1
                    self.ocr = RapidPaddleOCR(**init_params)
                except Exception as e2:
                    logger.error("重试初始化失败: %s", e2)
                    raise e2

            self._initialized = True
            logger.info("OCR 初始化完成 (DirectML=%s)", self._use_dml)

    def close(self) -> None:
        """显式释放 OCR 实例并清理内存。

        调用后再次使用需要重新创建实例。
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
        对图片进行OCR识别
        
        Args:
            image_input: 图片路径、numpy数组或PIL Image对象
            
        Returns:
            识别结果列表，每项包含:
            - box: 文本框坐标 [[x1,y1], [x2,y2], [x3,y3], [x4,y4]]
            - text: 识别的文本
            - confidence: 置信度
        """
        # 接受 PIL Image / numpy array / OpenCV 图像
        if isinstance(image_input, Image.Image):
            image_np = np.array(image_input)
        else:
            image_np = image_input

        # 使用推理锁，防止并发导致重复创建或竞态
        with self._inference_lock:
            try:
                logger.info("[OCR Engine] 调用 PaddleOCR.predict()，图像尺寸: %s", image_np.shape if hasattr(image_np, 'shape') else 'unknown')
                # PaddleOCR 3.x 不再支持在 ocr() 调用时传入 cls 参数
                # 角度分类器在初始化时通过 use_angle_cls 控制
                result = self.ocr.predict(image_np)
                logger.info("[OCR Engine] PaddleOCR.predict() 返回: %s, 长度: %s", type(result), len(result) if result else 'None')
            except Exception as ocr_err:
                logger.exception("[OCR Engine] PaddleOCR.predict() 异常: %s", ocr_err)
                return []

        if not result or result[0] is None:
            # 尝试释放临时对象
            try:
                del result
            except Exception:
                pass
            gc.collect()
            return []

        ocr_results = []
        page_result = result[0]
        
        # PaddleOCR 3.x 以及 RapidOCR 兼容层返回字典格式
        if isinstance(page_result, dict):
            rec_texts = page_result.get('rec_texts', [])
            rec_scores = page_result.get('rec_scores', [])
            # 使用 dt_polys（检测多边形，原始图像坐标）
            # 不要使用 rec_polys，那是识别器裁剪后的相对坐标
            dt_polys = page_result.get('dt_polys', [])

            # 调试：打印第一个框的格式
            if len(dt_polys) > 0:
                sample = dt_polys[0]
                logger.info("[OCR Engine] dt_polys 样例格式: type=%s, shape=%s, len=%s", type(sample), getattr(sample, 'shape', None), len(sample) if hasattr(sample, '__len__') else 'N/A')
            
            for i, text in enumerate(rec_texts):
                coords = dt_polys[i] if i < len(dt_polys) else []
                score = rec_scores[i] if i < len(rec_scores) else 0.0
                
                # 转换为列表格式
                if hasattr(coords, 'tolist'):
                    coords = coords.tolist()
                
                # 确保是 4 个点的格式 [[x1,y1], [x2,y2], [x3,y3], [x4,y4]]
                # PaddleOCR 3.x 可能返回 [x1,y1,x2,y2,x3,y3,x4,y4] 扁平格式
                if isinstance(coords, (list, tuple)):
                    if len(coords) == 8:
                        # 扁平格式，转换为嵌套格式
                        coords = [
                            [coords[0], coords[1]],
                            [coords[2], coords[3]],
                            [coords[4], coords[5]],
                            [coords[6], coords[7]]
                        ]
                    elif len(coords) == 4 and isinstance(coords[0], (list, tuple)) and len(coords[0]) == 2:
                        # 已经是正确格式
                        pass
                    else:
                        logger.warning("[OCR Engine] 未知的坐标格式: %s", coords)
                        coords = [[0,0], [0,0], [0,0], [0,0]]
                
                ocr_results.append({
                    'box': coords,
                    'text': text,
                    'confidence': float(score)
                })
        # PaddleOCR 2.x 返回列表格式: [[coords, (text, confidence)], ...]
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
                    logger.error("[OCR Engine] 解析 OCR 结果行失败: %s", parse_err)
                    continue
        else:
            logger.warning("[OCR Engine] 未知的 page_result 格式: %s", type(page_result))
        
        logger.info("[OCR Engine] 解析完成，得到 %d 个文本块，OCR任务用时 %.3f 秒", len(ocr_results), self.ocr.get_last_elapse()[2])

        # 清理临时对象
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
        批量OCR识别
        
        Args:
            images: 图片列表
            cls: 是否使用方向分类器
            
        Returns:
            每张图片的识别结果列表
        """
        results = []
        for image in images:
            results.append(self.recognize(image, cls=cls))
        return results


class OCRVisualizer:
    """OCR结果可视化工具"""
    
    def __init__(self, font_path: str = 'C:/Windows/Fonts/simhei.ttf'):
        """
        初始化可视化工具
        
        Args:
            font_path: 字体文件路径
        """
        self.font = self._load_font(font_path)
    
    def _load_font(self, font_path: str, size: int = 20) -> ImageFont.FreeTypeFont:
        """加载字体"""
        try:
            if os.path.exists(font_path):
                return ImageFont.truetype(font_path, size, encoding="utf-8")
            
            # 尝试其他常用字体
            alt_fonts = ['arial.ttf', 'msyh.ttf', 'simsun.ttc']
            for f in alt_fonts:
                try:
                    return ImageFont.truetype(f, size)
                except:
                    continue
            
            logger.warning("无法加载字体，使用默认字体")
            return ImageFont.load_default()
        except Exception as e:
            logger.error("加载字体失败: %s", e)
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
        在图片上绘制OCR结果
        
        Args:
            image: PIL Image对象
            ocr_results: OCR识别结果
            box_color: 边框颜色
            text_color: 文本颜色
            show_confidence: 是否显示置信度
            
        Returns:
            绘制了结果的图片
        """
        image = image.copy().convert('RGB')
        draw = ImageDraw.Draw(image)
        
        for result in ocr_results:
            box = result['box']
            text = result['text']
            confidence = result['confidence']
            
            # 绘制多边形边框
            xy = [tuple(point) for point in box]
            draw.polygon(xy, outline=box_color)
            
            # 绘制文本
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
        """保存可视化结果到文件"""
        result_image = self.draw_results(image, ocr_results, **kwargs)
        result_image.save(output_path)
        logger.info("可视化结果已保存至 %s", output_path)


# 便捷函数
def get_ocr_engine(**kwargs) -> OCREngine:
    """获取OCR引擎实例（单例）"""
    if 'use_dml' not in kwargs:
        kwargs['use_dml'] = os.environ.get('CARBONPAPER_USE_DML', '').strip() == '1'
    return OCREngine(**kwargs)


if __name__ == "__main__":
    # 测试代码
    engine = get_ocr_engine()
    
    # 创建测试图片
    import cv2
    test_img = np.zeros((200, 600, 3), dtype=np.uint8)
    cv2.putText(test_img, 'PaddleOCR Test', (50, 100), 
                cv2.FONT_HERSHEY_SIMPLEX, 1.5, (255, 255, 255), 2)
    
    # 识别
    results = engine.recognize(test_img)
    print(f"识别结果: {results}")
