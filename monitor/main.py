"""入口：仅作为程序接入点，其他逻辑在 `monitor` 包中实现。"""

# [CRITICAL] 必须在导入任何其他涉及 DLL 的库（如 paddleocr, cv2）之前导入 torch
# 否则会出现 WinError 127 (shm.dll dependency missing)
try:
    import torch
except ImportError:
    pass

from monitor import start, stop, stop_event


def main():
  # 启动模块（包含截图线程与命名管道 IPC 服务器，默认管道名 `carbon_monitor_secure`）

  start(_debug=False, pipe_name="carbon_monitor_secure")
  # 主线程仅等待，直到外部通过 IPC 或 Ctrl+C 停止
  try:
    while not stop_event.is_set():
      import time
      time.sleep(0.5)
  except KeyboardInterrupt:
    pass
  finally:
    stop()


if __name__ == '__main__':

  main()