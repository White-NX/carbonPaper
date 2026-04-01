"""Entry point: only serves as the programme entry; all logic lives in the `monitor` package."""

# [CRITICAL] torch MUST be imported before any other DLL-dependent library (e.g. cv2)
# otherwise WinError 127 (shm.dll dependency missing) will occur
try:
    import torch
except ImportError:
    pass

from logging_config import setup_logging
setup_logging()

import logging
logger = logging.getLogger(__name__)

from monitor import start, stop, stop_event
import argparse


def main():
  # Parse command-line arguments
  parser = argparse.ArgumentParser(description='Carbon Monitor Service')
  parser.add_argument('--pipe-name', type=str, help='Named pipe name for IPC')
  parser.add_argument('--auth-token', type=str, help='Authentication token for IPC')
  parser.add_argument('--storage-pipe', type=str, help='Named pipe name for storage IPC (Rust storage service)')
  args = parser.parse_args()

  if not args.pipe_name or not args.auth_token:
    logger.error('--pipe-name and --auth-token arguments are required')
    return 1

  auth_token = args.auth_token

  logger.info(f'Starting monitor service: pipe={args.pipe_name[:30]}, token={auth_token[:16]}..., storage_pipe={args.storage_pipe[:30] if args.storage_pipe else "None"}')

  # Initialise storage client (if a storage pipe name was provided)
  if args.storage_pipe:
    from storage_client import init_storage_client
    storage_client = init_storage_client(args.storage_pipe)
    logger.info(f'Storage client initialised: {args.storage_pipe}')

  # Start the module (named-pipe IPC server)
  from monitor import start
  start(_debug=False, pipe_name=args.pipe_name, auth_token=auth_token, storage_pipe=args.storage_pipe)

  # Main thread blocks until stopped via IPC or Ctrl+C
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
