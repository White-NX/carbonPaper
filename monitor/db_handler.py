"""
数据库交互器模块 - SQLite存储OCR数据，支持去重和检索
"""
import sqlite3
import json
import hashlib
import logging
import os
from datetime import datetime
from typing import List, Dict, Any, Optional, Tuple
from contextlib import contextmanager

logger = logging.getLogger(__name__)


class OCRDatabaseHandler:
    """OCR数据SQLite数据库处理器"""
    
    def __init__(self, db_path: str = "ocr_data.db"):
        """
        初始化数据库处理器
        
        Args:
            db_path: 数据库文件路径
        """
        self.db_path = db_path
        self._init_database()
    
    def _init_database(self):
        """初始化数据库表结构"""
        with self._get_connection() as conn:
            cursor = conn.cursor()
            
            # 截图记录表
            cursor.execute('''
                CREATE TABLE IF NOT EXISTS screenshots (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    image_path TEXT NOT NULL,
                    image_hash TEXT UNIQUE NOT NULL,
                    width INTEGER,
                    height INTEGER,
                    window_title TEXT,
                    process_name TEXT,
                    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                    metadata TEXT
                )
            ''')
            
            # OCR结果表
            cursor.execute('''
                CREATE TABLE IF NOT EXISTS ocr_results (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    screenshot_id INTEGER NOT NULL,
                    text TEXT NOT NULL,
                    text_hash TEXT NOT NULL,
                    confidence REAL,
                    box_x1 REAL, box_y1 REAL,
                    box_x2 REAL, box_y2 REAL,
                    box_x3 REAL, box_y3 REAL,
                    box_x4 REAL, box_y4 REAL,
                    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                    FOREIGN KEY (screenshot_id) REFERENCES screenshots(id) ON DELETE CASCADE
                )
            ''')
            
            # 文本内容索引表（用于快速去重和检索）
            cursor.execute('''
                CREATE TABLE IF NOT EXISTS text_index (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    text_hash TEXT UNIQUE NOT NULL,
                    text TEXT NOT NULL,
                    first_seen TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                    last_seen TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                    occurrence_count INTEGER DEFAULT 1
                )
            ''')
            
            # 创建索引
            cursor.execute('CREATE INDEX IF NOT EXISTS idx_image_hash ON screenshots(image_hash)')
            cursor.execute('CREATE INDEX IF NOT EXISTS idx_text_hash ON ocr_results(text_hash)')
            cursor.execute('CREATE INDEX IF NOT EXISTS idx_screenshot_id ON ocr_results(screenshot_id)')
            cursor.execute('CREATE INDEX IF NOT EXISTS idx_text_content ON text_index(text)')
            
            # 启用外键约束
            cursor.execute('PRAGMA foreign_keys = ON')
            
            conn.commit()
    
    @contextmanager
    def _get_connection(self):
        """获取数据库连接的上下文管理器"""
        conn = sqlite3.connect(self.db_path)
        conn.row_factory = sqlite3.Row
        try:
            yield conn
        finally:
            conn.close()
    
    @staticmethod
    def _compute_hash(data: Any) -> str:
        """计算数据的哈希值"""
        if isinstance(data, bytes):
            return hashlib.md5(data).hexdigest()
        return hashlib.md5(str(data).encode('utf-8')).hexdigest()

    def get_screenshots_by_time_range(self, start_ts: float, end_ts: float) -> List[Dict[str, Any]]:
        """
        获取指定时间范围内的截图记录
        
        Args:
           start_ts: 开始时间戳 (秒)
           end_ts: 结束时间戳 (秒)
        """
        # Ensure we use UTC time for query because SQLite stores TIMESTAMP as UTC by default (CURRENT_TIMESTAMP)
        # using fromtimestamp without timezone info creates local time, which causes offset issues.
        start_dt = datetime.utcfromtimestamp(start_ts)
        end_dt = datetime.utcfromtimestamp(end_ts)
        
        with self._get_connection() as conn:
            cursor = conn.cursor()
            # SQLite strftime('%s', created_at) returns seconds string. Cast to integer.
            cursor.execute('''
                  SELECT id, image_path, window_title, process_name, 
                      strftime('%s', created_at) as timestamp, 
                      width, height,
                      metadata
                FROM screenshots
                WHERE created_at BETWEEN ? AND ?
                ORDER BY created_at ASC
            ''', (start_dt, end_dt))
            
            rows = cursor.fetchall()
            return [dict(row) for row in rows]

    def get_screenshot_by_id(self, screenshot_id: int) -> Optional[Dict[str, Any]]:
        """根据ID获取截图记录"""
        with self._get_connection() as conn:
            cursor = conn.cursor()
            cursor.execute('SELECT * FROM screenshots WHERE id = ?', (screenshot_id,))
            row = cursor.fetchone()
            return dict(row) if row else None

    def get_screenshot_by_path(self, image_path: str) -> Optional[Dict[str, Any]]:
        """根据图片路径获取截图记录（精确匹配或文件名匹配）"""
        # Normalize path separators
        image_path = os.path.normpath(image_path).replace('\\', '/')
        
        with self._get_connection() as conn:
            cursor = conn.cursor()
            
            # 1. 尝试精确匹配
            cursor.execute('SELECT * FROM screenshots WHERE replace(image_path, "\\", "/") = ?', (image_path,))
            row = cursor.fetchone()
            if row: return dict(row)
            
            # 2. 尝试匹配文件名
            basename = os.path.basename(image_path)
            cursor.execute('SELECT * FROM screenshots WHERE image_path LIKE ?', (f'%{basename}',))
            row = cursor.fetchone()
            if row: return dict(row)
            
            return None

    @staticmethod
    def _compute_image_hash(image_path: str) -> str:
        """计算图片文件的哈希值"""
        with open(image_path, 'rb') as f:
            return hashlib.md5(f.read()).hexdigest()
    
    def screenshot_exists(self, image_hash: str) -> bool:
        """检查截图是否已存在"""
        with self._get_connection() as conn:
            cursor = conn.cursor()
            cursor.execute('SELECT id FROM screenshots WHERE image_hash = ?', (image_hash,))
            return cursor.fetchone() is not None
    
    def add_screenshot(
        self,
        image_path: str,
        width: int = None,
        height: int = None,
        window_title: str = None,
        process_name: str = None,
        metadata: Dict = None,
        image_hash: str = None
    ) -> Optional[int]:
        """
        添加截图记录
        
        Args:
            image_path: 图片路径
            width: 图片宽度
            height: 图片高度
            window_title: 窗口标题
            process_name: 进程名
            metadata: 额外元数据
            image_hash: 图片哈希（可选，不提供则自动计算）
            
        Returns:
            截图ID，如果已存在则返回None
        """
        if image_hash is None:
            image_hash = self._compute_image_hash(image_path)
        
        # 检查是否已存在
        if self.screenshot_exists(image_hash):
            return None
        
        with self._get_connection() as conn:
            cursor = conn.cursor()
            cursor.execute('''
                INSERT INTO screenshots (image_path, image_hash, width, height, window_title, process_name, metadata)
                VALUES (?, ?, ?, ?, ?, ?, ?)
            ''', (
                image_path,
                image_hash,
                width,
                height,
                window_title,
                process_name,
                json.dumps(metadata) if metadata else None
            ))
            conn.commit()
            return cursor.lastrowid
    
    def add_ocr_results(
        self,
        screenshot_id: int,
        ocr_results: List[Dict[str, Any]],
        skip_duplicates: bool = True
    ) -> Tuple[int, int]:
        """
        添加OCR识别结果
        
        Args:
            screenshot_id: 截图ID
            ocr_results: OCR结果列表
            skip_duplicates: 是否跳过重复文本
            
        Returns:
            (添加的记录数, 跳过的重复数)
        """
        added = 0
        skipped = 0
        
        with self._get_connection() as conn:
            cursor = conn.cursor()
            
            for result in ocr_results:
                text = result['text']
                text_hash = self._compute_hash(text)
                box = result['box']
                confidence = result.get('confidence', 0.0)
                
                # 更新文本索引
                cursor.execute('''
                    INSERT INTO text_index (text_hash, text)
                    VALUES (?, ?)
                    ON CONFLICT(text_hash) DO UPDATE SET
                        last_seen = CURRENT_TIMESTAMP,
                        occurrence_count = occurrence_count + 1
                ''', (text_hash, text))
                
                # 检查是否需要跳过重复
                if skip_duplicates:
                    # 检查同一截图中是否已有相同文本（在相近位置）
                    cursor.execute('''
                        SELECT id FROM ocr_results 
                        WHERE screenshot_id = ? AND text_hash = ?
                        AND ABS(box_x1 - ?) < 10 AND ABS(box_y1 - ?) < 10
                    ''', (screenshot_id, text_hash, box[0][0], box[0][1]))
                    
                    if cursor.fetchone():
                        skipped += 1
                        continue
                
                # 插入OCR结果
                cursor.execute('''
                    INSERT INTO ocr_results (
                        screenshot_id, text, text_hash, confidence,
                        box_x1, box_y1, box_x2, box_y2,
                        box_x3, box_y3, box_x4, box_y4
                    ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                ''', (
                    screenshot_id, text, text_hash, confidence,
                    box[0][0], box[0][1], box[1][0], box[1][1],
                    box[2][0], box[2][1], box[3][0], box[3][1]
                ))
                added += 1
            
            conn.commit()
        
        return added, skipped
    
    def save_ocr_data(
        self,
        image_path: str,
        ocr_results: List[Dict[str, Any]],
        width: int = None,
        height: int = None,
        window_title: str = None,
        process_name: str = None,
        metadata: Dict = None
    ) -> Dict[str, Any]:
        """
        保存完整的OCR数据（截图 + 识别结果）
        
        Args:
            image_path: 图片路径
            ocr_results: OCR结果列表
            width: 图片宽度
            height: 图片高度
            window_title: 窗口标题
            process_name: 进程名
            metadata: 额外元数据
            
        Returns:
            保存结果信息
        """
        image_hash = self._compute_image_hash(image_path)
        
        # 添加截图记录
        screenshot_id = self.add_screenshot(
            image_path, width, height, window_title, process_name, metadata, image_hash
        )
        
        if screenshot_id is None:
            # 截图已存在，获取现有ID
            with self._get_connection() as conn:
                cursor = conn.cursor()
                cursor.execute('SELECT id FROM screenshots WHERE image_hash = ?', (image_hash,))
                row = cursor.fetchone()
                return {
                    'status': 'duplicate',
                    'screenshot_id': row['id'] if row else None,
                    'added': 0,
                    'skipped': len(ocr_results)
                }
        
        # 添加OCR结果
        added, skipped = self.add_ocr_results(screenshot_id, ocr_results)
        
        return {
            'status': 'success',
            'screenshot_id': screenshot_id,
            'added': added,
            'skipped': skipped
        }
    
    def search_text(
        self,
        query: str,
        limit: int = 100,
        offset: int = 0,
        fuzzy: bool = True,
        process_names: Optional[List[str]] = None,
        start_time: Optional[float] = None,
        end_time: Optional[float] = None
    ) -> List[Dict[str, Any]]:
        """
        搜索文本
        
        Args:
            query: 搜索关键词
            limit: 返回结果数量限制
            offset: 偏移量，用于分页
            fuzzy: 是否模糊匹配
            process_names: 进程名过滤列表
            start_time: 起始时间戳（秒，UTC）
            end_time: 结束时间戳（秒，UTC）
            
        Returns:
            搜索结果列表
        """
        with self._get_connection() as conn:
            cursor = conn.cursor()

            clauses = []
            params: List[Any] = []
            relevance_params: List[Any] = []
            relevance_expr = None

            if query:
                query = str(query)
                if fuzzy:
                    terms = [t for t in query.split() if t]
                    if terms:
                        for term in terms:
                            term_norm = term.lower()
                            clauses.append('lower(r.text) LIKE ?')
                            params.append(f'%{term_norm}%')
                        relevance_expr = ' + '.join(
                            ['(CASE WHEN length(?) > 0 THEN '
                             '(length(lower(r.text)) - length(replace(lower(r.text), ?, \'\'))) / length(?) '
                             'ELSE 0 END)'] * len(terms)
                        )
                        for term in terms:
                            term_norm = term.lower()
                            relevance_params.extend([term_norm, term_norm, term_norm])
                    else:
                        clauses.append('1=1')
                else:
                    clauses.append('r.text = ?')
                    params.append(query)
            else:
                clauses.append('1=1')

            if process_names:
                normalized = [p for p in process_names if isinstance(p, str) and p.strip()]
                if normalized:
                    placeholders = ','.join(['?'] * len(normalized))
                    clauses.append(f"(s.process_name IN ({placeholders}))")
                    params.extend(normalized)

            def _to_sqlite_ts(ts_value: Optional[float]) -> Optional[str]:
                if ts_value is None:
                    return None
                try:
                    return datetime.utcfromtimestamp(float(ts_value)).strftime('%Y-%m-%d %H:%M:%S')
                except Exception:
                    return None

            start_ts_str = _to_sqlite_ts(start_time)
            end_ts_str = _to_sqlite_ts(end_time)

            if start_ts_str:
                clauses.append('s.created_at >= ?')
                params.append(start_ts_str)
            if end_ts_str:
                clauses.append('s.created_at <= ?')
                params.append(end_ts_str)

            where_clause = ' AND '.join(clauses) if clauses else '1=1'

            params.extend([max(int(limit), 0), max(int(offset), 0)])

            select_relevance = ''
            order_clause = 'ORDER BY s.created_at DESC, r.id DESC'
            if relevance_expr:
                select_relevance = f", {relevance_expr} as relevance"
                order_clause = 'ORDER BY relevance DESC, s.created_at DESC, r.id DESC'

            cursor.execute(f'''
                SELECT
                    r.*, 
                    s.id as screenshot_id,
                    s.image_path,
                    s.window_title,
                    s.process_name,
                    s.created_at as screenshot_created_at{select_relevance}
                FROM ocr_results r
                JOIN screenshots s ON r.screenshot_id = s.id
                WHERE {where_clause}
                {order_clause}
                LIMIT ? OFFSET ?
            ''', relevance_params + params)
            
            results = []
            for row in cursor.fetchall():
                results.append({
                    'id': row['id'],
                    'screenshot_id': row['screenshot_id'],
                    'text': row['text'],
                    'confidence': row['confidence'],
                    'box': [
                        [row['box_x1'], row['box_y1']],
                        [row['box_x2'], row['box_y2']],
                        [row['box_x3'], row['box_y3']],
                        [row['box_x4'], row['box_y4']]
                    ],
                    'image_path': row['image_path'],
                    'window_title': row['window_title'],
                    'process_name': row['process_name'],
                    'created_at': row['created_at'],
                    'screenshot_created_at': row['screenshot_created_at']
                })

            return results

    def list_distinct_processes(self, limit: Optional[int] = None) -> List[Dict[str, Any]]:
        """列出数据库中的进程名及数量"""
        """@deprecated Python Subservice will never manage database content"""
        with self._get_connection() as conn:
            cursor = conn.cursor()
            sql = (
                'SELECT process_name, COUNT(*) as count '
                'FROM screenshots '
                'WHERE process_name IS NOT NULL AND TRIM(process_name) != "" '
                'GROUP BY process_name '
                'ORDER BY count DESC, process_name COLLATE NOCASE ASC'
            )
            params: Tuple[Any, ...] = tuple()
            if limit is not None:
                sql += ' LIMIT ?'
                params = (int(limit),)
            cursor.execute(sql, params)
            rows = cursor.fetchall()
            return [
                {
                    'process_name': row['process_name'],
                    'count': row['count']
                }
                for row in rows
            ]
    
    def get_screenshot_ocr_results(self, screenshot_id: int) -> List[Dict[str, Any]]:
        """获取指定截图的所有OCR结果"""
        with self._get_connection() as conn:
            cursor = conn.cursor()
            cursor.execute('''
                SELECT * FROM ocr_results WHERE screenshot_id = ?
                ORDER BY box_y1, box_x1
            ''', (screenshot_id,))
            
            results = []
            for row in cursor.fetchall():
                results.append({
                    'id': row['id'],
                    'text': row['text'],
                    'confidence': row['confidence'],
                    'box': [
                        [row['box_x1'], row['box_y1']],
                        [row['box_x2'], row['box_y2']],
                        [row['box_x3'], row['box_y3']],
                        [row['box_x4'], row['box_y4']]
                    ]
                })
            
            return results
    
    def get_recent_screenshots(self, limit: int = 50) -> List[Dict[str, Any]]:
        """获取最近的截图记录"""
        with self._get_connection() as conn:
            cursor = conn.cursor()
            cursor.execute('''
                SELECT s.*, COUNT(r.id) as text_count
                FROM screenshots s
                LEFT JOIN ocr_results r ON s.id = r.screenshot_id
                GROUP BY s.id
                ORDER BY s.created_at DESC
                LIMIT ?
            ''', (limit,))
            
            results = []
            for row in cursor.fetchall():
                results.append({
                    'id': row['id'],
                    'image_path': row['image_path'],
                    'window_title': row['window_title'],
                    'width': row['width'],
                    'height': row['height'],
                    'text_count': row['text_count'],
                    'created_at': row['created_at']
                })
            
            return results
    
    def get_text_statistics(self) -> Dict[str, Any]:
        """获取文本统计信息"""
        with self._get_connection() as conn:
            cursor = conn.cursor()
            
            # 获取总计数
            cursor.execute('SELECT COUNT(*) FROM screenshots')
            screenshot_count = cursor.fetchone()[0]
            
            cursor.execute('SELECT COUNT(*) FROM ocr_results')
            ocr_result_count = cursor.fetchone()[0]
            
            cursor.execute('SELECT COUNT(*) FROM text_index')
            unique_text_count = cursor.fetchone()[0]
            
            # 获取高频文本
            cursor.execute('''
                SELECT text, occurrence_count 
                FROM text_index 
                ORDER BY occurrence_count DESC 
                LIMIT 10
            ''')
            frequent_texts = [
                {'text': row['text'], 'count': row['occurrence_count']}
                for row in cursor.fetchall()
            ]
            
            return {
                'screenshot_count': screenshot_count,
                'ocr_result_count': ocr_result_count,
                'unique_text_count': unique_text_count,
                'frequent_texts': frequent_texts
            }
    
    def delete_screenshot(self, screenshot_id: int) -> bool:
        """删除截图及其OCR结果"""
        with self._get_connection() as conn:
            cursor = conn.cursor()
            # First get the image path to delete the file
            cursor.execute('SELECT image_path FROM screenshots WHERE id = ?', (screenshot_id,))
            row = cursor.fetchone()
            image_path = row['image_path'] if row else None
            
            # Delete from database
            cursor.execute('DELETE FROM screenshots WHERE id = ?', (screenshot_id,))
            conn.commit()
            deleted = cursor.rowcount > 0
            
            # Try to delete the image file
            if deleted and image_path:
                try:
                    if os.path.exists(image_path):
                        os.remove(image_path)
                except Exception as e:
                    logger.error("Failed to delete image file %s: %s", image_path, e)
            
            return deleted
    
    def delete_screenshots_by_time_range(self, start_ts: float, end_ts: float) -> int:
        """删除指定时间范围内的截图及其OCR结果
        
        Args:
            start_ts: 开始时间戳 (毫秒)
            end_ts: 结束时间戳 (毫秒)
            
        Returns:
            删除的记录数
        """
        # Convert milliseconds to seconds for datetime
        start_dt = datetime.utcfromtimestamp(start_ts / 1000.0)
        end_dt = datetime.utcfromtimestamp(end_ts / 1000.0)
        
        deleted_count = 0
        
        with self._get_connection() as conn:
            cursor = conn.cursor()
            
            # First get all image paths to delete files
            cursor.execute('''
                SELECT id, image_path FROM screenshots
                WHERE created_at BETWEEN ? AND ?
            ''', (start_dt, end_dt))
            
            rows = cursor.fetchall()
            image_paths = [(row['id'], row['image_path']) for row in rows]
            
            # Delete from database
            cursor.execute('''
                DELETE FROM screenshots
                WHERE created_at BETWEEN ? AND ?
            ''', (start_dt, end_dt))
            
            deleted_count = cursor.rowcount
            conn.commit()
            
            # Try to delete the image files
            for screenshot_id, image_path in image_paths:
                try:
                    if image_path and os.path.exists(image_path):
                        os.remove(image_path)
                except Exception as e:
                    logger.error("Failed to delete image file %s: %s", image_path, e)
        
        return deleted_count
    
    def cleanup_old_data(self, days: int = 30) -> Tuple[int, int]:
        """清理指定天数之前的旧数据"""
        with self._get_connection() as conn:
            cursor = conn.cursor()
            
            # 删除旧截图（级联删除OCR结果）
            cursor.execute('''
                DELETE FROM screenshots 
                WHERE created_at < datetime('now', ? || ' days')
            ''', (f'-{days}',))
            deleted_screenshots = cursor.rowcount
            
            # 清理孤立的文本索引
            cursor.execute('''
                DELETE FROM text_index 
                WHERE text_hash NOT IN (SELECT DISTINCT text_hash FROM ocr_results)
            ''')
            deleted_indices = cursor.rowcount
            
            conn.commit()
            return deleted_screenshots, deleted_indices


if __name__ == "__main__":
    # 测试代码
    db = OCRDatabaseHandler("test_ocr.db")
    
    # 模拟OCR结果
    test_results = [
        {
            'box': [[10, 10], [100, 10], [100, 30], [10, 30]],
            'text': '测试文本',
            'confidence': 0.95
        },
        {
            'box': [[10, 50], [200, 50], [200, 80], [10, 80]],
            'text': 'Hello World',
            'confidence': 0.98
        }
    ]
    
    # 保存测试数据
    result = db.save_ocr_data(
        image_path="test_image.jpg",
        ocr_results=test_results,
        width=800,
        height=600,
        window_title="Test Window"
    )
    print(f"保存结果: {result}")
    
    # 搜索测试
    search_results = db.search_text("测试")
    print(f"搜索结果: {search_results}")
    
    # 统计信息
    stats = db.get_text_statistics()
    print(f"统计信息: {stats}")
