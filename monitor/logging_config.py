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


def log_model_loading(loading_model_name):
    """Log the model that is being loaded, along with a list of already loaded models."""
    import sys
    loaded = []
    
    # MiniLM (task clustering)
    if 'task_clustering' in sys.modules:
        try:
            from task_clustering import TaskEmbedder
            if TaskEmbedder._instance is not None and TaskEmbedder._instance._model is not None:
                loaded.append("MiniLM-L12-v2")
        except Exception:
            pass

    loaded_str = ", ".join(loaded) if loaded else "None"
    msg = f"[MODEL_LOADING] Loading: {loading_model_name} | Already loaded in memory: [{loaded_str}]"
    print(msg, file=sys.stderr, flush=True)
    logging.getLogger("model_loading").info(msg)
