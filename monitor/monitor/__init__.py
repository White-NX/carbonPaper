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
_classifier = None           # ClassificationService instance
_clustering_manager = None   # HotColdManager instance
_clustering_scheduler = None # ClusteringScheduler instance
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
    """Dispatch command (with diagnostic timing). Security: PID verification at transport layer + auth token at application layer."""
    import time as _time
    _t0 = _time.perf_counter()
    result = _handle_command_impl(req)
    elapsed = _time.perf_counter() - _t0
    cmd = (req.get('command') or '?').lower() if isinstance(req, dict) else '?'
    if elapsed > 5.0:
        logger.warning('[DIAG:CMD-PY] command=%s took %.3fs', cmd, elapsed)
    return result


_auth_token = None
_last_seq_no = -1

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
            image_hash = req.get('image_hash', '')
            ocr_text = ' '.join([r.get('text', '') for r in filtered])
            if _ocr_worker.enable_vector_store and _ocr_worker.vector_store:
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

            # Classify screenshot
            category = None
            category_confidence = None
            if _classifier:
                try:
                    window_title = req.get('window_title', '')
                    process_name = req.get('process_name', '')
                    category, category_confidence = _classifier.classify(
                        title=window_title,
                        ocr_text=ocr_text,
                        process_name=process_name,
                    )
                    category_confidence = round(category_confidence, 4)
                except Exception as ce:
                    logger.warning('Classification failed: %s', ce)

            # Add to task clustering hot layer (non-blocking, best-effort)
            if _clustering_manager:
                try:
                    _clustering_manager.add_snapshot(
                        screenshot_id=screenshot_id,
                        process_name=req.get('process_name', ''),
                        window_title=req.get('window_title', ''),
                        ocr_text=ocr_text,
                        timestamp=req.get('timestamp', 0),
                        category=category or '',
                    )
                except Exception as te:
                    logger.warning('Task vector add failed: %s', te)

            result = {
                'status': 'success',
                'ocr_results': filtered,
            }
            if category:
                result['category'] = category
                result['category_confidence'] = category_confidence
            return result
        except Exception as e:
            logger.error('process_ocr failed: %s', e)
            _ocr_worker.stats['failed_count'] += 1
            return {'error': str(e)}

    # ----- Classification commands -----
    if cmd == 'classify':
        title = req.get('title', '')
        ocr_text = req.get('ocr_text', '')
        process_name = req.get('process_name', '')
        if not _classifier:
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
            from .presidio_service import PresidioService
            svc = PresidioService.get_instance()
            if not svc._initialized:
                svc.initialize(language)
            results = svc.analyze(texts, entity_types)
            return {
                'status': 'success',
                'results': [
                    {'entities': list(ents)} for ents in results
                ],
            }
        except Exception as e:
            logger.error('presidio_analyze failed: %s', e)
            return {'error': str(e)}

    if cmd == 'presidio_set_language':
        language = req.get('language', 'zh-CN')
        try:
            from .presidio_service import PresidioService
            svc = PresidioService.get_instance()
            svc.switch_language(language)
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
            from .presidio_service import PresidioService
            svc = PresidioService.get_instance()
            return {'status': 'success', **svc.get_status()}
        except Exception as e:
            return {'status': 'success', 'loaded': False, 'language': None, 'model': 'none'}

    if cmd == 'presidio_unload':
        try:
            from .presidio_service import PresidioService
            svc = PresidioService.get_instance()
            svc.unload()
            return {'status': 'success', 'unloaded': True}
        except Exception as e:
            logger.error('presidio_unload failed: %s', e)
            return {'error': str(e)}

    if cmd == 'presidio_check_idle':
        try:
            from .presidio_service import PresidioService
            svc = PresidioService.get_instance()
            unloaded = svc.check_idle_and_unload()
            return {'status': 'success', 'unloaded': unloaded}
        except Exception as e:
            logger.error('presidio_check_idle failed: %s', e)
            return {'error': str(e)}

    # ----- Task clustering commands -----
    if cmd == 'run_clustering':
        if not _clustering_scheduler:
            return {'error': 'Clustering service not initialised'}
        start_time = req.get('start_time')
        end_time = req.get('end_time')
        try:
            if start_time is not None:
                start_time = float(start_time)
            if end_time is not None:
                end_time = float(end_time)
            result = _clustering_scheduler.run_now(start_time=start_time, end_time=end_time)
            return {'status': 'success', **result}
        except Exception as e:
            return {'error': str(e)}

    if cmd == 'get_clustering_status':
        if not _clustering_scheduler:
            return {'error': 'Clustering service not initialised'}
        config = _clustering_scheduler.get_config()
        last = _clustering_scheduler.get_last_result()
        return {
            'status': 'success',
            'config': config,
            'last_result': {
                k: v for k, v in (last or {}).items()
                if k != 'clusters'
            } if last else None,
        }

    if cmd == 'set_clustering_interval':
        if not _clustering_scheduler:
            return {'error': 'Clustering service not initialised'}
        interval = req.get('interval', '1w')
        try:
            _clustering_scheduler.set_interval(interval)
            return {'status': 'success', 'interval': interval}
        except ValueError as e:
            return {'error': str(e)}

    if cmd == 'get_tasks':
        if not _clustering_manager:
            return {'error': 'Clustering service not initialised'}
        try:
            last = _clustering_scheduler.get_last_result() if _clustering_scheduler else None
            hot_clusters = last.get('clusters', []) if last else []
            cold_clusters = _clustering_manager.get_cold_clusters()
            return {
                'status': 'success',
                'hot_clusters': hot_clusters,
                'cold_clusters': cold_clusters,
            }
        except Exception as e:
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
    global _server, _ocr_worker, _classifier, _storage_pipe, _clustering_manager, _clustering_scheduler, _auth_token, _last_seq_no

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

    # Lazy import to avoid triggering heavy model loading before IPC is ready
    from ocr_service import OCRService

    logger.info("Initialising OCR service...")
    _ocr_worker = OCRService(
        vector_db_path=os.path.join(get_data_dir(), 'chroma_db'),
        storage_pipe=storage_pipe,
        chroma_client=shared_chroma_client,
    )
    _ocr_worker.start()

    # Initialise classification service (BGE-small-zh-v1.5)
    try:
        from classifier import ClassificationService
        _classifier = ClassificationService(
            anchors_path=os.path.join(get_data_dir(), 'anchors.json'),
        )
        logger.info('Classification service initialised')
    except Exception as e:
        logger.warning('Classification service failed to initialise (non-fatal): %s', e)
        _classifier = None

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
            _clustering_scheduler.start()
            logger.info('Task clustering service initialised')
        else:
            logger.warning('Task clustering service skipped: shared ChromaDB client is None')
            _clustering_manager = None
            _clustering_scheduler = None
    except Exception as e:
        logger.warning('Task clustering service failed to initialise (non-fatal): %s', e)
        _clustering_manager = None
        _clustering_scheduler = None

    # NOTE: Screenshot capture loop is handled by Rust (capture.rs).
    # Python only provides OCR via the 'process_ocr' IPC command.

    return _server


def stop():
    """Shut down the OCR service and IPC server."""
    stop_event.set()
    if _clustering_scheduler:
        try:
            _clustering_scheduler.stop()
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
