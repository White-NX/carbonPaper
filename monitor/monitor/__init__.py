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
    update_advanced_capture_config,
    update_clustering_resource_config,
    update_feature_config,
)
from .clustering_commands import handle_clustering_command
from .ipc_pipe import start_pipe_server
import os
import uuid
import base64
import json
import logging
import queue
import time
import threading

logger = logging.getLogger(__name__)

CLUSTERING_AUTH_POLL_INTERVAL_SECS = 2.0

_server = None
_ocr_worker = None          # OCRService instance
_classifier = None           # ClassificationService instance
_clustering_manager = None   # HotColdManager instance
_clustering_scheduler = None # ClusteringScheduler instance
_clustering_scheduler_active = False  # whether scheduler thread is currently active
_last_clustering_auth_check = 0.0
_last_clustering_session_valid = False
_clustering_auth_monitor_thread = None
_clustering_auth_gate_lock = threading.Lock()
_clustering_ingest_queue = None
_clustering_ingest_thread = None
_clustering_ingest_stop = threading.Event()
_auth_token = None           # Auth token for IPC validation
_last_seq_no = -1            # Last processed sequence number
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
    if (not force) and (now - _last_clustering_auth_check < CLUSTERING_AUTH_POLL_INTERVAL_SECS):
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
    """Start/stop clustering scheduler based on current auth unlock state."""
    global _clustering_scheduler_active

    if not _clustering_scheduler:
        return False

    with _clustering_auth_gate_lock:
        session_valid = _is_storage_session_valid(force=force)

        if session_valid and not _clustering_scheduler_active:
            try:
                _clustering_scheduler.start()
                _clustering_scheduler_active = True
                logger.info('Task clustering scheduler enabled (session unlocked)')
            except Exception as exc:
                logger.warning('Failed to start clustering scheduler: %s', exc)
                _clustering_scheduler_active = False
        elif (not session_valid) and _clustering_scheduler_active:
            try:
                _clustering_scheduler.stop()
            except Exception as exc:
                logger.warning('Failed to stop clustering scheduler: %s', exc)
            _clustering_scheduler_active = False
            logger.info('Task clustering scheduler disabled (session locked)')

    return session_valid


def _cached_clustering_session_valid() -> bool:
    if not _clustering_scheduler:
        return False
    if _storage_pipe is None:
        return True
    return _last_clustering_session_valid


def _start_clustering_auth_monitor():
    """Start background auth monitor so OCR hot path does not perform IPC checks."""
    global _clustering_auth_monitor_thread

    if _clustering_auth_monitor_thread and _clustering_auth_monitor_thread.is_alive():
        return

    def _worker():
        while not stop_event.is_set():
            try:
                _sync_clustering_scheduler_auth_gate(force=True)
            except Exception as exc:
                logger.debug('Clustering auth monitor tick failed: %s', exc)
            stop_event.wait(timeout=CLUSTERING_AUTH_POLL_INTERVAL_SECS)

    _clustering_auth_monitor_thread = threading.Thread(
        target=_worker,
        name='clustering-auth-monitor',
        daemon=True,
    )
    _clustering_auth_monitor_thread.start()


def _start_clustering_ingest_worker(maxsize: int = 128):
    global _clustering_ingest_queue, _clustering_ingest_thread
    if _clustering_ingest_thread and _clustering_ingest_thread.is_alive():
        return
    _clustering_ingest_queue = queue.Queue(maxsize=max(1, maxsize))
    _clustering_ingest_stop.clear()

    def _loop():
        while not _clustering_ingest_stop.is_set():
            try:
                item = _clustering_ingest_queue.get(timeout=0.2)
            except queue.Empty:
                continue
            try:
                if not (_clustering_manager and config.CLUSTERING_ENABLED):
                    continue
                if not (_clustering_scheduler_active and _last_clustering_session_valid):
                    logger.debug(
                        '[DIAG:clustering_ingest] skipped screenshot_id=%s (session locked/inactive)',
                        item.get('screenshot_id'),
                    )
                    continue
                started = time.perf_counter()
                _clustering_manager.add_snapshot(**item)
                logger.debug(
                    '[DIAG:clustering_ingest] add_snapshot done screenshot_id=%s elapsed=%.3fs queue_size=%s',
                    item.get('screenshot_id'),
                    time.perf_counter() - started,
                    _clustering_ingest_queue.qsize(),
                )
            except Exception as exc:
                logger.warning(
                    '[DIAG:clustering_ingest] add_snapshot failed screenshot_id=%s error=%s',
                    item.get('screenshot_id'),
                    exc,
                )
            finally:
                _clustering_ingest_queue.task_done()

    _clustering_ingest_thread = threading.Thread(
        target=_loop,
        name='clustering-ingest',
        daemon=True,
    )
    _clustering_ingest_thread.start()


def _stop_clustering_ingest_worker():
    global _clustering_ingest_thread, _clustering_ingest_queue
    _clustering_ingest_stop.set()
    if _clustering_ingest_thread:
        _clustering_ingest_thread.join(timeout=2.0)
    _clustering_ingest_thread = None
    _clustering_ingest_queue = None


def _enqueue_clustering_snapshot(item: dict) -> bool:
    if not (_clustering_manager and config.CLUSTERING_ENABLED):
        return False
    if not (_clustering_scheduler_active and _last_clustering_session_valid):
        return False
    if _clustering_ingest_queue is None:
        _start_clustering_ingest_worker()
    try:
        _clustering_ingest_queue.put_nowait(item)
        return True
    except queue.Full:
        logger.warning(
            '[DIAG:clustering_ingest] queue full; dropped screenshot_id=%s maxsize=%s',
            item.get('screenshot_id'),
            _clustering_ingest_queue.maxsize,
        )
        return False


def _delete_vectors_by_hashes(image_hashes):
    """Best-effort delete from vector store using image hashes."""
    if not image_hashes:
        return {"deleted": 0, "requested": 0, "skipped": True}

    if not _ocr_worker:
        return {"deleted": 0, "requested": len(image_hashes), "skipped": True}
    if not hasattr(_ocr_worker, 'delete_vector_image'):
        return {"deleted": 0, "requested": len(image_hashes), "skipped": True}

    deleted = 0
    for image_hash in image_hashes:
        if not isinstance(image_hash, str) or not image_hash:
            continue
        try:
            ok = _ocr_worker.delete_vector_image(image_hash)
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
            'clustering_auth_unlocked': _cached_clustering_session_valid(),
            'clustering_scheduler_active': _clustering_scheduler_active,
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
        ocr_timeout_secs = int(req.get('ocr_timeout_secs', getattr(config, '_ocr_timeout_secs', 120)))
        allow_full_low_memory = bool(req.get(
            'clustering_allow_full_low_memory',
            getattr(config, 'CLUSTERING_ALLOW_FULL_LOW_MEMORY', False),
        ))
        update_advanced_capture_config(capture_on_ocr_busy, ocr_queue_max_size, ocr_timeout_secs)
        update_clustering_resource_config(allow_full_low_memory)
        return {
            'status': 'success',
            'capture_on_ocr_busy': capture_on_ocr_busy,
            'ocr_queue_max_size': ocr_queue_max_size,
            'ocr_timeout_secs': ocr_timeout_secs,
            'clustering_allow_full_low_memory': allow_full_low_memory,
        }

    if cmd == 'update_feature_config':
        clustering_enabled = req.get('clustering_enabled', True)
        classification_enabled = req.get('classification_enabled', True)
        update_feature_config(clustering_enabled, classification_enabled)
        return {
            'status': 'success',
            'clustering_enabled': clustering_enabled,
            'classification_enabled': classification_enabled,
        }

    if cmd == 'get_all_models':
        logger.info('[DIAG:CMD-PY] Received get_all_models command')
        local_appdata = os.environ.get('LOCALAPPDATA', os.path.expanduser('~'))
        
        search_dirs = [
            os.path.join(local_appdata, "carbonPaper", "models"),
            os.path.join(local_appdata, "carbonPaper", "ppOCRmodel"),
            os.path.join(os.path.expanduser('~'), ".rapidocr", "models"),
            os.path.join(os.path.expanduser('~'), ".paddleocr"),
        ]
        
        models_info = []
        seen_paths = set()

        for models_dir in search_dirs:
            models_dir = os.path.normpath(models_dir)
            if not os.path.exists(models_dir):
                logger.info('[DIAG:CMD-PY] Models dir does not exist: %s', models_dir)
                continue
            
            logger.info('[DIAG:CMD-PY] Scanning models dir: %s', models_dir)
            
            # For some dirs, we look at files directly (like .rapidocr/models contains .onnx files)
            # For others, we look at subdirectories (like carbonPaper/models contains model folders)
            items = os.listdir(models_dir)
            for m in items:
                m_path = os.path.join(models_dir, m)
                if m_path in seen_paths:
                    continue
                
                is_dir = os.path.isdir(m_path)
                is_model_file = m.endswith('.onnx') or m.endswith('.pdmodel') or m.endswith('.bin')
                
                if is_dir or is_model_file:
                    total_size = 0
                    if is_dir:
                        for dirpath, _, filenames in os.walk(m_path):
                            for f in filenames:
                                fp = os.path.join(dirpath, f)
                                if not os.path.islink(fp):
                                    try:
                                        total_size += os.path.getsize(fp)
                                    except Exception:
                                        pass
                    else:
                        try:
                            total_size = os.path.getsize(m_path)
                        except Exception:
                            pass
                    
                    if total_size == 0: continue

                    # Determine purpose
                    purpose = "未知"
                    m_lower = m.lower()
                    if "minilm" in m_lower:
                        purpose = "任务聚类"
                    elif "bge-small" in m_lower:
                        purpose = "内容分类"
                    elif "clip" in m_lower:
                        purpose = "图像特征提取"
                    elif "ocr" in m_lower or "det_infer" in m_lower or "rec_infer" in m_lower or "cls_infer" in m_lower or m_lower.startswith("ch_pp"):
                        purpose = "文字识别 (OCR)"
                    elif "zh_core_web" in m_lower or "en_core_web" in m_lower:
                        purpose = "敏感信息识别"
                        
                    # Format size
                    size_mb = total_size / (1024 * 1024)
                    size_str = f"{size_mb:.1f} MB"
                    if size_mb > 1024:
                        size_str = f"{size_mb / 1024:.1f} GB"

                    models_info.append({
                        "name": m,
                        "path": m_path,
                        "size": size_str,
                        "purpose": purpose
                    })
                    seen_paths.add(m_path)

        logger.info('[DIAG:CMD-PY] Returning %d models', len(models_info))
        return {'status': 'success', 'models': models_info}

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

        logger.debug(
            '[DIAG:process_ocr] start screenshot_id=%s image_hash=%s process=%s',
            screenshot_id,
            req.get('image_hash', ''),
            req.get('process_name', ''),
        )

        if not hasattr(_ocr_worker, 'request'):
            return {'error': 'OCR service requires RestartableModelWorker'}

        timeout_secs = int(req.get('timeout_secs', getattr(config, '_ocr_timeout_secs', 120)))
        try:
            result = _ocr_worker.request(
                'process_ocr',
                {'request': req},
                timeout=max(30, min(600, timeout_secs)),
            )
            if result.get('status') == 'success':
                ocr_text = result.get('ocr_text', '')
                ocr_results = result.get('ocr_results') or []
                ocr_diag = result.get('ocr_diag') or {}
                logger.info(
                    '[DIAG:process_ocr] success screenshot_id=%s returned_blocks=%s text_len=%s postprocess_enqueued=%s elapsed=%.3fs worker_protocol=%s image_bytes=%s image_size=%s raw_blocks=%s filtered_blocks=%s ocr_elapsed=%.3fs',
                    screenshot_id,
                    len(ocr_results) if isinstance(ocr_results, list) else 0,
                    len(ocr_text or ''),
                    result.get('postprocess_enqueued'),
                    float(result.get('elapsed') or 0.0),
                    result.get('worker_protocol'),
                    ocr_diag.get('image_bytes'),
                    ocr_diag.get('image_size'),
                    ocr_diag.get('raw_blocks'),
                    ocr_diag.get('filtered_blocks'),
                    float(ocr_diag.get('ocr_elapsed') or 0.0),
                )
                _enqueue_clustering_snapshot({
                    'screenshot_id': screenshot_id,
                    'process_name': req.get('process_name', ''),
                    'window_title': req.get('window_title', ''),
                    'ocr_text': ocr_text,
                    'timestamp': req.get('timestamp', 0),
                    'category': '',
                })
                result.pop('ocr_text', None)
            return result
        except TimeoutError as e:
            logger.error('[DIAG:process_ocr] worker timeout screenshot_id=%s error=%s', screenshot_id, e)
            return {'error': str(e)}
        except Exception as e:
            logger.error('[DIAG:process_ocr] worker failed screenshot_id=%s error=%s', screenshot_id, e, exc_info=True)
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
    """Start the IPC server and initialise the OCR service.

    Args:
        _debug: Debug mode flag.
        pipe_name: Named pipe name (generated if not provided).
        auth_token: Authentication token for IPC validation.
        storage_pipe: Storage service pipe name (Rust reverse IPC).
    """
    global _server, _ocr_worker, _classifier, _storage_pipe, _clustering_manager, _clustering_scheduler, _clustering_scheduler_active, _clustering_auth_monitor_thread, _auth_token, _last_seq_no, _last_clustering_auth_check, _last_clustering_session_valid

    _auth_token = auth_token
    _last_seq_no = -1
    _storage_pipe = storage_pipe
    _clustering_scheduler_active = False
    _clustering_auth_monitor_thread = None
    _last_clustering_auth_check = 0.0
    _last_clustering_session_valid = False
    _stop_clustering_ingest_worker()

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

    worker_env = {
        'CARBONPAPER_CLUSTERING_ENABLED': str(config.CLUSTERING_ENABLED),
        'CARBONPAPER_CLASSIFICATION_ENABLED': str(config.CLASSIFICATION_ENABLED),
        'CARBONPAPER_CLUSTERING_ALLOW_FULL_LOW_MEMORY': str(config.CLUSTERING_ALLOW_FULL_LOW_MEMORY),
        'CARBONPAPER_USE_ONNX': os.environ.get('CARBONPAPER_USE_ONNX', 'true'),
        'CARBONPAPER_OCR_TIMEOUT_SECS': str(getattr(config, '_ocr_timeout_secs', 120)),
    }
    _ocr_worker = RestartableModelWorker(
        storage_pipe=storage_pipe,
        data_dir=get_data_dir(),
        env=worker_env,
    )
    _classifier = _ocr_worker
    logger.info('Restartable model worker proxy initialised')

    # Initialise task clustering service (MiniLM + HDBSCAN)
    try:
        from task_clustering import HotColdManager, ClusteringScheduler

        if shared_chroma_client is not None:
            sc = None
            if storage_pipe:
                from storage_client import get_storage_client
                sc = get_storage_client()

            _clustering_manager = HotColdManager(shared_chroma_client, storage_client=sc)
            _clustering_scheduler = ClusteringScheduler(_clustering_manager, storage_client=sc)
            unlocked = _sync_clustering_scheduler_auth_gate(force=True)
            _start_clustering_auth_monitor()
            _start_clustering_ingest_worker()
            logger.info('Task clustering service initialised (scheduler_active=%s unlocked=%s)', _clustering_scheduler_active, unlocked)
        else:
            logger.warning('Task clustering service skipped: shared ChromaDB client is None')
            _clustering_manager = None
            _clustering_scheduler = None
            _clustering_scheduler_active = False
            _clustering_auth_monitor_thread = None
    except Exception as e:
        logger.warning('Task clustering service failed to initialise (non-fatal): %s', e)
        _clustering_manager = None
        _clustering_scheduler = None
        _clustering_scheduler_active = False
        _clustering_auth_monitor_thread = None

    # Start smart cluster worker (idle-aware drain loop). Best-effort: any
    # failure here must not block monitor startup since smart clusters are
    # an optional feature.
    try:
        if _clustering_manager is not None and storage_pipe:
            from smart_cluster_worker import SmartClusterWorker
            from storage_client import get_storage_client
            sc = get_storage_client()
            if sc is not None:
                SmartClusterWorker().start(
                    sc,
                    _clustering_manager.embedder,
                    hot_collection_getter=lambda: _clustering_manager.hot_collection,
                )
                logger.info('Smart Cluster worker started')
    except Exception as e:
        logger.warning('Smart Cluster worker failed to start (non-fatal): %s', e)

    # NOTE: Screenshot capture loop is handled by Rust (capture.rs).
    # Python only provides OCR via the 'process_ocr' IPC command.

    return _server


def stop():
    """Shut down the OCR service and IPC server."""
    global _clustering_scheduler_active, _clustering_auth_monitor_thread
    stop_event.set()
    _stop_clustering_ingest_worker()
    if _clustering_scheduler:
        try:
            _clustering_scheduler.stop()
        except Exception:
            pass
    _clustering_scheduler_active = False
    if _clustering_auth_monitor_thread:
        _clustering_auth_monitor_thread = None
    try:
        from smart_cluster_worker import SmartClusterWorker
        SmartClusterWorker().stop()
    except Exception:
        pass
    try:
        from .presidio_worker import get_presidio_worker
        get_presidio_worker().stop()
    except Exception:
        pass
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
