"""
RapidOCR to PaddleOCR 3.3 Compatibility Layer
"""

from rapidocr_onnxruntime import RapidOCR
from typing import Union, List, Optional
import numpy as np
from PIL import Image


class PaddleOCR:
    """
    RapidOCR的PaddleOCR 3.3兼容层

    支持的初始化参数：
    - use_angle_cls: 是否使用方向分类器
    - lang: 语言（'ch', 'en'等，但RapidOCR主要支持中英文）
    - show_log: 是否显示日志
    - use_gpu: GPU加速
    - cpu_threads: CPU线程数（不知道实际上有没有作用的参数）
    - use_doc_orientation_classify: 文档方向分类（兼容）
    - use_doc_unwarping: 文档去畸变（兼容）
    - text_detection_model_name: 文本检测模型名称（兼容）
    - text_recognition_model_name: 文本识别模型名称（兼容）
    - ocr_version: OCR模型版本（兼容）
    """

    def __init__(
        self,
        use_angle_cls: bool = True,
        lang: str = "ch",
        use_gpu: bool = False,
        show_log: bool = False,
        cpu_threads: int = 2,
        use_doc_orientation_classify: bool = False,
        use_doc_unwarping: bool = False,
        text_detection_model_name: Optional[str] = None,
        text_recognition_model_name: Optional[str] = None,
        ocr_version: Optional[str] = None,
    ):
        """
        初始化OCR引擎

        Args:
            use_angle_cls: 是否使用文字方向分类
            lang: 语言类型（兼容参数，实际由RapidOCR处理）
            use_gpu: 是否使用GPU（RapidOCR-ONNX不支持，会忽略）
            show_log: 是否显示日志
            cpu_threads: CPU线程数（不知道为什么在主程序中被使用了，之后会移除）
            TODO: 移除 cpu_threads 参数
        """
        if use_gpu and show_log:
            print(
                "[WARNING] RapidOCR did not support GPU acceleration in ONNX version. Ignoring use_gpu=True."
            )

        # 初始化RapidOCR引擎
        self.engine = RapidOCR(use_angle_cls=use_angle_cls, print_verbose=show_log)

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
        兼容PaddleOCR 3.3的predict方法，调用ocr()
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
        执行OCR识别（兼容PaddleOCR 3.3格式）

        Args:
            img: 图片路径、numpy数组、bytes或PIL Image对象
            det: 是否进行文本检测（兼容参数）
            rec: 是否进行文本识别（兼容参数）
            cls: 是否进行方向分类（兼容参数）

        Returns:
            PaddleOCR 3.3格式的结果:
            [
                [  # 第一页/第一张图
                    [box, (text, score)],  # 第一行文本
                    [box, (text, score)],  # 第二行文本
                    ...
                ]
            ]

            其中：
            - box: [[x1,y1], [x2,y2], [x3,y3], [x4,y4]] 四个角点坐标
            - text: str, 识别的文本
            - score: float, 置信度(0-1)
        """
        # 调用RapidOCR
        result, elapse = self.engine(img)
        self._last_elapse = elapse

        # 转换为PaddleOCR 3.3格式
        if result is None or len(result) == 0:
            # 空结果：返回 [[]]
            return [[]]

        # 转换格式：RapidOCR [box, text, score] -> PaddleOCR [box, (text, score)]
        paddle_format = []
        for item in result:
            box = item[0]  # [[x1,y1], [x2,y2], [x3,y3], [x4,y4]]
            text = item[1]  # str
            score = item[2]  # float

            # 组装成PaddleOCR格式
            paddle_format.append([box, (text, score)])

        # 外层再包一层列表（模拟多页结果）
        return [paddle_format]

    def get_last_elapse(self) -> float:
        """
        获取上次OCR耗时（秒）
        这是额外添加的方法，PaddleOCR原生不支持
        """
        return self._last_elapse

    def __call__(self, img, **kwargs):
        """支持直接调用：ocr(img)"""
        return self.ocr(img, **kwargs)


if __name__ == "__main__":
    ocr = PaddleOCR(
        use_angle_cls=True, lang="ch", use_gpu=False, show_log=True, cpu_threads=2
    )

    # 方式1: 使用文件路径
    result = ocr.ocr("test.jpg", cls=True)

    # 方式2: 使用numpy数组
    import cv2

    img = cv2.imread("test.jpg")
    result = ocr.ocr(img)

    # 方式3: 直接调用
    result = ocr("test.jpg")

    # 解析结果
    for res in result:
        if not res:
            print("未检测到文本")
            continue

        for line in res:
            box = line[0]
            text = line[1][0]
            score = line[1][1]

            print(f"文本: {text}")
            print(f"置信度: {score:.4f}")
            print(f"坐标: {box}")
            print("-" * 50)

    # 额外功能：获取耗时
    print(f"\nOCR耗时: {ocr.get_last_elapse():.3f}秒")
