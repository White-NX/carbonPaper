"""统一日志配置 — 仅输出到 stderr（由 Rust 捕获并写入文件）"""

import logging
import sys


def setup_logging():
    """配置 Python 日志，仅输出到 stderr。

    格式不含时间戳，因为 Rust 的 tracing 层会统一添加时间戳。
    """
    formatter = logging.Formatter(
        '[%(levelname)s] %(name)s: %(message)s'
    )
    handler = logging.StreamHandler(sys.stderr)
    handler.setFormatter(formatter)

    root = logging.getLogger()
    root.setLevel(logging.DEBUG)
    root.addHandler(handler)
