"""入口：仅作为程序接入点，其他逻辑在 `monitor` 包中实现。"""

# [CRITICAL] 必须在导入任何其他涉及 DLL 的库（如 paddleocr, cv2）之前导入 torch
# 否则会出现 WinError 127 (shm.dll dependency missing)
try:
    import torch
except ImportError:
    pass

from monitor import start, stop, stop_event
import argparse


def main():
  # 解析命令行参数
  parser = argparse.ArgumentParser(description='Carbon Monitor Service')
  parser.add_argument('--pipe-name', type=str, help='Named pipe name for IPC')
  parser.add_argument('--auth-token', type=str, help='Authentication token for IPC')
  parser.add_argument('--storage-pipe', type=str, help='Named pipe name for storage IPC (Rust storage service)')
  args = parser.parse_args()

  # 确保管道名和认证 token 都已提供
  if not args.pipe_name or not args.auth_token:
    print('错误: 必须提供 --pipe-name 和 --auth-token 参数')
    return 1

  print(f'启动监控服务: pipe={args.pipe_name}, token={args.auth_token[:16]}..., storage_pipe={args.storage_pipe}')

  # 初始化存储客户端（如果提供了存储管道名）
  if args.storage_pipe:
    from storage_client import init_storage_client
    storage_client = init_storage_client(args.storage_pipe)
    print(f'存储客户端已初始化: {args.storage_pipe}')

  # 启动模块（包含截图线程与命名管道 IPC 服务器）
  start(_debug=False, pipe_name=args.pipe_name, auth_token=args.auth_token, storage_pipe=args.storage_pipe)
  
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
  import sys
  sys.exit(main() or 0)
