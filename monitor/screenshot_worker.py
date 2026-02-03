"""
截图OCR工作进程模块 - 长时运行的截图处理和OCR识别
"""
import os
import time
import datetime
import threading
import queue
from typing import Optional, Callable, Dict, Any, List
from pathlib import Path
from PIL import Image

# 导入相关模块
from ocr_engine import OCREngine, OCRVisualizer, get_ocr_engine
from db_handler import OCRDatabaseHandler
from vector_store import VectorStore


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
        visualization_dir: str = "./visualizations"
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
        """
        self.screenshot_dir = Path(screenshot_dir)
        self.screenshot_dir.mkdir(parents=True, exist_ok=True)
        
        self.ocr_confidence_threshold = ocr_confidence_threshold
        self.save_visualizations = save_visualizations
        self.visualization_dir = Path(visualization_dir)
        if save_visualizations:
            self.visualization_dir.mkdir(parents=True, exist_ok=True)
        
        # 初始化组件
        print("初始化OCR引擎...")
        try:
            self.ocr_engine = get_ocr_engine()
            print("OCR引擎实例已获取")
        except Exception as e:
            print(f"OCR引擎初始化失败: {e}")
            import traceback
            traceback.print_exc()
            raise
        
        print("初始化数据库处理器...")
        self.db_handler = OCRDatabaseHandler(db_path)
        
        self.enable_vector_store = enable_vector_store
        self.vector_store: Optional[VectorStore] = None
        if enable_vector_store:
            print("初始化向量存储...")
            self.vector_store = VectorStore(
                collection_name="screenshots",
                persist_directory=vector_db_path
            )
        
        if save_visualizations:
            self.visualizer = OCRVisualizer()
        
        # 控制变量
        self._stop_event = threading.Event()
        self._pause_event = threading.Event()
        self._worker_thread: Optional[threading.Thread] = None
        
        # 任务队列（用于异步处理）
        self._task_queue: queue.Queue = queue.Queue()
        
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
            on_ocr_complete: OCR完成回调 (image_path, result)
            on_error: 错误回调 (image_path, exception)
        """
        self._on_ocr_complete = on_ocr_complete
        self._on_error = on_error
    
    def process_image(
        self,
        image_path: str,
        window_title: str = None,
        process_name: str = None,
        metadata: Dict = None
    ) -> Dict[str, Any]:
        """
        处理单张图片
        
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
            
            # OCR识别
            print(f"[OCR] 开始识别: {image_path}")
            ocr_results = self.ocr_engine.recognize(image)
            print(f"[OCR] 识别完成，得到 {len(ocr_results) if ocr_results else 0} 个原始结果")
            
            # 过滤低置信度结果
            filtered_results = [
                r for r in ocr_results 
                if r['confidence'] >= self.ocr_confidence_threshold
            ]
            result['ocr_results'] = filtered_results
            
            # 合并OCR文本
            ocr_text = ' '.join([r['text'] for r in filtered_results])
            
            # 保存到SQLite
            db_result = self.db_handler.save_ocr_data(
                image_path=image_path,
                ocr_results=filtered_results,
                width=width,
                height=height,
                window_title=window_title,
                process_name=process_name,
                metadata=metadata
            )
            result['db_result'] = db_result
            
            # 保存到向量存储
            if self.enable_vector_store and self.vector_store:
                screenshot_id_val = db_result.get('screenshot_id') if db_result else None
                try:
                    vector_id = self.vector_store.add_image(
                        image_path=image_path,
                        image=image,
                        metadata={
                            'window_title': window_title or '',
                            'process_name': process_name or '',
                            'width': width,
                            'height': height,
                            'text_count': len(filtered_results),
                            'screenshot_id': screenshot_id_val if screenshot_id_val else -1,
                            'created_at': datetime.datetime.now().strftime('%Y-%m-%d %H:%M:%S')
                        },
                        ocr_text=ocr_text
                    )
                    print(f"[screenshot_worker] add_image returned vector_id={vector_id}")
                    result['vector_id'] = vector_id
                except Exception as e:
                    print(f"[screenshot_worker] add_image raised exception: {e}")
                    result['vector_id'] = None
            
            # 保存可视化结果
            if self.save_visualizations and filtered_results:
                vis_path = self.visualization_dir / f"vis_{Path(image_path).stem}.jpg"
                self.visualizer.save_visualization(
                    image, filtered_results, str(vis_path),
                    show_confidence=True
                )
            
            result['success'] = True
            self.stats['processed_count'] += 1
            self.stats['total_texts_found'] += len(filtered_results)
            
        except Exception as e:
            result['error'] = str(e)
            self.stats['failed_count'] += 1
            if self._on_error:
                self._on_error(image_path, e)
        
        if result['success'] and self._on_ocr_complete:
            self._on_ocr_complete(image_path, result)
        
        return result
    
    def _worker_loop(self, watch_dir: bool = True, interval: float = 5.0):
        """
        工作循环
        
        Args:
            watch_dir: 是否监视目录中的新文件
            interval: 检查间隔（秒）
        """
        self.stats['start_time'] = datetime.datetime.now()
        processed_files = set()
        
        print(f"截图OCR工作进程已启动，监视目录: {self.screenshot_dir}")
        
        while not self._stop_event.is_set():
            # 检查暂停
            if self._pause_event.is_set():
                time.sleep(0.2)
                continue
            
            # 处理任务队列中的任务
            while not self._task_queue.empty():
                try:
                    task = self._task_queue.get_nowait()
                    print(f"[OCR Worker] 开始处理队列任务: {task.get('image_path', 'unknown')}")
                    try:
                        result = self.process_image(**task)
                        if result.get('success'):
                            print(f"[OCR Worker] 任务完成: 识别到 {len(result.get('ocr_results', []))} 个文本块")
                        else:
                            print(f"[OCR Worker] 任务失败: {result.get('error', 'unknown error')}")
                    except Exception as proc_err:
                        print(f"[OCR Worker] process_image 异常: {proc_err}")
                        import traceback
                        traceback.print_exc()
                except queue.Empty:
                    break
            
            # 监视目录中的新文件
            if watch_dir:
                try:
                    for file_path in self.screenshot_dir.glob("*.jpg"):
                        if str(file_path) not in processed_files:
                            # 等待文件写入完成
                            time.sleep(0.5)
                            
                            print(f"发现新截图: {file_path}")
                            result = self.process_image(str(file_path))
                            
                            if result['success']:
                                print(f"处理完成: 识别到 {len(result['ocr_results'])} 个文本块")
                            else:
                                print(f"处理失败: {result['error']}")
                            
                            processed_files.add(str(file_path))
                    
                    # 同时检查 PNG 文件
                    for file_path in self.screenshot_dir.glob("*.png"):
                        if str(file_path) not in processed_files:
                            time.sleep(0.5)
                            print(f"发现新截图: {file_path}")
                            result = self.process_image(str(file_path))
                            
                            if result['success']:
                                print(f"处理完成: 识别到 {len(result['ocr_results'])} 个文本块")
                            else:
                                print(f"处理失败: {result['error']}")
                            
                            processed_files.add(str(file_path))
                            
                except Exception as e:
                    print(f"监视目录时出错: {e}")
            
            # 等待下一次检查
            self._stop_event.wait(interval)
        
        print("截图OCR工作进程已停止")
    
    def start(self, watch_dir: bool = True, interval: float = 5.0):
        """
        启动工作进程
        
        Args:
            watch_dir: 是否监视目录
            interval: 检查间隔（秒）
        """
        if self._worker_thread and self._worker_thread.is_alive():
            print("工作进程已在运行")
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
        print("工作进程已暂停")
    
    def resume(self):
        """恢复工作进程"""
        self._pause_event.clear()
        print("工作进程已恢复")
    
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
        print(f"[OCR Worker] 添加任务到队列: {image_path}")
        self._task_queue.put({
            'image_path': image_path,
            'window_title': window_title,
            'process_name': process_name,
            'metadata': metadata
        })
        print(f"[OCR Worker] 当前队列大小: {self._task_queue.qsize()}")
    
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
        if not self.vector_store:
            raise RuntimeError("向量存储未启用")

        # 增加查询数量以便在过滤后仍能满足需求
        target_count = max(int(n_results) + int(offset), int(n_results))
        buffer_multiplier = 2
        fetch_count = max(target_count * buffer_multiplier, target_count + 20)

        raw_results = self.vector_store.search_by_text(query, n_results=fetch_count)

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

        for item in raw_results:
            metadata = item.get('metadata') or {}
            process_name = (metadata.get('process_name') or '').strip()

            if normalized_processes and process_name not in normalized_processes:
                continue

            created_at_str = metadata.get('created_at')
            created_ts = _parse_timestamp(created_at_str)

            # 补充从数据库中获取更精确的时间
            if (start_ts is not None or end_ts is not None) and created_ts is None:
                screenshot_id = metadata.get('screenshot_id')
                try:
                    if screenshot_id and int(screenshot_id) >= 0:
                        record = self.db_handler.get_screenshot_by_id(int(screenshot_id))
                        if record and record.get('created_at'):
                            created_ts = datetime.datetime.strptime(record['created_at'], '%Y-%m-%d %H:%M:%S').timestamp()
                except (ValueError, TypeError):
                    created_ts = None

            if start_ts is not None and created_ts is not None and created_ts < start_ts:
                continue
            if end_ts is not None and created_ts is not None and created_ts > end_ts:
                continue

            filtered.append(item)

        # 应用偏移与限制
        return filtered[int(offset): int(offset) + int(n_results)]

    def list_processes(self, limit: Optional[int] = None) -> List[Dict[str, Any]]:
        """返回数据库中的进程列表"""
        return self.db_handler.list_distinct_processes(limit=limit)
    
    def process_existing_screenshots(self):
        """处理目录中已存在的所有截图"""
        print(f"处理 {self.screenshot_dir} 中的现有截图...")
        
        count = 0
        for ext in ['*.jpg', '*.png', '*.jpeg']:
            for file_path in self.screenshot_dir.glob(ext):
                print(f"处理: {file_path}")
                result = self.process_image(str(file_path))
                if result['success']:
                    count += 1
                    print(f"  -> 识别到 {len(result['ocr_results'])} 个文本块")
                else:
                    print(f"  -> 失败: {result['error']}")
        
        print(f"处理完成，共处理 {count} 张图片")
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
        from monitor.capture import capture_focused_window
        
        print(f"截图循环已启动，间隔 {self.capture_interval} 秒")
        
        while not self._capture_stop_event.is_set():
            ts = datetime.datetime.now().strftime('%Y%m%d_%H%M%S')
            out_file = os.path.join(self.screenshot_dir, f'shot_{ts}.jpg')
            
            try:
                # 获取窗口标题
                try:
                    import win32gui
                    hwnd = win32gui.GetForegroundWindow()
                    window_title = win32gui.GetWindowText(hwnd)
                except:
                    window_title = None
                
                monitor = capture_focused_window(out_file)
                print(f"[{ts}] 截图已保存: {out_file}")
                
                # 添加到OCR处理队列
                self.ocr_worker.add_task(
                    image_path=out_file,
                    window_title=window_title,
                    metadata={'monitor': monitor}
                )
                
            except Exception as e:
                print(f"[{ts}] 截图失败: {e}")
            
            self._capture_stop_event.wait(self.capture_interval)
        
        print("截图循环已停止")
    
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
        
        print("截图OCR服务已启动")
    
    def stop(self):
        """停止服务"""
        self._capture_stop_event.set()
        self.ocr_worker.stop()
        
        if self._capture_thread:
            self._capture_thread.join(timeout=5.0)
        
        print("截图OCR服务已停止")
    
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
    print("启动截图监视...")
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
