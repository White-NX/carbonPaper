"""Unified logging configuration — outputs to stderr only (captured by Rust and written to file)."""

import logging
import sys


def setup_logging():
    """Configure Python logging to output to stderr only.

    The format omits timestamps because the Rust tracing layer adds them.
    """
    formatter = logging.Formatter(
        '[%(levelname)s] %(name)s: %(message)s'
    )
    handler = logging.StreamHandler(sys.stderr)
    handler.setFormatter(formatter)

    root = logging.getLogger()
    root.setLevel(logging.DEBUG)
    root.addHandler(handler)
