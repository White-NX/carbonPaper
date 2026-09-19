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
)
from legacy_vector_export import LegacyVectorExporter
import os
import uuid
import logging
import threading

logger = logging.getLogger(__name__)

_server = None
_clip_exporter = None        # Read-only legacy Chroma exporter
_minilm_exporter = None      # Read-only legacy MiniLM exporter
_auth_token = None           # Auth token for IPC validation
_last_seq_no = -1            # Last processed sequence number
_seen_seq_nos = set()        # Accepted sequence numbers inside the replay window
_seq_lock = threading.Lock()
_SEQ_REPLAY_WINDOW = 4096
_storage_pipe = None         # Storage service pipe name

# Cache for dynamically extracted icons by process name
_dynamic_icon_cache = {}


def _is_storage_session_valid() -> bool:
    """Probe the Rust credential session for each legacy export request."""
    if _storage_pipe is None:
        return True

    try:
        from storage_client import get_storage_client
        sc = get_storage_client()
        return bool(sc and sc.is_session_valid())
    except Exception as exc:
        logger.debug('Failed to query storage auth status: %s', exc)
        return False


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
    """Dispatch command (with diagnostic timing). Security: PID verification at transport layer + auth token at application layer."""
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
        logger.warning('Auth failed: token_present=%s', bool(req_token))
        return {'error': 'Authentication failed: Invalid token'}

    # Replay-attack prevention
    if req_seq_no is not None:
        if not isinstance(req_seq_no, int) or isinstance(req_seq_no, bool) or req_seq_no < 0:
            return {'error': 'Authentication failed: Invalid sequence number type'}
        with _seq_lock:
            minimum_retained = max(0, _last_seq_no - _SEQ_REPLAY_WINDOW + 1)
            if req_seq_no in _seen_seq_nos or req_seq_no < minimum_retained:
                return {
                    'error': (
                        'Authentication failed: Replayed or expired sequence number '
                        f'(got {req_seq_no}, highest {_last_seq_no})'
                    )
                }
            _seen_seq_nos.add(req_seq_no)
            if req_seq_no > _last_seq_no:
                _last_seq_no = req_seq_no
                cutoff = max(0, _last_seq_no - _SEQ_REPLAY_WINDOW + 1)
                _seen_seq_nos.difference_update(
                    seq for seq in tuple(_seen_seq_nos) if seq < cutoff
                )

    cmd = (req.get('command') or '').lower()

    # ----- Lifecycle commands -----
    if cmd == 'pause':
        paused_event.set()
        return {'status': 'paused'}

    if cmd in ('resume', 'continue'):
        paused_event.clear()
        return {'status': 'resumed'}

    if cmd == 'stop':
        stop_event.set()
        paused_event.clear()
        return {'status': 'stopped'}

    if cmd == 'status':
        status = {
            'paused': paused_event.is_set(),
            'stopped': stop_event.is_set(),
            'interval': INTERVAL,
        }
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

    # ----- Presidio PII detection commands -----
    if cmd == 'presidio_analyze':
        texts = req.get('texts', [])
        language = req.get('language', 'zh-CN')
        entity_types = req.get('entity_types')
        if not isinstance(texts, list) or len(texts) == 0:
            return {'error': 'texts must be a non-empty list'}
        try:
            from .presidio_worker import get_presidio_worker
            results = get_presidio_worker().analyze(
                texts,
                language,
                entity_types,
                timeout=float(req.get('timeout_secs', 14.0)),
            )
            return {
                'status': 'success',
                'results': results,
            }
        except TimeoutError as e:
            logger.warning('presidio_analyze timeout: %s', e)
            return {'error': str(e)}
        except Exception as e:
            logger.error('presidio_analyze failed: %s', e)
            return {'error': str(e)}

    if cmd == 'presidio_set_language':
        language = req.get('language', 'zh-CN')
        try:
            from .presidio_worker import get_presidio_worker
            result = get_presidio_worker().request(
                {'command': 'set_language', 'language': language},
                timeout=5.0,
            )
            if result.get('status') != 'success':
                return {'error': result.get('error', 'presidio_set_language failed')}
            return {
                'status': 'success',
                'ok': True,
                'language': language,
            }
        except Exception as e:
            logger.error('presidio_set_language failed: %s', e)
            return {'error': str(e)}

    if cmd == 'presidio_status':
        try:
            from .presidio_worker import get_presidio_worker
            result = get_presidio_worker().status()
            if result.get('status') != 'success':
                return {'status': 'success', 'loaded': False, 'language': None, 'model': 'none'}
            return {
                'status': 'success',
                'loaded': bool(result.get('initialized')),
                'language': result.get('language'),
                'model': result.get('model') or 'none',
                'watchdog': get_presidio_worker().status_snapshot(),
            }
        except Exception as e:
            return {'status': 'success', 'loaded': False, 'language': None, 'model': 'none'}

    if cmd == 'presidio_unload':
        try:
            from .presidio_worker import get_presidio_worker
            result = get_presidio_worker().unload()
            if result.get('status') != 'success':
                return {'error': result.get('error', 'presidio_unload failed')}
            return {'status': 'success', 'unloaded': True}
        except Exception as e:
            logger.error('presidio_unload failed: %s', e)
            return {'error': str(e)}

    if cmd == 'presidio_check_idle':
        try:
            from .presidio_worker import get_presidio_worker
            return get_presidio_worker().check_idle()
        except Exception as e:
            logger.error('presidio_check_idle failed: %s', e)
            return {'error': str(e)}

    # ----- Legacy Chroma snapshot exports (read-only) -----
    if cmd in (
        'start_clip_vectors_export',
        'get_clip_vectors_export_status',
        'export_clip_vectors_page',
        'finish_clip_vectors_export',
        'start_task_vectors_export',
        'get_task_vectors_export_status',
        'export_task_vectors_page',
        'finish_task_vectors_export',
    ):
        exporter = _clip_exporter if 'clip_vectors' in cmd else _minilm_exporter
        if not exporter:
            return {'error': 'Legacy vector collection is unavailable'}
        if not _is_storage_session_valid():
            return {'error': 'AUTH_REQUIRED: vector export requires an unlocked session'}
        export_id = req.get('export_id', '')
        try:
            if cmd.startswith('start_'):
                return {'status': 'success', **exporter.start(export_id)}
            if cmd.startswith('get_'):
                return {'status': 'success', **exporter.status(export_id)}
            if cmd.startswith('export_'):
                return {
                    'status': 'success',
                    **exporter.page(
                        export_id,
                        cursor=req.get('cursor', 0),
                        limit=req.get('limit', 128),
                    ),
                }
            return {
                'status': 'success',
                'released': exporter.finish(export_id),
            }
        except Exception as exc:
            logger.exception('%s failed', cmd)
            return {'error': str(exc)}

    return {'error': 'unknown command'}


# ---------------------------------------------------------------------------
# Service lifecycle
# ---------------------------------------------------------------------------

def start(_debug, pipe_name: str = None, auth_token: str = None, storage_pipe: str = None):
    """Start the IPC server and initialise the legacy vector export services.

    Args:
        _debug: Debug mode flag.
        pipe_name: Named pipe name (generated if not provided).
        auth_token: Authentication token for IPC validation.
        storage_pipe: Storage service pipe name (Rust reverse IPC).
    """
    global _server, _clip_exporter, _minilm_exporter, _storage_pipe, _auth_token, _last_seq_no

    _auth_token = auth_token
    with _seq_lock:
        _last_seq_no = -1
        _seen_seq_nos.clear()
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

    # Use standard named pipe server with PID verification
    if _server is None:
        from .ipc_pipe import start_pipe_server
        _server = start_pipe_server(handler=_handle_command, pipe_name=pipe_name)

    # --- Single Shared ChromaDB Client ---
    try:
        import chromadb
        from chromadb.config import Settings as ChromaSettings
        chroma_path = os.path.join(get_data_dir(), 'chroma_db')
        shared_chroma_client = chromadb.PersistentClient(
            path=chroma_path,
            settings=ChromaSettings(anonymized_telemetry=False),
        )
    except Exception as e:
        logger.error("Failed to initialize shared ChromaDB client: %s", e)
        shared_chroma_client = None

    try:
        _clip_exporter = LegacyVectorExporter(shared_chroma_client, kind='clip')
        _minilm_exporter = LegacyVectorExporter(shared_chroma_client, kind='minilm')
    except Exception as exc:
        logger.warning('Legacy vector export unavailable (non-fatal): %s', exc)
        _clip_exporter = None
        _minilm_exporter = None

    # Screenshot capture, OCR, semantic/CLIP inference, and Smart Cluster
    # scoring and category classification are handled by Rust. Python provides
    # Presidio and legacy read-only migration export.

    return _server


def stop():
    """Shut down the Presidio worker and IPC server."""
    stop_event.set()
    try:
        from .presidio_worker import get_presidio_worker
        get_presidio_worker().stop()
    except Exception:
        pass
    if _server:
        try:
            _server.shutdown()
        except Exception:
            pass
