"""Monitor package entry point.

Provides ``start()`` / ``stop()`` and the IPC command dispatcher that
bridges Rust - Python communication.
"""

from .config import (
    paused_event,
    stop_event,
    INTERVAL,
    update_exclusion_settings,
    get_exclusion_settings,
    _get_process_icon_base64,
    update_advanced_capture_config,
)
from .ipc_pipe import start_pipe_server
import os
import uuid
import base64
import json
import logging

logger = logging.getLogger(__name__)

_server = None
_ocr_worker = None          # OCRService instance
_auth_token = None           # Auth token for IPC validation
_last_seq_no = -1            # Last processed sequence number
_storage_pipe = None         # Storage service pipe name

# Cache for dynamically extracted icons by process name
_dynamic_icon_cache = {}


def _delete_vectors_by_hashes(image_hashes):
    """Best-effort delete from vector store using image hashes."""
    if not image_hashes:
        return {"deleted": 0, "requested": 0, "skipped": True}

    if not _ocr_worker or not _ocr_worker.enable_vector_store or not _ocr_worker.vector_store:
        return {"deleted": 0, "requested": len(image_hashes), "skipped": True}

    deleted = 0
    for image_hash in image_hashes:
        if not isinstance(image_hash, str) or not image_hash:
            continue
        try:
            ok = _ocr_worker.vector_store.delete_image(f"memory://{image_hash}")
            if ok:
                deleted += 1
        except Exception:
            # Best-effort; ignore individual failures
            continue

    return {"deleted": deleted, "requested": len(image_hashes), "skipped": False}


def get_data_dir():
    """Return the application data directory."""
    env_dir = os.environ.get('CARBONPAPER_DATA_DIR')
    if env_dir:
        return env_dir

    local_appdata = os.environ.get('LOCALAPPDATA')
    if not local_appdata:
        raise RuntimeError('LOCALAPPDATA environment variable not set')
    return os.path.join(local_appdata, 'CarbonPaper', 'data')


def _find_and_extract_icon(process_name: str):
    """Try to find the exe path for a process name and extract its icon."""
    if not process_name:
        return None

    try:
        import psutil
        process_name_lower = process_name.lower()
        for proc in psutil.process_iter(['name', 'exe']):
            try:
                if proc.info['name'] and proc.info['name'].lower() == process_name_lower:
                    exe_path = proc.info['exe']
                    if exe_path:
                        return _get_process_icon_base64(exe_path)
            except (psutil.NoSuchProcess, psutil.AccessDenied):
                continue
    except Exception:
        pass

    return None


# ---------------------------------------------------------------------------
# IPC command handling
# ---------------------------------------------------------------------------

def _handle_command(req: dict):
    """Validate auth before dispatching (with diagnostic timing)."""
    import time as _time
    _t0 = _time.perf_counter()
    result = _handle_command_impl(req)
    elapsed = _time.perf_counter() - _t0
    cmd = (req.get('command') or '?').lower() if isinstance(req, dict) else '?'
    if elapsed > 5.0:
        logger.warning('[DIAG:CMD-PY] command=%s took %.3fs', cmd, elapsed)
    return result


def _handle_command_impl(req: dict):
    """Actual command dispatch logic."""
    global _last_seq_no

    # Validate auth token
    req_token = req.get('_auth_token')
    req_seq_no = req.get('_seq_no')

    if _auth_token and req_token != _auth_token:
        logger.warning('Auth failed: expected=%s... got=%s...',
                       _auth_token[:16] if _auth_token else 'None',
                       req_token[:16] if req_token else 'None')
        return {'error': 'Authentication failed: Invalid token'}

    # Replay-attack prevention
    if req_seq_no is not None:
        if req_seq_no <= _last_seq_no:
            return {'error': f'Authentication failed: Invalid sequence number (got {req_seq_no}, expected > {_last_seq_no})'}
        _last_seq_no = req_seq_no

    cmd = (req.get('command') or '').lower()

    # ----- Lifecycle commands -----
    if cmd == 'pause':
        paused_event.set()
        if _ocr_worker:
            _ocr_worker.pause()
        return {'status': 'paused'}

    if cmd in ('resume', 'continue'):
        paused_event.clear()
        if _ocr_worker:
            _ocr_worker.resume()
        return {'status': 'resumed'}

    if cmd == 'stop':
        stop_event.set()
        paused_event.clear()
        if _ocr_worker:
            _ocr_worker.stop()
        return {'status': 'stopped'}

    if cmd == 'status':
        status = {
            'paused': paused_event.is_set(),
            'stopped': stop_event.is_set(),
            'interval': INTERVAL,
        }
        if _ocr_worker:
            status['ocr_stats'] = _ocr_worker.get_stats()
        return status

    # ----- Configuration commands -----
    if cmd == 'update_filters':
        filters = req.get('filters', {}) if isinstance(req, dict) else {}
        try:
            update_exclusion_settings(
                processes=filters.get('processes') or req.get('processes'),
                titles=filters.get('titles') or req.get('titles'),
                ignore_protected=filters.get('ignore_protected') if 'ignore_protected' in filters else req.get('ignore_protected'),
            )
            return {'status': 'success', 'filters': get_exclusion_settings()}
        except Exception as e:
            return {'error': str(e)}

    if cmd == 'update_advanced_config':
        capture_on_ocr_busy = bool(req.get('capture_on_ocr_busy', False))
        ocr_queue_max_size = int(req.get('ocr_queue_max_size', 1))
        update_advanced_capture_config(capture_on_ocr_busy, ocr_queue_max_size)
        return {
            'status': 'success',
            'capture_on_ocr_busy': capture_on_ocr_busy,
            'ocr_queue_max_size': ocr_queue_max_size,
        }

    # ----- Vector search -----
    if cmd == 'search_nl':
        query = req.get('query', '')
        limit = req.get('limit', 10)
        offset = req.get('offset', 0)
        process_names = req.get('process_names') or None
        start_time = req.get('start_time')
        end_time = req.get('end_time')

        if not _ocr_worker or not _ocr_worker.enable_vector_store:
            return {'error': 'Vector store not enabled'}

        if isinstance(process_names, list):
            process_names = [p for p in process_names if isinstance(p, str) and p.strip()]
            if len(process_names) == 0:
                process_names = None
        else:
            process_names = None

        def _normalize_timestamp(value):
            if value in (None, ''):
                return None
            try:
                return float(value)
            except (TypeError, ValueError):
                return None

        start_time = _normalize_timestamp(start_time)
        end_time = _normalize_timestamp(end_time)

        try:
            results = _ocr_worker.search_by_natural_language(
                query,
                n_results=limit,
                offset=offset,
                process_names=process_names,
                start_time=start_time,
                end_time=end_time,
            )
        except Exception as exc:
            return {'error': str(exc)}

        return {'status': 'success', 'results': results}

    # ----- Deletion (vector store cleanup; DB deletion handled by Rust) -----
    if cmd == 'delete_screenshot':
        image_hashes = []
        image_hash = req.get('image_hash')
        if isinstance(image_hash, str) and image_hash:
            image_hashes.append(image_hash)
        vector_info = _delete_vectors_by_hashes(image_hashes)
        return {
            'status': 'success',
            'vector_deleted': vector_info.get('deleted', 0),
        }

    if cmd == 'delete_by_time_range':
        image_hashes = req.get('image_hashes')
        if not isinstance(image_hashes, list):
            image_hashes = []
        vector_info = _delete_vectors_by_hashes(image_hashes)
        return {
            'status': 'success',
            'vector_deleted': vector_info.get('deleted', 0),
        }

    # ----- OCR processing (called by Rust capture loop) -----
    if cmd == 'process_ocr':
        screenshot_id = req.get('screenshot_id')
        if screenshot_id is None:
            return {'error': 'screenshot_id is required'}
        if not _ocr_worker:
            return {'error': 'OCR service not initialised'}

        try:
            from PIL import Image
            import io as _io
            from storage_client import get_storage_client

            # Fetch decrypted image from Rust via reverse IPC
            sc = get_storage_client()
            if not sc:
                return {'error': 'Storage client not available'}

            resp = sc._send_request({
                'command': 'get_temp_image',
                'screenshot_id': screenshot_id,
            })

            if resp.get('status') != 'success':
                return {'error': f"Failed to fetch image: {resp.get('error', 'unknown')}"}

            image_data_b64 = resp.get('data', {}).get('image_data')
            if not image_data_b64:
                return {'error': 'No image data returned from storage'}

            image_bytes = base64.b64decode(image_data_b64)
            image_pil = Image.open(_io.BytesIO(image_bytes))

            # Run OCR
            ocr_results = _ocr_worker.ocr_engine.recognize(image_pil)
            filtered = [r for r in ocr_results if r.get('confidence', 0) >= 0.5]

            # Update stats
            _ocr_worker.stats['processed_count'] += 1
            _ocr_worker.stats['total_texts_found'] += len(filtered)

            # Add to vector store (if enabled)
            if _ocr_worker.enable_vector_store and _ocr_worker.vector_store:
                image_hash = req.get('image_hash', '')
                ocr_text = ' '.join([r.get('text', '') for r in filtered])
                if ocr_text.strip():
                    try:
                        _ocr_worker.vector_store.add_image(
                            image_path=f"memory://{image_hash}",
                            image=image_pil,
                            metadata={
                                'window_title': req.get('window_title', ''),
                                'process_name': req.get('process_name', ''),
                                'timestamp': req.get('timestamp', 0),
                            },
                            ocr_text=ocr_text,
                        )
                    except Exception as ve:
                        logger.warning('Vector store add failed: %s', ve)

            return {
                'status': 'success',
                'ocr_results': filtered,
            }
        except Exception as e:
            logger.error('process_ocr failed: %s', e)
            _ocr_worker.stats['failed_count'] += 1
            return {'error': str(e)}

    return {'error': 'unknown command'}


# ---------------------------------------------------------------------------
# Service lifecycle
# ---------------------------------------------------------------------------

def start(_debug, pipe_name: str = None, auth_token: str = None, storage_pipe: str = None):
    """Start the IPC server and initialise the OCR service.

    Args:
        _debug: Debug mode flag.
        pipe_name: Named pipe name (generated if not provided).
        auth_token: Authentication token for IPC validation.
        storage_pipe: Storage service pipe name (Rust reverse IPC).
    """
    global _server, _ocr_worker, _auth_token, _last_seq_no, _storage_pipe

    _auth_token = auth_token
    _last_seq_no = -1
    _storage_pipe = storage_pipe

    if not pipe_name:
        pipe_name = os.environ.get('CARBON_MONITOR_PIPE')

    if not pipe_name:
        pipe_name = f'carbon_monitor_{uuid.uuid4().hex}'
        print(pipe_name, flush=True)

    if _debug:
        try:
            with open('monitor_pipe_name.txt', 'w', encoding='utf-8') as f:
                f.write(pipe_name)
        except Exception as e:
            logger.warning('Debug mode enabled but unable to write pipe-name file: %s', e)

    # Prefer inherited handle from parent process (more secure)
    handle_val = os.environ.get('CARBON_MONITOR_PIPE_HANDLE')
    if handle_val:
        try:
            handle_int = int(handle_val, 0)
            from .ipc_pipe import start_inherited_handle_server
            _server = start_inherited_handle_server(handler=_handle_command, handle_value=handle_int)
        except Exception as e:
            logger.error('Failed to use inherited handle for IPC server, falling back to named pipe: %s', e)

    if _server is None:
        _server = start_pipe_server(handler=_handle_command, pipe_name=pipe_name)

    # Lazy import to avoid triggering heavy model loading before IPC is ready
    from ocr_service import OCRService

    logger.info("Initialising OCR service...")
    _ocr_worker = OCRService(
        vector_db_path=os.path.join(get_data_dir(), 'chroma_db'),
        storage_pipe=storage_pipe,
    )
    _ocr_worker.start()

    # NOTE: Screenshot capture loop is handled by Rust (capture.rs).
    # Python only provides OCR via the 'process_ocr' IPC command.

    return _server


def stop():
    """Shut down the OCR service and IPC server."""
    stop_event.set()
    if _ocr_worker:
        try:
            _ocr_worker.stop()
        except Exception:
            pass
    if _server:
        try:
            _server.shutdown()
        except Exception:
            pass
