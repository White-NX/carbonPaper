"""
截图OCR工作进程模块 - 长时运行的截图处理和OCR识别
"""
import hashlib
import os
import time
import datetime
import threading
import queue
import logging
from typing import Optional, Callable, Dict, Any, List
from pathlib import Path
from PIL import Image

logger = logging.getLogger(__name__)

# 导入相关模块
from ocr_engine import OCREngine, OCRVisualizer, get_ocr_engine
from db_handler import OCRDatabaseHandler
from vector_store import VectorStore
from storage_client import StorageClient, get_storage_client, init_storage_client


class ScreenshotOCRWorker:
    """截图OCR工作进程"""
    
    def __init__(
        self,
        screenshot_dir: str = "./screenshots",
        db_path: str = "./ocr_data.db",
        vector_db_path: str = "./chroma_db",
        enable_vector_store: bool = True,
        ocr_confidence_threshold: float = 0.5,
        save_visualizations: bool = False,
        visualization_dir: str = "./visualizations",
        storage_pipe: str = None
    ):
        """
        初始化工作进程
        
        Args:
            screenshot_dir: 截图保存目录
            db_path: SQLite数据库路径
            vector_db_path: ChromaDB向量数据库路径
            enable_vector_store: 是否启用向量存储
            ocr_confidence_threshold: OCR置信度阈值
            save_visualizations: 是否保存可视化结果
            visualization_dir: 可视化结果保存目录
            storage_pipe: Rust 存储服务管道名
        """
        self.screenshot_dir = Path(screenshot_dir)
        self.screenshot_dir.mkdir(parents=True, exist_ok=True)
        
        self.ocr_confidence_threshold = ocr_confidence_threshold
        self.save_visualizations = save_visualizations
        self.visualization_dir = Path(visualization_dir)
        if save_visualizations:
            self.visualization_dir.mkdir(parents=True, exist_ok=True)
        
        # 存储客户端（用于将数据发送到 Rust）
        self.storage_pipe = storage_pipe
        self.storage_client: Optional[StorageClient] = None
        if storage_pipe:
            self.storage_client = init_storage_client(storage_pipe)
            logger.info("存储客户端已初始化: %s", storage_pipe)
        
        # 初始化组件
        logger.info("Initalizing OCR engine...")
        try:
            self.ocr_engine = get_ocr_engine()
            logger.info("OCR engine initialized successfully.")
        except Exception as e:
            logger.error("OCR Engine initalized failed: %s", e)
            import traceback
            logger.exception("OCR Engine initialization ERROR")
            raise
        
        # 数据库处理器（当没有存储客户端时使用本地数据库）
        logger.info("Initalizing local database handler...")
        self.db_handler = OCRDatabaseHandler(db_path)
        
        self.enable_vector_store = enable_vector_store
        self.vector_store: Optional[VectorStore] = None
        if enable_vector_store:
            logger.info("Initalizing vector store...")
            self.vector_store = VectorStore(
                collection_name="screenshots",
                persist_directory=vector_db_path,
                storage_client=self.storage_client  # 传递存储客户端用于加密
            )
        
        if save_visualizations:
            self.visualizer = OCRVisualizer()
        
        # 控制变量
        self._stop_event = threading.Event()
        self._pause_event = threading.Event()
        self._worker_thread: Optional[threading.Thread] = None
        
        # 任务队列（用于异步处理）
        self._task_queue: queue.Queue = queue.Queue()
        # 当前正在处理（in-flight）的任务计数与锁
        self._in_flight: int = 0
        self._in_flight_lock = threading.Lock()
        
        # 回调函数
        self._on_ocr_complete: Optional[Callable] = None
        self._on_error: Optional[Callable] = None
        
        # 统计
        self.stats = {
            'processed_count': 0,
            'failed_count': 0,
            'total_texts_found': 0,
            'start_time': None
        }
    
    def set_callbacks(
        self,
        on_ocr_complete: Optional[Callable[[str, Dict], None]] = None,
        on_error: Optional[Callable[[str, Exception], None]] = None
    ):
        """
        设置回调函数
        
        Args:
            on_ocr_complete: OCR完成回调 (image_id, result)
            on_error: 错误回调 (image_id, exception)
        """
        self._on_ocr_complete = on_ocr_complete
        self._on_error = on_error
    
    def _compute_bytes_hash(self, data: bytes) -> str:
        """计算字节数据的哈希值"""
        import hashlib
        return hashlib.md5(data).hexdigest()
    
    def process_image_from_memory(
        self,
        image_bytes: bytes,
        image_pil: Image.Image,
        window_title: str = None,
        process_name: str = None,
        width: int = None,
        height: int = None,
        metadata: Dict = None,
        screenshot_id: Optional[str] = None
    ) -> Dict[str, Any]:
        """
        处理内存中的图片数据
        
        Args:
            image_bytes: JPEG 格式的图片字节数据
            image_pil: PIL Image 对象（用于 OCR）
            window_title: 窗口标题
            process_name: 进程名
            width: 图片宽度
            height: 图片高度
            metadata: 额外元数据
            
        Returns:
            处理结果
        """
        image_hash = self._compute_bytes_hash(image_bytes)
        
        result = {
            'image_hash': image_hash,
            'success': False,
            'ocr_results': [],
            'db_result': None,
            'vector_id': None,
            'error': None
        }
        
        try:
            if width is None or height is None:
                width, height = image_pil.size
            # 如果尚未有 screenshot_id，则尝试立即进行临时加密保存（非阻塞的短调用）
            if self.storage_client and not screenshot_id:
                try:
                    temp_res = self.storage_client.save_screenshot_temp(
                        image_data=image_bytes,
                        image_hash=image_hash,
                        width=width,
                        height=height,
                        window_title=window_title,
                        process_name=process_name,
                        metadata=metadata,
                    )
                    if temp_res.get('status') == 'success' or temp_res.get('screenshot_id'):
                        screenshot_id = temp_res.get('screenshot_id') or temp_res.get('id')
                        logger.info("[storage_client] 临时保存截图成功 id=%s", screenshot_id)
                    else:
                        logger.error("[storage_client] save_screenshot_temp failed: %s", temp_res.get('error'))
                except Exception as e:
                    logger.error("[storage_client] save_screenshot_temp exception: %s", e)
            
            # OCR识别（使用 PIL Image）
            logger.info("[OCR] 开始识别: hash=%s... size=%dx%d", image_hash[:8], width, height)
            ocr_results = self.ocr_engine.recognize(image_pil)
            logger.info("[OCR] 识别完成，得到 %d 个原始结果", len(ocr_results) if ocr_results else 0)
            
            # 过滤低置信度结果
            filtered_results = [
                r for r in ocr_results 
                if r['confidence'] >= self.ocr_confidence_threshold
            ]
            result['ocr_results'] = filtered_results
            
            # 合并OCR文本
            ocr_text = ' '.join([r['text'] for r in filtered_results])
            
            # 发送到 Rust 加密存储
            if self.storage_client:
                try:
                    # 格式化 OCR 结果
                    ocr_for_storage = [
                        {
                            'text': r['text'],
                            'confidence': r['confidence'],
                            'box': r['box']
                        }
                        for r in filtered_results
                    ]
                    
                    # 发送到 Rust 存储服务
                    # 如果已有 screenshot_id（由 capture 时已临时保存），则提交 OCR 结果
                    if screenshot_id:
                        commit_result = self.storage_client.commit_screenshot(
                            screenshot_id=screenshot_id,
                            ocr_results=ocr_for_storage
                        )
                        if commit_result.get('status') == 'success' or commit_result.get('status') is None:
                            # 返回中采用与旧接口兼容的字段名
                            result['db_result'] = commit_result
                            logger.info("[storage_client] 截图已提交并写入 OCR: id=%s", screenshot_id)
                        else:
                            # 尝试中止 pending 状态以清理 .pending 文件
                            reason = f"commit failed: {commit_result.get('error')}"
                            try:
                                abort_res = self.storage_client.abort_screenshot(
                                    screenshot_id=screenshot_id,
                                    reason=reason
                                )
                                logger.info("[storage_client] abort_screenshot called for id=%s: %s", screenshot_id, abort_res)
                            except Exception as abort_exc:
                                logger.error("[storage_client] abort_screenshot exception for id=%s: %s", screenshot_id, abort_exc)
                            raise Exception(reason)
                    else:
                        # 兼容旧行为：直接保存（同步）
                        storage_result = self.storage_client.save_screenshot(
                            image_data=image_bytes,
                            image_hash=image_hash,
                            width=width,
                            height=height,
                            window_title=window_title,
                            process_name=process_name,
                            metadata=metadata,
                            ocr_results=ocr_for_storage
                        )

                        if storage_result.get('status') == 'success':
                            result['db_result'] = storage_result
                            logger.info("[storage_client] 截图已加密保存: %s", storage_result.get('image_path'))
                        elif storage_result.get('status') == 'duplicate':
                            result['db_result'] = storage_result
                            logger.info("[storage_client] 截图已存在（跳过）: %s...", image_hash[:8])
                        else:
                            raise Exception(f"存储失败: {storage_result.get('error')}")
                        
                except Exception as e:
                    # 当提交或存储抛异常时，若之前已临时保存 screenshot_id，应主动中止以清理 pending 文件
                    try:
                        if screenshot_id:
                            reason = str(e)
                            try:
                                abort_res = self.storage_client.abort_screenshot(
                                    screenshot_id=screenshot_id,
                                    reason=reason
                                )
                                logger.info("[storage_client] abort_screenshot called for id=%s: %s", screenshot_id, abort_res)
                            except Exception as abort_exc:
                                logger.error("[storage_client] abort_screenshot exception for id=%s: %s", screenshot_id, abort_exc)
                    except Exception:
                        # 忽略 abort 本身的任何异常，继续抛原始错误
                        pass

                    logger.error("[storage_client] 加密存储失败: %s", e)
                    result['error'] = str(e)
                    raise  # 没有后备方案，直接失败（不保存明文）
            else:
                raise Exception("存储客户端未初始化，无法安全存储截图")
            
            # 保存到向量存储（用于语义搜索）
            if self.enable_vector_store and self.vector_store:
                screenshot_id_val = screenshot_id  # 直接使用已有变量（来自 save_screenshot_temp）
                real_image_path = (result['db_result'].get('image_path')
                                   if result.get('db_result') else None) or f"memory://{image_hash}"
                try:
                    import datetime
                    created_at_value = datetime.datetime.now().strftime('%Y-%m-%d %H:%M:%S')

                    vector_id = self.vector_store.add_image(
                        image_path=real_image_path,
                        image=image_pil,
                        metadata={
                            'window_title': window_title or '',
                            'process_name': process_name or '',
                            'width': width,
                            'height': height,
                            'text_count': len(filtered_results),
                            'screenshot_id': screenshot_id_val if screenshot_id_val else -1,
                            'created_at': created_at_value,
                            'screenshot_created_at': created_at_value
                        },
                        ocr_text=ocr_text
                    )
                    logger.info("[screenshot_worker] add_image returned vector_id=%s", vector_id)
                    result['vector_id'] = vector_id
                except Exception as e:
                    logger.error("[screenshot_worker] add_image raised exception: %s", e)
                    result['vector_id'] = None
            
            result['success'] = True
            self.stats['processed_count'] += 1
            self.stats['total_texts_found'] += len(filtered_results)
            
        except Exception as e:
            result['error'] = str(e)
            self.stats['failed_count'] += 1
            if self._on_error:
                self._on_error(image_hash, e)
        
        if result['success'] and self._on_ocr_complete:
            self._on_ocr_complete(image_hash, result)
        
        return result

    def pending_count(self) -> int:
        """
        返回当前队列中待处理（pending）任务数量。
        """
        try:
            qsize = self._task_queue.qsize()
        except Exception:
            qsize = 0

        try:
            with self._in_flight_lock:
                in_flight = int(self._in_flight)
        except Exception:
            in_flight = 0

        return qsize + in_flight
    
    def _compute_image_hash(self, image_path: str) -> str:
        """计算图片文件的哈希值（用于兼容旧代码）"""
        import hashlib
        with open(image_path, 'rb') as f:
            return hashlib.md5(f.read()).hexdigest()
    
    def process_image(
        self,
        image_path: str,
        window_title: str = None,
        process_name: str = None,
        metadata: Dict = None
    ) -> Dict[str, Any]:
        """
        处理磁盘上的图片文件（兼容旧接口，但会尝试加密存储后删除原文件）
        
        Args:
            image_path: 图片路径
            window_title: 窗口标题
            process_name: 进程名
            metadata: 额外元数据
            
        Returns:
            处理结果
        """
        result = {
            'image_path': image_path,
            'success': False,
            'ocr_results': [],
            'db_result': None,
            'vector_id': None,
            'error': None
        }
        
        try:
            # 加载图片
            image = Image.open(image_path)
            width, height = image.size
            
            # 读取图片数据
            with open(image_path, 'rb') as f:
                image_bytes = f.read()
            
            # 使用内存处理方法
            mem_result = self.process_image_from_memory(
                image_bytes=image_bytes,
                image_pil=image,
                window_title=window_title,
                process_name=process_name,
                width=width,
                height=height,
                metadata=metadata
            )
            
            result.update(mem_result)
            result['image_path'] = image_path
            
            # 如果成功加密存储，删除原始明文文件
            if mem_result.get('success') and os.path.exists(image_path):
                try:
                    os.remove(image_path)
                    logger.info("[screenshot_worker] 已删除原始明文文件: %s", image_path)
                except Exception as del_err:
                    logger.error("[screenshot_worker] 删除原始文件失败: %s", del_err)
            
        except Exception as e:
            result['error'] = str(e)
            self.stats['failed_count'] += 1
            if self._on_error:
                self._on_error(image_path, e)
        
        return result
    
    def queue_image_from_memory(
        self,
        image_bytes: bytes,
        image_pil: Image.Image,
        window_title: str = None,
        process_name: str = None,
        width: int = None,
        height: int = None,
        metadata: Dict = None,
        screenshot_id: Optional[str] = None
    ):
        """
        将内存中的图片添加到处理队列
        
        Args:
            image_bytes: JPEG 格式的图片字节数据
            image_pil: PIL Image 对象
            window_title: 窗口标题
            process_name: 进程名
            width: 图片宽度
            height: 图片高度
            metadata: 额外元数据
        """
        self._task_queue.put({
            'image_bytes': image_bytes,
            'image_pil': image_pil,
            'window_title': window_title,
            'process_name': process_name,
            'width': width,
            'height': height,
            'metadata': metadata,
            'screenshot_id': screenshot_id,
            '_from_memory': True  # 标记为内存数据
        })

        if self._task_queue.qsize() > 10:
            logger.warning("[screenshot_worker] Task queue size is large: %d, is OCR process overloaded?", self._task_queue.qsize())
            # 通过遍历队列中任务，累加 `image_bytes` 的长度来估算内存占用（线程安全）
            total_bytes = 0
            with self._task_queue.mutex:
                queued_items = list(self._task_queue.queue)

            for item in queued_items:
                try:
                    if isinstance(item, dict) and 'image_bytes' in item and isinstance(item['image_bytes'], (bytes, bytearray)):
                        total_bytes += len(item['image_bytes'])
                except Exception:
                    # 忽略任何异常，继续估算其它项
                    continue

            def _fmt_bytes(n: int) -> str:
                n = float(n)
                for unit in ['B', 'KB', 'MB', 'GB', 'TB']:
                    if n < 1024.0:
                        return f"{n:.1f}{unit}"
                    n /= 1024.0
                return f"{n:.1f}PB"

            estimated_memory = total_bytes
            logger.warning("[screenshot_worker] Estimated image memory usage: ~%s (queue items: %d)", _fmt_bytes(estimated_memory), len(queued_items))
    
    def _save_to_local_db(
        self,
        image_path: str,
        ocr_results: List[Dict],
        width: int,
        height: int,
        window_title: str = None,
        process_name: str = None,
        metadata: Dict = None
    ) -> Dict[str, Any]:
        """保存到本地 SQLite 数据库"""
        return self.db_handler.save_ocr_data(
            image_path=image_path,
            ocr_results=ocr_results,
            width=width,
            height=height,
            window_title=window_title,
            process_name=process_name,
            metadata=metadata
        )
    
    def _worker_loop(self, watch_dir: bool = True, interval: float = 5.0):
        """
        工作循环
        
        Args:
            watch_dir: 是否监视目录中的新文件（仅用于兼容旧数据）
            interval: 检查间隔（秒）
        """
        self.stats['start_time'] = datetime.datetime.now()
        processed_files = set()
        
        logger.info("截图OCR工作进程已启动")
        
        while not self._stop_event.is_set():
            # 检查暂停
            if self._pause_event.is_set():
                time.sleep(0.2)
                continue
            
            # 处理任务队列中的任务
            while not self._task_queue.empty():
                try:
                    task = self._task_queue.get_nowait()

                    # 标记为 in-flight
                    try:
                        with self._in_flight_lock:
                            self._in_flight += 1
                    except Exception:
                        # 若锁不可用则忽略，但仍继续处理
                        pass

                    try:
                        # 检查是否是内存数据
                        if task.get('_from_memory'):
                            # 内存数据处理（不涉及磁盘文件）
                            logger.info("[OCR Worker] 开始处理内存数据任务")
                            try:
                                result = self.process_image_from_memory(
                                    image_bytes=task.get('image_bytes'),
                                    image_pil=task.get('image_pil'),
                                    window_title=task.get('window_title'),
                                    process_name=task.get('process_name'),
                                    width=task.get('width'),
                                    height=task.get('height'),
                                    metadata=task.get('metadata'),
                                    screenshot_id=task.get('screenshot_id')
                                )
                                if result.get('success'):
                                    logger.info("[OCR Worker] 任务完成: 识别到 %d 个文本块", len(result.get('ocr_results', [])))
                                else:
                                    logger.error("[OCR Worker] 任务失败: %s", result.get('error', 'unknown error'))
                            except Exception as proc_err:
                                logger.exception("[OCR Worker] process_image_from_memory 异常: %s", proc_err)
                        else:
                            # 旧的文件路径处理（兼容）
                            logger.info("[OCR Worker] 开始处理文件任务: %s", task.get('image_path', 'unknown'))
                            try:
                                result = self.process_image(**task)
                                if result.get('success'):
                                    logger.info("[OCR Worker] 任务完成: 识别到 %d 个文本块", len(result.get('ocr_results', [])))
                                else:
                                    logger.error("[OCR Worker] 任务失败: %s", result.get('error', 'unknown error'))
                            except Exception as proc_err:
                                logger.exception("[OCR Worker] process_image 异常: %s", proc_err)
                    finally:
                        # 取消 in-flight 标记（确保在任何情况下都能减少计数）
                        try:
                            with self._in_flight_lock:
                                # 防止负数
                                if self._in_flight > 0:
                                    self._in_flight -= 1
                                else:
                                    self._in_flight = 0
                        except Exception:
                            pass
                except queue.Empty:
                    break
            
            # 监视目录中的新文件（仅用于处理旧的明文文件）
            if watch_dir:
                try:
                    for file_path in self.screenshot_dir.glob("*.jpg"):
                        if str(file_path) not in processed_files:
                            # 等待文件写入完成
                            time.sleep(0.5)
                            
                            logger.info("发现旧的明文截图: %s", file_path)
                            result = self.process_image(str(file_path))

                            if result['success']:
                                logger.info("处理完成: 识别到 %d 个文本块", len(result['ocr_results']))
                            else:
                                logger.error("处理失败: %s", result['error'])
                            
                            processed_files.add(str(file_path))
                    
                    # 同时检查 PNG 文件
                    for file_path in self.screenshot_dir.glob("*.png"):
                        if str(file_path) not in processed_files:
                            time.sleep(0.5)
                            logger.info("发现新截图: %s", file_path)
                            result = self.process_image(str(file_path))

                            if result['success']:
                                logger.info("处理完成: 识别到 %d 个文本块", len(result['ocr_results']))
                            else:
                                logger.error("处理失败: %s", result['error'])
                            
                            processed_files.add(str(file_path))
                            
                except Exception as e:
                    logger.error("监视目录时出错: %s", e)
            
            # 等待下一次检查
            self._stop_event.wait(interval)
        
        logger.info("截图OCR工作进程已停止")
    
    def start(self, watch_dir: bool = True, interval: float = 5.0):
        """
        启动工作进程
        
        Args:
            watch_dir: 是否监视目录
            interval: 检查间隔（秒）
        """
        if self._worker_thread and self._worker_thread.is_alive():
            logger.warning("工作进程已在运行")
            return
        
        self._stop_event.clear()
        self._pause_event.clear()
        
        self._worker_thread = threading.Thread(
            target=self._worker_loop,
            args=(watch_dir, interval),
            daemon=True
        )
        self._worker_thread.start()
    
    def stop(self):
        """停止工作进程"""
        self._stop_event.set()
        if self._worker_thread:
            self._worker_thread.join(timeout=5.0)
    
    def pause(self):
        """暂停工作进程"""
        self._pause_event.set()
        logger.info("工作进程已暂停")
    
    def resume(self):
        """恢复工作进程"""
        self._pause_event.clear()
        logger.info("工作进程已恢复")
    
    def add_task(
        self,
        image_path: str,
        window_title: str = None,
        process_name: str = None,
        metadata: Dict = None
    ):
        """
        添加处理任务到队列
        
        Args:
            image_path: 图片路径
            window_title: 窗口标题
            process_name: 进程名
            metadata: 额外元数据
        """
        logger.info("[OCR Worker] 添加任务到队列: %s", image_path)
        self._task_queue.put({
            'image_path': image_path,
            'window_title': window_title,
            'process_name': process_name,
            'metadata': metadata
        })
        logger.info("[OCR Worker] 当前队列大小: %d", self._task_queue.qsize())
    
    def get_stats(self) -> Dict[str, Any]:
        """获取统计信息"""
        stats = self.stats.copy()
        if stats['start_time']:
            stats['runtime'] = str(datetime.datetime.now() - stats['start_time'])
        
        # 添加数据库统计
        stats['db_stats'] = self.db_handler.get_text_statistics()
        
        # 添加向量存储统计
        if self.vector_store:
            stats['vector_stats'] = self.vector_store.get_collection_stats()
        
        return stats
    
    def search_text(
        self,
        query: str,
        limit: int = 20,
        offset: int = 0,
        fuzzy: bool = True,
        process_names: Optional[List[str]] = None,
        start_time: Optional[float] = None,
        end_time: Optional[float] = None
    ) -> list:
        """搜索OCR文本"""
        return self.db_handler.search_text(
            query,
            limit=limit,
            offset=offset,
            fuzzy=fuzzy,
            process_names=process_names,
            start_time=start_time,
            end_time=end_time
        )
    
    def search_by_natural_language(
        self,
        query: str,
        n_results: int = 10,
        offset: int = 0,
        process_names: Optional[List[str]] = None,
        start_time: Optional[float] = None,
        end_time: Optional[float] = None
    ) -> list:
        """使用自然语言搜索图片"""
        import time as _time
        _t_total = _time.perf_counter()

        if not self.vector_store:
            raise RuntimeError("向量存储未启用")

        # 增加查询数量以便在过滤后仍能满足需求
        target_count = max(int(n_results) + int(offset), int(n_results))
        buffer_multiplier = 2
        fetch_count = max(target_count * buffer_multiplier, target_count + 20)

        _t0 = _time.perf_counter()
        raw_results = self.vector_store.search_by_text(query, n_results=fetch_count)
        _t_vector_search = _time.perf_counter() - _t0

        filtered: List[Dict[str, Any]] = []
        normalized_processes = None
        if process_names:
            normalized_processes = [p for p in process_names if isinstance(p, str) and p.strip()]

        def _parse_timestamp(value: Optional[str]) -> Optional[float]:
            if not value:
                return None
            try:
                return datetime.datetime.strptime(value, '%Y-%m-%d %H:%M:%S').timestamp()
            except ValueError:
                return None

        start_ts = float(start_time) if start_time is not None else None
        end_ts = float(end_time) if end_time is not None else None

        _t0 = _time.perf_counter()
        _db_query_count = 0
        for item in raw_results:
            metadata = item.get('metadata') or {}
            process_name = (metadata.get('process_name') or '').strip()

            if normalized_processes and process_name not in normalized_processes:
                continue

            created_at_str = metadata.get('created_at') or metadata.get('screenshot_created_at')
            created_ts = _parse_timestamp(created_at_str)

            # 确保 screenshot_created_at 始终设置（前端时间线跳转依赖此字段）
            if created_at_str:
                if 'screenshot_created_at' not in metadata:
                    metadata['screenshot_created_at'] = created_at_str
                item['screenshot_created_at'] = created_at_str

            if start_ts is not None and created_ts is not None and created_ts < start_ts:
                continue
            if end_ts is not None and created_ts is not None and created_ts > end_ts:
                continue

            filtered.append(item)
        _t_filter = _time.perf_counter() - _t0

        # 应用偏移与限制
        result = filtered[int(offset): int(offset) + int(n_results)]

        if (_time.perf_counter() - _t_total) > 5.0:
            logger.warning(
                "[DIAG:search_nl] vector_search=%.3fs filter=%.3fs "
                "db_queries=%d raw=%d filtered=%d returned=%d total=%.3fs",
                _t_vector_search, _t_filter,
                _db_query_count, len(raw_results), len(filtered), len(result),
                _time.perf_counter() - _t_total
            )
        return result

    def list_processes(self, limit: Optional[int] = None) -> List[Dict[str, Any]]:
        """返回数据库中的进程列表"""
        return self.db_handler.list_distinct_processes(limit=limit)
    
    def process_existing_screenshots(self):
        """处理目录中已存在的所有截图"""
        logger.info("处理 %s 中的现有截图...", self.screenshot_dir)

        count = 0
        for ext in ['*.jpg', '*.png', '*.jpeg']:
            for file_path in self.screenshot_dir.glob(ext):
                logger.info("处理: %s", file_path)
                result = self.process_image(str(file_path))
                if result['success']:
                    count += 1
                    logger.info("  -> 识别到 %d 个文本块", len(result['ocr_results']))
                else:
                    logger.error("  -> 失败: %s", result['error'])

        logger.info("处理完成，共处理 %d 张图片", count)
        return count


class ScreenshotOCRService:
    """截图OCR服务 - 整合截图和OCR功能"""
    
    def __init__(
        self,
        screenshot_dir: str = "./screenshots",
        db_path: str = "./ocr_data.db",
        vector_db_path: str = "./chroma_db",
        capture_interval: float = 5.0,
        enable_vector_store: bool = True
    ):
        """
        初始化服务
        
        Args:
            screenshot_dir: 截图目录
            db_path: 数据库路径
            vector_db_path: 向量数据库路径
            capture_interval: 截图间隔
            enable_vector_store: 是否启用向量存储
        """
        self.screenshot_dir = screenshot_dir
        self.capture_interval = capture_interval
        
        # 初始化OCR工作进程
        self.ocr_worker = ScreenshotOCRWorker(
            screenshot_dir=screenshot_dir,
            db_path=db_path,
            vector_db_path=vector_db_path,
            enable_vector_store=enable_vector_store
        )
        
        # 截图相关
        self._capture_thread: Optional[threading.Thread] = None
        self._capture_stop_event = threading.Event()
    
    def _capture_loop(self):
        """截图循环"""
        from monitor.capture import capture_focused_window_memory
        
        logger.info("截图循环已启动，间隔 %s 秒", self.capture_interval)
        
        while not self._capture_stop_event.is_set():
            ts = datetime.datetime.now().strftime('%Y%m%d_%H%M%S')
            # 如果队列中已有未处理的任务，则跳过截图以避免积压
            try:
                MAX_PENDING = 1
                if hasattr(self, 'ocr_worker') and self.ocr_worker is not None:
                    pending = self.ocr_worker.pending_count()
                    if pending >= MAX_PENDING:
                        logger.info("[capture] Skipping capture because pending OCR tasks = %d", pending)
                        self._capture_stop_event.wait(self.capture_interval)
                        continue
            except Exception:
                # 若 pending_count 不可用则继续截图
                pass
            
            try:
                # 获取窗口标题
                try:
                    import win32gui
                    hwnd = win32gui.GetForegroundWindow()
                    window_title = win32gui.GetWindowText(hwnd)
                except Exception:
                    window_title = None

                # 强制使用内存捕获 + 内存直传。当 storage_client 不可用时，直接抛错并放弃（不写盘、不回退）。
                try:
                    storage_client = getattr(self.ocr_worker, 'storage_client', None)
                except Exception:
                    storage_client = None

                if not storage_client:
                    raise RuntimeError("Storage client not initialized; refusing to write plaintext screenshots to disk")

                # 捕获到内存（bytes + PIL.Image）
                img_bytes, img_pil, monitor, _title = capture_focused_window_memory()
                # prefer window_title from capture if available
                if _title and _title != "Capture Failed":
                    window_title = _title

                if not img_bytes or img_pil is None:
                    raise RuntimeError("Capture failed: no image data")

                width, height = img_pil.size

                image_hash = None
                try:
                    image_hash = hashlib.md5(img_bytes).hexdigest()
                except Exception:
                    image_hash = ''

                temp_res = storage_client.save_screenshot_temp(
                    image_data=img_bytes,
                    image_hash=image_hash or '',
                    width=width,
                    height=height,
                    window_title=window_title,
                    process_name=None,
                    metadata={'monitor': monitor}
                )

                if temp_res.get('status') == 'success' or temp_res.get('screenshot_id'):
                    screenshot_id = temp_res.get('screenshot_id') or temp_res.get('id')
                    # 将图片以内存方式加入 OCR 队列
                    self.ocr_worker.queue_image_from_memory(
                        image_bytes=img_bytes,
                        image_pil=img_pil,
                        window_title=window_title,
                        process_name=None,
                        width=width,
                        height=height,
                        metadata={'monitor': monitor},
                        screenshot_id=screenshot_id
                    )
                else:
                    raise RuntimeError(f"save_screenshot_temp failed: {temp_res.get('error')}")
            except Exception as e:
                logger.error("[%s] 截图失败: %s", ts, e)
                raise

            # 等待下一次检查
            self._capture_stop_event.wait(self.capture_interval)
        logger.info("截图循环已停止")
    
    def start(self):
        """启动服务（截图 + OCR）"""
        # 启动OCR工作进程
        self.ocr_worker.start(watch_dir=True)
        
        # 启动截图线程
        self._capture_stop_event.clear()
        self._capture_thread = threading.Thread(
            target=self._capture_loop,
            daemon=True
        )
        self._capture_thread.start()
        
        logger.info("截图OCR服务已启动")
    
    def stop(self):
        """停止服务"""
        self._capture_stop_event.set()
        self.ocr_worker.stop()
        
        if self._capture_thread:
            self._capture_thread.join(timeout=5.0)
        
        logger.info("截图OCR服务已停止")
    
    def get_stats(self):
        """获取统计信息"""
        return self.ocr_worker.get_stats()


if __name__ == "__main__":
    import argparse
    
    parser = argparse.ArgumentParser(description="截图OCR工作进程")
    parser.add_argument("--dir", default="./screenshots", help="截图目录")
    parser.add_argument("--db", default="./ocr_data.db", help="数据库路径")
    parser.add_argument("--interval", type=float, default=5.0, help="检查间隔")
    parser.add_argument("--process-existing", action="store_true", help="处理现有截图")
    parser.add_argument("--no-vector", action="store_true", help="禁用向量存储")
    
    args = parser.parse_args()
    
    # 创建工作进程
    worker = ScreenshotOCRWorker(
        screenshot_dir=args.dir,
        db_path=args.db,
        enable_vector_store=not args.no_vector
    )
    
    # 处理现有截图
    if args.process_existing:
        worker.process_existing_screenshots()
    
    # 启动监视
    logger.info("启动截图监视...")
    worker.start(watch_dir=True, interval=args.interval)
    
    # 保持运行
    try:
        while True:
            time.sleep(1)
    except KeyboardInterrupt:
        print("\n正在停止...")
        worker.stop()

        # 打印统计信息
        stats = worker.get_stats()
        print(f"\n统计信息:")
        print(f"  处理图片: {stats['processed_count']}")
        print(f"  识别文本: {stats['total_texts_found']}")
        print(f"  失败: {stats['failed_count']}")
