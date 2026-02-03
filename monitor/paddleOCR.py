from paddleocr import PaddleOCR
import cv2
import os
import numpy as np
from PIL import Image, ImageDraw, ImageFont

def draw_ocr_custom(image, boxes, txts, scores, font_path='C:/Windows/Fonts/simhei.ttf'):
    """
    可视化OCR结果 (PIL方式，支持中文)
    """
    draw = ImageDraw.Draw(image)
    
    # 尝试加载字体
    font = None
    try:
        if os.path.exists(font_path):
             font = ImageFont.truetype(font_path, 20, encoding="utf-8")
        else:
             # Try other common fonts
             alt_fonts = ['arial.ttf', 'msyh.ttf'] 
             found = False
             for f in alt_fonts:
                 try:
                     font = ImageFont.truetype(f, 20)
                     found = True
                     break
                 except:
                     continue
             
             if not found:
                 print(f"警告: 字体文件可能无法加载，尝试默认")
                 font = ImageFont.load_default()
    except Exception as e:
        print(f"加载字体失败: {e}")
        font = ImageFont.load_default()

    for (box, txt, score) in zip(boxes, txts, scores):
        xy = [tuple(point) for point in box]
        draw.polygon(xy, outline='red')
        
        if len(xy) > 0:
            txt_pos = xy[0]
            txt_pos = (txt_pos[0], max(0, txt_pos[1] - 25))
            draw.text(txt_pos, f"{txt}", fill='blue', font=font)
        
    return image

try:
    import paddle
except ImportError:
    paddle = None

# ...

# 1. 初始化模型
print("正在初始化 PaddleOCR (使用 PP-OCRv5)...")
# use_angle_cls=True: 启用方向分类器
# lang="ch": 设置语言为中文
# use_gpu=False: 强制使用CPU (通过 paddle.device.set_device 设置)
# ocr_version='PP-OCRv5': 指定使用 PP-OCRv5 模型
# 注意: 如果本地没有模型，会自动下载

if paddle:
    paddle.device.set_device("cpu")

try:
    ocr = PaddleOCR(use_angle_cls=True, lang="ch", ocr_version='PP-OCRv5')
except Exception as e:
    print(f"初始化失败: {e}")
    print("尝试不指定 ocr_version (默认使用最新版)...")
    ocr = PaddleOCR(use_angle_cls=True, lang="ch")

# 2. 读取图片
image_path = "focused_window.jpg"
if not os.path.exists(image_path):
    print(f"错误: 找不到文件 {image_path}")
    # 生成测试图片
    print("生成测试图片...")
    img = np.zeros((200, 600, 3), dtype=np.uint8)
    cv2.putText(img, 'PaddleOCR Test', (50, 100), cv2.FONT_HERSHEY_SIMPLEX, 1.5, (255, 255, 255), 2)
    cv2.imwrite(image_path, img)

# 3. 进行预测
print("开始进行OCR识别...")
result = ocr.ocr(image_path, cls=True)

# 4. 打印结果
if not result or result[0] is None:
    print("未检测到任何文本。")
else:
    ocr_result = result[0]
    print(f"检测到的文本行数: {len(ocr_result)}")
    for i in range(min(5, len(ocr_result))):
        coords, (text, confidence) = ocr_result[i]
        print(f"Text {i}: {text} (Confidence: {confidence:.4f})")

    # 5. 可视化并保存结果
    try:
        image = Image.open(image_path).convert('RGB')
        boxes = [line[0] for line in ocr_result]
        txts = [line[1][0] for line in ocr_result]
        scores = [line[1][1] for line in ocr_result]
        
        im_show = draw_ocr_custom(image, boxes, txts, scores)
        im_show.save('visualized_result.jpg')
        print("可视化结果已保存至 visualized_result.jpg")
    except Exception as e:
        print(f"可视化保存失败: {e}")
