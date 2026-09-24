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
import os
import uuid
import logging
import threading

logger = logging.getLogger(__name__)

_server = None
_auth_token = None           # Auth token for IPC validation
_last_seq_no = -1            # Last processed sequence number
_seen_seq_nos = set()        # Accepted sequence numbers inside the replay window
_seq_lock = threading.Lock()
_SEQ_REPLAY_WINDOW = 4096
_storage_pipe = None         # Storage service pipe name

# Cache for dynamically extracted icons by process name
_dynamic_icon_cache = {}


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

    return {'error': 'unknown command'}


# ---------------------------------------------------------------------------
# Service lifecycle
# ---------------------------------------------------------------------------

def start(_debug, pipe_name: str = None, auth_token: str = None, storage_pipe: str = None):
    """Start the IPC server.

    Args:
        _debug: Debug mode flag.
        pipe_name: Named pipe name (generated if not provided).
        auth_token: Authentication token for IPC validation.
        storage_pipe: Storage service pipe name (Rust reverse IPC).
    """
    global _server, _storage_pipe, _auth_token, _last_seq_no

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

    # Screenshot capture, OCR, semantic/CLIP inference, Smart Cluster scoring,
    # category classification and personal information detection are handled
    # by Rust. Python serves only the monitor lifecycle and IPC commands above.

    return _server


def stop():
    """Shut down the IPC server."""
    stop_event.set()
    if _server:
        try:
            _server.shutdown()
        except Exception:
            pass
