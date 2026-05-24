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
    
    # 1. OCR Engine
    if 'ocr_engine' in sys.modules:
        try:
            from ocr_engine import OCREngine
            if OCREngine._instance is not None and getattr(OCREngine._instance, '_initialized', False):
                loaded.append("PaddleOCR (RapidOCR)")
        except Exception:
            pass
            
    # 2. Chinese-CLIP
    if 'vector_store' in sys.modules:
        try:
            from vector_store import ChineseCLIPSingleton
            if ChineseCLIPSingleton._instance is not None and getattr(ChineseCLIPSingleton._instance, '_initialized', False):
                loaded.append("Chinese-CLIP")
        except Exception:
            pass
            
    # 3. BGE (Classifier)
    if 'classifier' in sys.modules:
        try:
            from classifier import TextEmbedder
            if TextEmbedder._instance is not None and TextEmbedder._instance._model is not None:
                loaded.append("BGE-small-zh-v1.5")
        except Exception:
            pass
            
    # 4. MiniLM (Clustering)
    if 'task_clustering' in sys.modules:
        try:
            from task_clustering import TaskEmbedder
            if TaskEmbedder._instance is not None and TaskEmbedder._instance._model is not None:
                loaded.append("MiniLM-L12-v2")
        except Exception:
            pass
            
    # 5. Reranker (Smart Clustering)
    if 'reranker' in sys.modules:
        try:
            from reranker import Reranker
            if Reranker._instance is not None and Reranker._instance._session is not None:
                loaded.append("bge-reranker-v2-m3")
        except Exception:
            pass

    loaded_str = ", ".join(loaded) if loaded else "None"
    msg = f"[MODEL_LOADING] Loading: {loading_model_name} | Already loaded in memory: [{loaded_str}]"
    print(msg, file=sys.stderr, flush=True)
    logging.getLogger("model_loading").info(msg)
