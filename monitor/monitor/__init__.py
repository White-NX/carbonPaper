"""Monitor package entry point.

Provides ``start()`` / ``stop()`` and the IPC command dispatcher that
bridges Rust - Python communication.
"""

from . import config
from .config import (
    paused_event,
    stop_event,
    INTERVAL,
    update_exclusion_settings,
    get_exclusion_settings,
    _get_process_icon_base64,
    update_clustering_resource_config,
    update_feature_config,
)
from .clustering_commands import handle_clustering_command
from legacy_clip_export import LegacyClipVectorExporter
from .ipc_pipe import start_pipe_server
import os
import uuid
import base64
import json
import logging
import time
import threading

logger = logging.getLogger(__name__)

# Short cache for explicit/manual compatibility commands. Rust owns all
# periodic background admission; this is not an authentication monitor.
AUTH_STATUS_CACHE_INTERVAL_SECS = 2.0

_server = None
_model_worker = None        # Classification/postprocess worker proxy
_classifier = None           # ClassificationService instance
_clip_exporter = None        # Read-only legacy Chroma exporter
_clustering_manager = None   # HotColdManager instance
_clustering_scheduler = None # compatibility facade; no background timer
_clustering_scheduler_active = False
_last_clustering_auth_check = 0.0
_last_clustering_session_valid = False
_auth_token = None           # Auth token for IPC validation
_last_seq_no = -1            # Last processed sequence number
_seen_seq_nos = set()        # Accepted sequence numbers inside the replay window
_seq_lock = threading.Lock()
_SEQ_REPLAY_WINDOW = 4096
_storage_pipe = None         # Storage service pipe name

# Cache for dynamically extracted icons by process name
_dynamic_icon_cache = {}


def _is_storage_session_valid(force: bool = False) -> bool:
    """Return whether Rust credential session is unlocked (cached for a short period)."""
    global _last_clustering_auth_check, _last_clustering_session_valid

    if _storage_pipe is None:
        # No storage IPC configured: treat as available to avoid disabling clustering.
        return True

    now = time.perf_counter()
    if (not force) and (now - _last_clustering_auth_check < AUTH_STATUS_CACHE_INTERVAL_SECS):
        return _last_clustering_session_valid

    _last_clustering_auth_check = now
    try:
        from storage_client import get_storage_client
        sc = get_storage_client()
        if not sc:
            _last_clustering_session_valid = False
            return False
        _last_clustering_session_valid = bool(sc.is_session_valid())
        return _last_clustering_session_valid
    except Exception as exc:
        logger.debug('Failed to query storage auth status: %s', exc)
        _last_clustering_session_valid = False
        return False


def _sync_clustering_scheduler_auth_gate(force: bool = False) -> bool:
    """Compatibility auth probe for explicit/manual IPC commands.

    Rust owns the periodic scheduler now; this helper only reports the live UI
    session for legacy commands that still require an interactive unlock.
    """
    # The Python object is only a compatibility facade. Reporting it as an
    # active scheduler would make status consumers believe the retired timer
    # and authentication monitor are still running.
    global _clustering_scheduler_active
    _clustering_scheduler_active = False
    return _is_storage_session_valid(force=force)


def _cached_clustering_session_valid() -> bool:
    if not _clustering_scheduler:
        return False
    if _storage_pipe is None:
        return True
    return _last_clustering_session_valid


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
        if _model_worker:
            _model_worker.pause()
        return {'status': 'paused'}

    if cmd in ('resume', 'continue'):
        paused_event.clear()
        if _model_worker:
            _model_worker.resume()
        return {'status': 'resumed'}

    if cmd == 'stop':
        stop_event.set()
        paused_event.clear()
        if _model_worker:
            _model_worker.stop()
        return {'status': 'stopped'}

    if cmd == 'status':
        status = {
            'paused': paused_event.is_set(),
            'stopped': stop_event.is_set(),
            'interval': INTERVAL,
            'clustering_auth_unlocked': _cached_clustering_session_valid(),
            'clustering_scheduler_active': _clustering_scheduler_active,
        }
        if _model_worker:
            status['postprocess_stats'] = _model_worker.get_stats()
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
        allow_full_low_memory = bool(req.get(
            'clustering_allow_full_low_memory',
            getattr(config, 'CLUSTERING_ALLOW_FULL_LOW_MEMORY', False),
        ))
        update_clustering_resource_config(allow_full_low_memory)
        return {
            'status': 'success',
            'clustering_allow_full_low_memory': allow_full_low_memory,
        }

    if cmd == 'update_feature_config':
        clustering_enabled = req.get('clustering_enabled', True)
        classification_enabled = req.get('classification_enabled', True)
        update_feature_config(clustering_enabled, classification_enabled)
        # The classification worker snapshots its feature config from the
        # environment at startup; forward the change so jobs it dequeues from
        # here on honour the new setting without an app restart.
        worker_result = None
        if _model_worker is not None and hasattr(_model_worker, 'update_feature_config'):
            try:
                worker_result = _model_worker.update_feature_config(
                    clustering_enabled, classification_enabled
                )
            except Exception as exc:
                logger.warning('Feature-config sync to model worker failed: %s', exc)
                worker_result = {'status': 'deferred', 'error': str(exc)}
        return {
            'status': 'success',
            'clustering_enabled': clustering_enabled,
            'classification_enabled': classification_enabled,
            'worker_sync': worker_result,
        }

    if cmd == 'enqueue_ocr_postprocess':
        screenshot_id = req.get('screenshot_id')
        if screenshot_id is None:
            return {'error': 'screenshot_id is required'}
        if not _model_worker or not hasattr(_model_worker, 'request'):
            return {'error': 'Classification postprocess service is not initialised'}
        timeout_secs = int(req.get('timeout_secs', 120) or 120)
        try:
            # Sensitive-content filtering and classification only. The semantic
            # index used to be fed from here too, by handing the same payload to
            # the clustering ingest queue; M2.5 step 5 moved that to the Rust
            # capture path, which enqueues the screenshot the moment its OCR row
            # commits and encodes it while the machine is idle.
            return _model_worker.request(
                'enqueue_ocr_postprocess',
                {'request': req},
                timeout=max(30, min(600, timeout_secs)),
            )
        except Exception as e:
            logger.error(
                '[DIAG:enqueue_ocr_postprocess] failed screenshot_id=%s error=%s',
                screenshot_id,
                e,
                exc_info=True,
            )
            return {'error': str(e)}

    # ----- Classification commands -----
    if cmd == 'classify':
        title = req.get('title', '')
        ocr_text = req.get('ocr_text', '')
        process_name = req.get('process_name', '')
        if not _classifier or not hasattr(_classifier, 'classify'):
            return {'error': 'Classification service not initialised'}
        try:
            category, confidence = _classifier.classify(
                title=title,
                ocr_text=ocr_text,
                process_name=process_name,
            )
            return {
                'status': 'success',
                'category': category,
                'category_confidence': round(confidence, 4),
            }
        except Exception as e:
            return {'error': str(e)}

    if cmd == 'classify_debug':
        title = req.get('title', '')
        ocr_text = req.get('ocr_text', '')
        process_name = req.get('process_name', '')
        if not _classifier:
            return {'error': 'Classification service not initialised'}
        try:
            debug = _classifier.classify_debug(
                title=title,
                ocr_text=ocr_text,
                process_name=process_name,
            )
            return {'status': 'success', **debug}
        except Exception as e:
            return {'error': str(e)}

    if cmd == 'add_anchor':
        category = req.get('category', '')
        title = req.get('title', '')
        ocr_text = req.get('ocr_text', '')
        old_category = req.get('old_category')  # None or string
        process_name = req.get('process_name', '')
        if not _classifier:
            return {'error': 'Classification service not initialised'}
        if not category or not title:
            return {'error': 'category and title are required'}
        try:
            result = _classifier.add_anchor(
                category=category,
                title=title,
                ocr_text=ocr_text,
                old_category=old_category,
                process_name=process_name,
            )
            return {'status': 'success', **result}
        except Exception as e:
            return {'error': str(e)}

    if cmd == 'remove_anchor':
        category = req.get('category', '')
        title = req.get('title', '')
        if not _classifier:
            return {'error': 'Classification service not initialised'}
        try:
            removed = _classifier.remove_anchor(category, title)
            return {'status': 'success', 'removed': removed}
        except Exception as e:
            return {'error': str(e)}

    if cmd == 'remove_local_anchors_by_process':
        category = req.get('category', '')
        process_name = req.get('process_name', '')
        if not _classifier:
            return {'error': 'Classification service not initialised'}
        if not category or not process_name:
            return {'error': 'category and process_name are required'}
        try:
            removed_count = _classifier.remove_local_anchors_by_process(category, process_name)
            return {'status': 'success', 'removed_count': removed_count}
        except Exception as e:
            return {'error': str(e)}

    if cmd == 'get_categories':
        if not _classifier:
            return {'error': 'Classification service not initialised'}
        return {
            'status': 'success',
            'categories': _classifier.get_categories(),
        }

    if cmd == 'get_anchors':
        if not _classifier:
            return {'error': 'Classification service not initialised'}
        return {
            'status': 'success',
            'anchors': _classifier.get_anchors(),
        }

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

    # ----- Legacy CLIP Chroma snapshot export (read-only) -----
    if cmd in (
        'start_clip_vectors_export',
        'get_clip_vectors_export_status',
        'export_clip_vectors_page',
        'finish_clip_vectors_export',
    ):
        if not _clip_exporter:
            return {'error': 'Legacy CLIP collection is unavailable'}
        if not _sync_clustering_scheduler_auth_gate(force=True):
            return {'error': 'AUTH_REQUIRED: the CLIP export requires an unlocked session'}
        export_id = req.get('export_id', '')
        try:
            if cmd == 'start_clip_vectors_export':
                return {'status': 'success', **_clip_exporter.start(export_id)}
            if cmd == 'get_clip_vectors_export_status':
                return {'status': 'success', **_clip_exporter.status(export_id)}
            if cmd == 'export_clip_vectors_page':
                return {
                    'status': 'success',
                    **_clip_exporter.page(
                        export_id,
                        cursor=req.get('cursor', 0),
                        limit=req.get('limit', 128),
                    ),
                }
            return {
                'status': 'success',
                'released': _clip_exporter.finish(export_id),
            }
        except Exception as exc:
            logger.exception('%s failed', cmd)
            return {'error': str(exc)}

    clustering_response = handle_clustering_command(
        req,
        scheduler=_clustering_scheduler,
        manager=_clustering_manager,
        auth_gate=_sync_clustering_scheduler_auth_gate,
    )
    if clustering_response is not None:
        return clustering_response

    return {'error': 'unknown command'}


# ---------------------------------------------------------------------------
# Service lifecycle
# ---------------------------------------------------------------------------

def start(_debug, pipe_name: str = None, auth_token: str = None, storage_pipe: str = None):
    """Start the IPC server and initialise classification/postprocess services.

    Args:
        _debug: Debug mode flag.
        pipe_name: Named pipe name (generated if not provided).
        auth_token: Authentication token for IPC validation.
        storage_pipe: Storage service pipe name (Rust reverse IPC).
    """
    global _server, _model_worker, _classifier, _clip_exporter, _storage_pipe, _clustering_manager, _clustering_scheduler, _clustering_scheduler_active, _auth_token, _last_seq_no, _last_clustering_auth_check, _last_clustering_session_valid

    _auth_token = auth_token
    with _seq_lock:
        _last_seq_no = -1
        _seen_seq_nos.clear()
    _storage_pipe = storage_pipe
    _clustering_scheduler_active = False
    _last_clustering_auth_check = 0.0
    _last_clustering_session_valid = False

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

    from .worker_process import RestartableModelWorker

    try:
        _clip_exporter = LegacyClipVectorExporter(shared_chroma_client)
    except Exception as exc:
        logger.warning('Legacy CLIP export unavailable (non-fatal): %s', exc)
        _clip_exporter = None

    worker_env = {
        'CARBONPAPER_CLUSTERING_ENABLED': str(config.CLUSTERING_ENABLED),
        'CARBONPAPER_CLASSIFICATION_ENABLED': str(config.CLASSIFICATION_ENABLED),
        'CARBONPAPER_CLUSTERING_ALLOW_FULL_LOW_MEMORY': str(config.CLUSTERING_ALLOW_FULL_LOW_MEMORY),
    }
    _model_worker = RestartableModelWorker(
        storage_pipe=storage_pipe,
        data_dir=get_data_dir(),
        env=worker_env,
    )
    _classifier = _model_worker
    logger.info('Restartable model worker proxy initialised')

    # Initialise task clustering service (MiniLM + HDBSCAN). Rust owns all
    # periodic scheduling; Python only executes an explicit IPC request.
    try:
        from task_clustering import HotColdManager, ClusteringScheduler

        if shared_chroma_client is not None:
            sc = None
            if storage_pipe:
                from storage_client import get_storage_client
                sc = get_storage_client()

            _clustering_manager = HotColdManager(shared_chroma_client, storage_client=sc)
            _clustering_scheduler = ClusteringScheduler(_clustering_manager, storage_client=sc)
            logger.info('Task clustering service initialised (Rust scheduler)')
        else:
            logger.warning('Task clustering service skipped: shared ChromaDB client is None')
            _clustering_manager = None
            _clustering_scheduler = None
    except Exception as e:
        logger.warning('Task clustering service failed to initialise (non-fatal): %s', e)
        _clustering_manager = None
        _clustering_scheduler = None

    # Screenshot capture, OCR, semantic/CLIP inference, and Smart Cluster
    # scoring are handled by Rust. Python provides classification orchestration,
    # task clustering, Presidio, and legacy read-only migration export.

    return _server


def stop():
    """Shut down classification/postprocess services and the IPC server."""
    stop_event.set()
    try:
        from .presidio_worker import get_presidio_worker
        get_presidio_worker().stop()
    except Exception:
        pass
    if _model_worker:
        try:
            _model_worker.stop()
        except Exception:
            pass
    if _server:
        try:
            _server.shutdown()
        except Exception:
            pass
