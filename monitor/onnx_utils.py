import os
import logging
import numpy as np
import threading

logger = logging.getLogger(__name__)

_TRUE_VALUES = ("1", "true", "yes", "on")
_ONNX_CONSTRUCT_LOCK = threading.Lock()

def get_data_dir() -> str:
    """Return the application data directory."""
    env_dir = os.environ.get('CARBONPAPER_DATA_DIR')
    if env_dir:
        return env_dir

    local_appdata = os.environ.get('LOCALAPPDATA')
    if not local_appdata:
        raise RuntimeError('LOCALAPPDATA environment variable not set')
    return os.path.join(local_appdata, 'CarbonPaper', 'data')

def is_onnx_testing_enabled() -> bool:
    """Check if the ONNX inference testing is enabled via environment variable."""
    return os.environ.get("CARBONPAPER_USE_ONNX", "").strip().lower() in _TRUE_VALUES

def get_onnx_model_path(model_dir: str, model_name: str) -> str:
    """Construct and return the absolute path if the ONNX model file exists, else empty string."""
    path = os.path.join(model_dir, model_name)
    if os.path.isfile(path):
        return path
    return ""

def create_onnx_session(onnx_path: str):
    """Create an ONNX InferenceSession with bounded threading.

    Buffer loading is the default because path loading caused severe file-backed
    paging thrash on memory-constrained Windows systems during testing. It does
    raise peak RSS by roughly the ONNX file size, so CARBONPAPER_ONNX_LOAD_MODE=path
    is kept as an escape hatch for environments where peak memory matters more.
    """
    import onnxruntime as ort
    logger.info("Creating ONNX session for %s with anti-thrashing options...", onnx_path)

    sess_options = ort.SessionOptions()
    sess_options.enable_cpu_mem_arena = False
    sess_options.enable_mem_pattern = False
    sess_options.graph_optimization_level = ort.GraphOptimizationLevel.ORT_ENABLE_BASIC
    sess_options.intra_op_num_threads = 1
    sess_options.inter_op_num_threads = 1
    sess_options.add_session_config_entry("session.use_device_allocator_for_initializers", "0")

    available = ort.get_available_providers()
    providers = []
    if (
        os.environ.get("CARBONPAPER_USE_DML", "").strip().lower() in _TRUE_VALUES
        and "DmlExecutionProvider" in available
    ):
        device_id = os.environ.get("CARBONPAPER_DML_DEVICE_ID", "").strip()
        if device_id:
            try:
                providers.append(("DmlExecutionProvider", {"device_id": int(device_id)}))
            except ValueError:
                logger.warning("Ignoring invalid CARBONPAPER_DML_DEVICE_ID=%r", device_id)
                providers.append("DmlExecutionProvider")
        else:
            providers.append("DmlExecutionProvider")
    providers.append("CPUExecutionProvider")

    logger.info("ONNX providers selected: %s for %s", providers, onnx_path)
    load_mode = os.environ.get("CARBONPAPER_ONNX_LOAD_MODE", "buffer").strip().lower()
    
    with _ONNX_CONSTRUCT_LOCK:
        if load_mode == "path":
            return ort.InferenceSession(onnx_path, sess_options=sess_options, providers=providers)

        with open(onnx_path, "rb") as f:
            model_bytes = f.read()
        try:
            return ort.InferenceSession(model_bytes, sess_options=sess_options, providers=providers)
        finally:
            del model_bytes


def build_transformer_inputs(session, encoded: dict) -> dict:
    """Build an ONNX feed from tokenizer output and the session's real inputs."""
    feeds = {}
    input_ids = encoded.get("input_ids")
    if input_ids is None:
        raise ValueError("Tokenizer output did not include input_ids")

    for meta in session.get_inputs():
        name = meta.name
        if name in encoded:
            arr = encoded[name]
        elif name == "token_type_ids":
            arr = np.zeros_like(input_ids)
        else:
            continue

        if "int64" in meta.type and arr.dtype != np.int64:
            arr = arr.astype(np.int64)
        elif "float" in meta.type and arr.dtype != np.float32:
            arr = arr.astype(np.float32)
        feeds[name] = arr

    missing = [i.name for i in session.get_inputs() if i.name not in feeds]
    if missing:
        raise ValueError(f"Required ONNX inputs are missing from feed: {missing}")
    return feeds
