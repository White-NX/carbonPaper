"""
存储客户端模块 - 通过 IPC 与 Rust 存储服务通信
"""
import json
import time
import threading
from collections import OrderedDict
import win32file
import win32pipe
import pywintypes
from typing import Optional, Dict, Any, List


class StorageClient:
    """与 Rust 存储服务通信的客户端"""
    
    def __init__(self, pipe_name: str):
        r"""
        初始化存储客户端
        
        Args:
            pipe_name: Rust 存储服务的管道名（不含 \\.\pipe\ 前缀）
        """
        self.pipe_name = pipe_name
        self.full_pipe_name = rf"\\.\pipe\{pipe_name}"
        self._public_key: Optional[bytes] = None
        self._semaphore = threading.Semaphore(2)
        self._decrypt_cache = OrderedDict()
        self._encrypt_cache = OrderedDict()
        self._cache_limit = 512

    def _cache_get(self, cache: OrderedDict, key: str) -> Optional[str]:
        value = cache.get(key)
        if value is not None:
            cache.move_to_end(key)
        return value

    def _cache_set(self, cache: OrderedDict, key: str, value: str) -> None:
        cache[key] = value
        cache.move_to_end(key)
        if len(cache) > self._cache_limit:
            cache.popitem(last=False)
    
    def _send_request(self, request: Dict[str, Any]) -> Dict[str, Any]:
        """
        发送请求到 Rust 存储服务
        
        Args:
            request: 请求数据
            
        Returns:
            响应数据
        """
        try:
            self._semaphore.acquire()
            handle = None
            last_error = None

            # 连接到管道（使用消息模式以支持大数据传输）
            for attempt in range(6):
                try:
                    handle = win32file.CreateFile(
                        self.full_pipe_name,
                        win32file.GENERIC_READ | win32file.GENERIC_WRITE,
                        0,
                        None,
                        win32file.OPEN_EXISTING,
                        0,
                        None
                    )
                    last_error = None
                    break
                except pywintypes.error as e:
                    last_error = e
                    if e.winerror == 231 and attempt < 5:
                        time.sleep(0.02 * (2 ** attempt))
                        continue
                    raise

            if handle is None:
                if last_error is not None:
                    raise last_error
                return {'status': 'error', 'error': 'Failed to connect to pipe'}
            
            try:
                # 设置管道模式为消息读取模式
                win32pipe.SetNamedPipeHandleState(
                    handle,
                    win32pipe.PIPE_READMODE_BYTE,
                    None,
                    None
                )
                
                # 发送请求（分块写入大数据）
                request_bytes = json.dumps(request).encode('utf-8')
                chunk_size = 64 * 1024  # 64KB chunks
                offset = 0
                
                while offset < len(request_bytes):
                    chunk = request_bytes[offset:offset + chunk_size]
                    win32file.WriteFile(handle, chunk)
                    offset += len(chunk)
                
                # 刷新管道确保所有数据都已发送
                win32file.FlushFileBuffers(handle)
                
                # 读取响应（支持分块读取大响应）
                response_bytes = b''
                while True:
                    try:
                        _, chunk = win32file.ReadFile(handle, 64 * 1024)
                        if not chunk:
                            break
                        response_bytes += chunk
                        # 尝试解析，如果成功则说明响应完整
                        try:
                            response = json.loads(response_bytes.decode('utf-8'))
                            return response
                        except json.JSONDecodeError:
                            # 响应不完整，继续读取
                            continue
                    except pywintypes.error as e:
                        # 管道已结束（109）或其他错误
                        if e.winerror == 109:
                            break
                        raise
                
                if response_bytes:
                    response = json.loads(response_bytes.decode('utf-8'))
                    return response
                else:
                    return {'status': 'error', 'error': 'Empty response'}
                    
            finally:
                win32file.CloseHandle(handle)
                
        except pywintypes.error as e:
            return {'status': 'error', 'error': f'IPC error: {e}'}
        except Exception as e:
            return {'status': 'error', 'error': f'Error: {e}'}
        finally:
            self._semaphore.release()
    
    def get_public_key(self) -> Optional[bytes]:
        """
        获取公钥（用于加密 ChromaDB 数据）
        
        Returns:
            公钥字节数据，或 None 如果失败
        """
        if self._public_key is not None:
            return self._public_key
        
        response = self._send_request({'command': 'get_public_key'})
        
        if response.get('status') == 'success':
            data = response.get('data', {})
            public_key_b64 = data.get('public_key')
            if public_key_b64:
                import base64
                self._public_key = base64.b64decode(public_key_b64)
                return self._public_key
        
        print(f"[storage_client] Failed to get public key: {response.get('error')}")
        return None
    
    def encrypt_for_chromadb(self, plaintext: str) -> Optional[str]:
        """
        加密数据（用于 ChromaDB 明文字段）
        
        Args:
            plaintext: 要加密的明文
            
        Returns:
            加密后的 Base64 字符串，或 None 如果失败
        """
        if plaintext:
            cached = self._cache_get(self._encrypt_cache, plaintext)
            if cached is not None:
                return cached

        response = self._send_request({
            'command': 'encrypt_for_chromadb',
            'plaintext': plaintext
        })
        
        if response.get('status') == 'success':
            data = response.get('data', {})
            encrypted = data.get('encrypted')
            if encrypted:
                self._cache_set(self._encrypt_cache, plaintext, encrypted)
            return encrypted
        
        print(f"[storage_client] Encryption failed: {response.get('error')}")
        return None
    
    def decrypt_from_chromadb(self, encrypted: str) -> Optional[str]:
        """
        解密数据
        
        Args:
            encrypted: 加密的 Base64 字符串
            
        Returns:
            解密后的明文，或 None 如果失败
        """
        if encrypted:
            cached = self._cache_get(self._decrypt_cache, encrypted)
            if cached is not None:
                return cached

        response = self._send_request({
            'command': 'decrypt_from_chromadb',
            'encrypted': encrypted
        })
        
        if response.get('status') == 'success':
            data = response.get('data', {})
            decrypted = data.get('decrypted')
            if decrypted is not None:
                self._cache_set(self._decrypt_cache, encrypted, decrypted)
            return decrypted
        
        print(f"[storage_client] Decryption failed: {response.get('error')}")
        return None
    
    def screenshot_exists(self, image_hash: str) -> bool:
        """
        检查截图是否已存在
        
        Args:
            image_hash: 图片哈希
            
        Returns:
            是否存在
        """
        response = self._send_request({
            'command': 'screenshot_exists',
            'image_hash': image_hash
        })
        
        if response.get('status') == 'success':
            data = response.get('data', {})
            return data.get('exists', False)
        
        return False
    
    def save_screenshot(
        self,
        image_data: bytes,
        image_hash: str,
        width: int,
        height: int,
        window_title: Optional[str] = None,
        process_name: Optional[str] = None,
        metadata: Optional[Dict[str, Any]] = None,
        ocr_results: Optional[List[Dict[str, Any]]] = None
    ) -> Dict[str, Any]:
        """
        保存截图到 Rust 存储服务
        
        Args:
            image_data: 图片二进制数据
            image_hash: 图片哈希
            width: 图片宽度
            height: 图片高度
            window_title: 窗口标题
            process_name: 进程名
            metadata: 元数据
            ocr_results: OCR 结果列表
            
        Returns:
            保存结果
        """
        import base64
        
        request = {
            'command': 'save_screenshot',
            'image_data': base64.b64encode(image_data).decode('utf-8'),
            'image_hash': image_hash,
            'width': width,
            'height': height,
            'window_title': window_title,
            'process_name': process_name,
            'metadata': metadata,
            'ocr_results': ocr_results
        }
        
        response = self._send_request(request)
        
        if response.get('status') == 'success':
            return response.get('data', {})
        
        return {
            'status': 'error',
            'error': response.get('error', 'Unknown error')
        }


# 全局存储客户端实例
_storage_client: Optional[StorageClient] = None


def get_storage_client() -> Optional[StorageClient]:
    """获取全局存储客户端实例"""
    return _storage_client


def init_storage_client(pipe_name: str) -> StorageClient:
    """
    初始化全局存储客户端
    
    Args:
        pipe_name: 管道名
        
    Returns:
        存储客户端实例
    """
    global _storage_client
    _storage_client = StorageClient(pipe_name)
    return _storage_client
