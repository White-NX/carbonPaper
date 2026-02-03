import os
import urllib.request
import tarfile

def download_and_extract():
    # 1. 定义模型下载地址 (PP-OCRv3 中文超轻量版)
    models = {
        "det": "https://paddleocr.bj.bcebos.com/PP-OCRv3/chinese/ch_PP-OCRv3_det_infer.tar",
        "cls": "https://paddleocr.bj.bcebos.com/dygraph_v2.0/ch/ch_ppocr_mobile_v2.0_cls_infer.tar",
        "rec": "https://paddleocr.bj.bcebos.com/PP-OCRv3/chinese/ch_PP-OCRv3_rec_infer.tar"
    }
    
    # 2. 字典文件地址 (识别文字必需)
    dict_url = "https://raw.githubusercontent.com/PaddlePaddle/PaddleOCR/release/2.7/ppocr/utils/ppocr_keys_v1.txt"
    
    target_dir = "./models"
    if not os.path.exists(target_dir):
        os.makedirs(target_dir)
        print(f"创建目录: {target_dir}")

    # 下载并解压模型
    for name, url in models.items():
        filename = url.split('/')[-1]
        filepath = os.path.join(target_dir, filename)
        
        print(f"--- 正在下载 {name} 模型 ---")
        if not os.path.exists(filepath):
            urllib.request.urlretrieve(url, filepath)
            print(f"下载完成: {filename}")
        else:
            print(f"文件已存在，跳过下载: {filename}")

        # 解压
        print(f"正在解压 {filename}...")
        with tarfile.open(filepath, "r") as tar:
            tar.extractall(path=target_dir)
        
        # 删除压缩包以节省空间（可选）
        # os.remove(filepath)

    # 下载字典文件
    print("--- 正在下载字典文件 ---")
    dict_path = os.path.join(target_dir, "ppocr_keys_v1.txt")
    if not os.path.exists(dict_path):
        try:
            urllib.request.urlretrieve(dict_url, dict_path)
            print("字典文件下载完成")
        except:
            print("字典下载失败，请手动从 GitHub 下载 ppocr_keys_v1.txt 并放入 models 文件夹")

    print("\n所有模型已就绪！")
    print(f"路径清单:")
    print(f"检测模型: {os.path.abspath(target_dir + '/ch_PP-OCRv3_det_infer')}")
    print(f"分类模型: {os.path.abspath(target_dir + '/ch_ppocr_mobile_v2.0_cls_infer')}")
    print(f"识别模型: {os.path.abspath(target_dir + '/ch_PP-OCRv3_rec_infer')}")
    print(f"字典文件: {os.path.abspath(dict_path)}")

if __name__ == "__main__":
    download_and_extract()