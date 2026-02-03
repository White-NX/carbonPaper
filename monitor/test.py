import fastdeploy as fd
print("FastDeploy Version:", fd.__version__)

try:
    print("Vision module available:", hasattr(fd, 'vision'))
    # 尝试列出 vision 下的组件
    if hasattr(fd, 'vision'):
        print("OCR modules:", dir(fd.vision.ocr))
except Exception as e:
    print("Error:", e)