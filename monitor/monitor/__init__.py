from .capture import (
    start_capture_thread,
    paused_event,
    stop_event,
    INTERVAL,
    set_screenshot_callback,
    update_exclusion_settings,
    get_exclusion_settings,
    _get_process_icon_base64,
)
from .ipc_pipe import start_pipe_server
import os
import uuid
import base64
import json

_server = None
_ocr_worker = None
_auth_token = None  # 存储认证 token
_last_seq_no = -1  # 最后一个处理的序列号
_storage_pipe = None  # 存储服务管道名

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
    # 获取 Windows LocalAppData/CarbonPaper/data 路径
    local_appdata = os.environ.get('LOCALAPPDATA')
    if not local_appdata:
        raise RuntimeError('LOCALAPPDATA 环境变量未设置')
    return os.path.join(local_appdata, 'CarbonPaper', 'data')

def _find_and_extract_icon(process_name: str):
    """Try to find exe path for a process name and extract its icon."""
    if not process_name:
        return None
    
    try:
        import psutil
        # Search running processes for matching name
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


def _handle_command(req: dict):
    """处理命令前验证认证信息"""
    global _last_seq_no
    
    # 提取并验证认证信息
    req_token = req.get('_auth_token')
    req_seq_no = req.get('_seq_no')
    
    # 检查认证 token
    if _auth_token and req_token != _auth_token:
        return {'error': 'Authentication failed: Invalid token'}
    
    # 检查序列号（防止重放攻击）
    if req_seq_no is not None:
        if req_seq_no <= _last_seq_no:
            return {'error': f'Authentication failed: Invalid sequence number (got {req_seq_no}, expected > {_last_seq_no})'}
        _last_seq_no = req_seq_no
    
    # 处理实际命令
    cmd = (req.get('command') or '').lower()
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
            'interval': INTERVAL
        }
        if _ocr_worker:
            status['ocr_stats'] = _ocr_worker.get_stats()
        return status

    if cmd == 'update_filters':
        filters = req.get('filters', {}) if isinstance(req, dict) else {}
        try:
            update_exclusion_settings(
                processes=filters.get('processes') or req.get('processes'),
                titles=filters.get('titles') or req.get('titles'),
                ignore_protected=filters.get('ignore_protected') if 'ignore_protected' in filters else req.get('ignore_protected')
            )
            return {'status': 'success', 'filters': get_exclusion_settings()}
        except Exception as e:
            return {'error': str(e)}
    
    # Get image command (IPC)
    if cmd == 'get_image':
        image_id = req.get('id')
        image_path_req = req.get('path')
        
        if not _ocr_worker:
            return {'error': 'OCR worker not initialized'}
            
        try:
            image_path = None
            if image_id is not None:
                record = _ocr_worker.db_handler.get_screenshot_by_id(image_id)
                if record:
                     image_path = record['image_path']
            
            if not image_path and image_path_req:
                image_path = image_path_req
                
            if not image_path:
                return {'error': 'Image not found (no ID or path provided or ID invalid)'}
            
            # Normalize the path first to resolve any ../ components
            image_path = os.path.normpath(image_path)
            
            # If path doesn't exist, try different resolution strategies
            if not os.path.exists(image_path):
                # Strategy 1: Use screenshot_dir + basename
                screenshot_dir = os.path.normpath(_ocr_worker.screenshot_dir)
                basename = os.path.basename(image_path)
                candidate = os.path.join(screenshot_dir, basename)
                if os.path.exists(candidate):
                    image_path = candidate
                else:
                    # Strategy 2: Search in common locations relative to this file
                    this_file_dir = os.path.dirname(os.path.abspath(__file__))
                    parent_dir = os.path.dirname(this_file_dir)  # monitor/
                    candidate2 = os.path.join(parent_dir, 'screenshots', basename)
                    if os.path.exists(candidate2):
                        image_path = candidate2
                    else:
                        return {'error': f'File not found. Tried: {image_path}, {candidate}, {candidate2}'}

            with open(image_path, 'rb') as f:
                data = f.read()
                b64 = base64.b64encode(data).decode('utf-8')
                return {'status': 'success', 'data': b64, 'mime_type': 'image/png'}
        except Exception as e:
            return {'error': str(e)}

    # Timeline command
    if cmd == 'get_timeline':
        start_time = req.get('start_time', 0)
        end_time = req.get('end_time', 0)
        
        # Convert ms to seconds if necessary (assuming JS sends ms)
        if start_time > 10000000000: # heuristic for ms
            start_time /= 1000.0
        if end_time > 10000000000:
            end_time /= 1000.0
            
        if not _ocr_worker:
            return {'error': 'OCR worker not initialized'}
            
        try:
            records = _ocr_worker.db_handler.get_screenshots_by_time_range(start_time, end_time)
            for r in records:
                meta_raw = r.get('metadata')
                meta = None
                if isinstance(meta_raw, str):
                    try:
                        meta = json.loads(meta_raw)
                    except Exception:
                        meta = None
                elif isinstance(meta_raw, dict):
                    meta = meta_raw

                if meta:
                    r['metadata'] = meta
                    icon_b64 = meta.get('process_icon')
                    if icon_b64:
                        r['process_icon'] = icon_b64
                    process_path = meta.get('process_path')
                    if process_path:
                        r['process_path'] = process_path
                else:
                    r['metadata'] = None

                # For records without icons, try to extract dynamically based on process_name
                if not r.get('process_icon'):
                    process_name = r.get('process_name')
                    if process_name:
                        # Check cache first
                        if process_name in _dynamic_icon_cache:
                            r['process_icon'] = _dynamic_icon_cache[process_name]
                        else:
                            # Try to find exe path and extract icon
                            icon_b64 = _find_and_extract_icon(process_name)
                            _dynamic_icon_cache[process_name] = icon_b64
                            if icon_b64:
                                r['process_icon'] = icon_b64

            return {'status': 'success', 'records': records}
        except Exception as e:
            return {'error': str(e)}


    # Get details command
    if cmd == 'get_screenshot_details':
        image_id = req.get('id')
        image_path = req.get('path')
        
        if not _ocr_worker:
            return {'error': 'OCR worker not initialized'}
            
        try:
            record = None
            if image_id and image_id != -1:
                record = _ocr_worker.db_handler.get_screenshot_by_id(image_id)
            
            # 如果ID查找失败或没有ID，尝试用路径查找
            if not record and image_path:
                record = _ocr_worker.db_handler.get_screenshot_by_path(image_path)
            
            if not record:
                return {'error': 'Image not found'}
            
            # 成功找到记录后，更新 image_id 以便查询 OCR 数据
            image_id = record['id']
            
            # Normalize metadata if present
            meta_raw = record.get('metadata') if isinstance(record, dict) else None
            if isinstance(meta_raw, str):
                try:
                    record['metadata'] = json.loads(meta_raw)
                except Exception:
                    pass

            # Get OCR results
            ocr_results = _ocr_worker.db_handler.get_screenshot_ocr_results(image_id)
            
            return {
                'status': 'success', 
                'record': record,
                'ocr_results': ocr_results
            }
        except Exception as e:
            return {'error': str(e)}


    # 新增搜索命令
    if cmd == 'search':

        query = req.get('query', '')
        limit = req.get('limit', 10)
        offset = req.get('offset', 0)
        process_names = req.get('process_names') or None
        start_time = req.get('start_time')
        end_time = req.get('end_time')
        fuzzy = req.get('fuzzy', True)

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

        if not _ocr_worker:
            return {'error': 'OCR worker not initialized'}

        try:
            results = _ocr_worker.search_text(
                query,
                limit=limit,
                offset=offset,
                fuzzy=fuzzy,
                process_names=process_names,
                start_time=start_time,
                end_time=end_time
            )
        except Exception as exc:
            return {'error': str(exc)}

        return {'status': 'success', 'results': results}
        
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
                end_time=end_time
            )
        except Exception as exc:
            return {'error': str(exc)}

        return {'status': 'success', 'results': results}

    if cmd == 'list_processes':
        if not _ocr_worker:
            return {'error': 'OCR worker not initialized'}
        try:
            processes = _ocr_worker.list_processes()
        except Exception as exc:
            return {'error': str(exc)}
        return {'status': 'success', 'processes': processes}

    if cmd == 'delete_screenshot':
        screenshot_id = req.get('screenshot_id')
        if screenshot_id is None:
            return {'error': 'screenshot_id is required'}
        if not _ocr_worker:
            return {'error': 'OCR worker not initialized'}
        try:
            deleted = _ocr_worker.db_handler.delete_screenshot(screenshot_id)
            image_hashes = []
            image_hash = req.get('image_hash')
            if isinstance(image_hash, str) and image_hash:
                image_hashes.append(image_hash)
            vector_info = _delete_vectors_by_hashes(image_hashes)
            return {
                'status': 'success',
                'deleted': deleted,
                'vector_deleted': vector_info.get('deleted', 0)
            }
        except Exception as exc:
            return {'error': str(exc)}

    if cmd == 'delete_by_time_range':
        start_time = req.get('start_time')
        end_time = req.get('end_time')
        if start_time is None or end_time is None:
            return {'error': 'start_time and end_time are required'}
        if not _ocr_worker:
            return {'error': 'OCR worker not initialized'}
        try:
            deleted_count = _ocr_worker.db_handler.delete_screenshots_by_time_range(
                float(start_time), float(end_time)
            )
            image_hashes = req.get('image_hashes')
            if not isinstance(image_hashes, list):
                image_hashes = []
            vector_info = _delete_vectors_by_hashes(image_hashes)
            return {
                'status': 'success',
                'deleted_count': deleted_count,
                'vector_deleted': vector_info.get('deleted', 0)
            }
        except Exception as exc:
            return {'error': str(exc)}

    return {'error': 'unknown command'}


def start(_debug, pipe_name: str = None, auth_token: str = None, storage_pipe: str = None):
    """
    启动 capture 线程和命名管道服务器。
    
    Args:
        _debug: 调试模式
        pipe_name: 管道名（如果未提供则从环境变量获取或生成随机名称）
        auth_token: 认证 token（用于验证 IPC 请求）
        storage_pipe: 存储服务管道名（用于将截图和元数据发送到 Rust 存储服务）
    """
    global _server, _ocr_worker, _auth_token, _last_seq_no, _storage_pipe

    # 存储认证 token
    _auth_token = auth_token
    _last_seq_no = -1  # 重置序列号
    _storage_pipe = storage_pipe

    if not pipe_name:
        pipe_name = os.environ.get('CARBON_MONITOR_PIPE')

    if not pipe_name:
        # 生成一次性不可预测的管道名
        pipe_name = f'carbon_monitor_{uuid.uuid4().hex}'
        # 将管道名打印到 stdout，便于父进程（如 Tauri）捕获
        print(pipe_name, flush=True)

    if _debug:
        try:
            with open('monitor_pipe_name.txt', 'w', encoding='utf-8') as f:
                f.write(pipe_name)
        except Exception as e:
            print('注意到调试模式已经开启，但是无法写入调试管道名文件:', e)

    # 如果父进程传入了继承句柄优先使用它（更安全）
    handle_val = os.environ.get('CARBON_MONITOR_PIPE_HANDLE')
    if handle_val:
        try:
            handle_int = int(handle_val, 0)
            from .ipc_pipe import start_inherited_handle_server
            _server = start_inherited_handle_server(handler=_handle_command, handle_value=handle_int)
        except Exception as e:
            print('无法使用继承句柄启动 IPC 服务器，回退到命名管道:', e)

    # start IPC server if not started by handle
    if _server is None:
        _server = start_pipe_server(handler=_handle_command, pipe_name=pipe_name)

    # 延迟导入，避免在模块加载阶段触发 PaddleOCR 初始化导致 IPC 迟迟不就绪
    from screenshot_worker import ScreenshotOCRWorker

    # 初始化并启动 OCR 工作进程
    print("正在启动 OCR 工作进程...")
    _ocr_worker = ScreenshotOCRWorker(
        screenshot_dir=os.path.join(get_data_dir(), 'screenshots'),
        db_path=os.path.join(get_data_dir(), 'ocr_data.db'),
        vector_db_path=os.path.join(get_data_dir(), 'chroma_db'),
        storage_pipe=storage_pipe  # 传递存储管道名
    )
    _ocr_worker.start(watch_dir=False) # 由 capture 回调触发，不需要轮询目录

    # 设置截图回调 - 内存模式，不写入明文文件
    def on_screenshot(image_bytes: bytes, image_pil, info: dict):
        """
        处理截图回调 - 纯内存模式
        
        Args:
            image_bytes: JPEG 编码的图像字节
            image_pil: PIL Image 对象（用于 OCR）
            info: 截图元数据
        """
        if _ocr_worker:
            metadata = info.get('metadata') or {}
            if not isinstance(metadata, dict):
                metadata = {'raw': metadata}

            monitor_data = info.get('monitor')
            if monitor_data:
                if isinstance(monitor_data, dict):
                    metadata.setdefault('monitor', monitor_data)
                else:
                    metadata.setdefault('monitor_raw', monitor_data)

            # 使用内存模式处理截图，不经过磁盘
            _ocr_worker.queue_image_from_memory(
                image_bytes=image_bytes,
                image_pil=image_pil,
                window_title=info.get('window_title'),
                process_name=info.get('process_name'),
                metadata=metadata
            )
    
    set_screenshot_callback(on_screenshot)

    # start capture thread
    start_capture_thread()

    return _server


def stop():
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
