"""
Storage client module — communicates with the Rust storage service via IPC.
"""
import json
import time
import logging
import threading
from collections import OrderedDict

logger = logging.getLogger(__name__)
import win32file
import win32pipe
import pywintypes
from typing import Optional, Dict, Any, List


class StorageClient:
    """Client for communicating with the Rust storage service."""
    
    def __init__(self, pipe_name: str):
        r"""
        Initialise the storage client.

        Args:
            pipe_name: Pipe name of the Rust storage service (without the \\.\pipe\ prefix).
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
        Send a request to the Rust storage service.

        Args:
            request: Request payload.

        Returns:
            Response data.
        """
        try:
            self._semaphore.acquire()
            handle = None
            last_error = None

            # Connect to the pipe (byte mode for large data transfer)
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
                # Set pipe mode to byte-read mode
                win32pipe.SetNamedPipeHandleState(
                    handle,
                    win32pipe.PIPE_READMODE_BYTE,
                    None,
                    None
                )
                
                # Send request (write large data in chunks)
                request_bytes = json.dumps(request).encode('utf-8')
                chunk_size = 64 * 1024  # 64KB chunks
                offset = 0
                
                while offset < len(request_bytes):
                    chunk = request_bytes[offset:offset + chunk_size]
                    win32file.WriteFile(handle, chunk)
                    offset += len(chunk)
                
                # Flush pipe to ensure all data has been sent
                win32file.FlushFileBuffers(handle)
                
                # Read response (supports chunked reads for large responses)
                response_bytes = b''
                while True:
                    try:
                        _, chunk = win32file.ReadFile(handle, 64 * 1024)
                        if not chunk:
                            break
                        response_bytes += chunk
                        # Try to parse; if successful, the response is complete
                        try:
                            response = json.loads(response_bytes.decode('utf-8'))
                            return response
                        except (json.JSONDecodeError, UnicodeDecodeError):
                            # Incomplete response or mid-character split — continue reading
                            continue
                    except pywintypes.error as e:
                        # Pipe ended (109) or other error
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
        Get the public key (used for encrypting ChromaDB data).

        Returns:
            Public key bytes, or None on failure.
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
        
        logger.error("[storage_client] Failed to get public key: %s", response.get('error'))
        return None
    
    def encrypt_for_chromadb(self, plaintext: str) -> Optional[str]:
        """
        Encrypt data (for ChromaDB plaintext fields).

        Args:
            plaintext: Plaintext to encrypt.

        Returns:
            Encrypted Base64 string, or None on failure.
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
        
        logger.error("[storage_client] Encryption failed: %s", response.get('error'))
        return None
    
    def decrypt_from_chromadb(self, encrypted: str) -> Optional[str]:
        """
        Decrypt data.

        Args:
            encrypted: Encrypted Base64 string.

        Returns:
            Decrypted plaintext, or None on failure.
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
        
        logger.error("[storage_client] Decryption failed: %s", response.get('error'))
        return None

    def decrypt_many_from_chromadb(self, encrypted_list: List[str]) -> List[Optional[str]]:
        """
        Batch-decrypt data.

        Args:
            encrypted_list: List of encrypted strings.

        Returns:
            List of decrypted strings (matching input order; None for failures).
        """
        if not encrypted_list:
            return []

        results: List[Optional[str]] = [None] * len(encrypted_list)
        pending_indices = []
        pending_values = []

        for idx, enc in enumerate(encrypted_list):
            if enc:
                cached = self._cache_get(self._decrypt_cache, enc)
                if cached is not None:
                    results[idx] = cached
                    continue
            pending_indices.append(idx)
            pending_values.append(enc)

        if not pending_values:
            return results

        response = self._send_request({
            'command': 'decrypt_many_from_chromadb',
            'encrypted_list': pending_values
        })

        if response.get('status') == 'success':
            data = response.get('data', {})
            decrypted_list = data.get('decrypted_list')

            if isinstance(decrypted_list, list) and len(decrypted_list) == len(pending_values):
                for i, decrypted in enumerate(decrypted_list):
                    idx = pending_indices[i]
                    results[idx] = decrypted
                    if decrypted is not None and pending_values[i]:
                        self._cache_set(self._decrypt_cache, pending_values[i], decrypted)
                return results

        logger.error("[storage_client] Batch decryption failed: %s", response.get('error'))
        return results
    
    def list_screenshots_for_clustering(
        self,
        start_ts: float = 0.0,
        end_ts: float = 0.0,
        offset: int = 0,
        limit: int = 500,
    ) -> Dict[str, Any]:
        """Fetch screenshots with OCR text from SQLite for clustering backfill.

        Returns {'screenshots': [...], 'total': int}.
        """
        return self._send_request({
            'command': 'list_screenshots_for_clustering',
            'start_ts': start_ts,
            'end_ts': end_ts,
            'offset': offset,
            'limit': limit,
        })

    def is_session_valid(self) -> bool:
        """Check whether the Rust credential session is currently unlocked."""
        response = self._send_request({'command': 'get_auth_status'})
        if response.get('status') == 'success':
            data = response.get('data', {})
            return bool(data.get('session_valid', False))
        return False

    def screenshot_exists(self, image_hash: str) -> bool:
        """
        Check whether a screenshot already exists.

        Args:
            image_hash: Image hash.

        Returns:
            Whether it exists.
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
        Save a screenshot to the Rust storage service.

        Args:
            image_data: Image binary data.
            image_hash: Image hash.
            width: Image width.
            height: Image height.
            window_title: Window title.
            process_name: Process name.
            metadata: Metadata.
            ocr_results: OCR result list.

        Returns:
            Save result.
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

    def save_screenshot_temp(
        self,
        image_data: bytes,
        image_hash: str,
        width: int,
        height: int,
        window_title: Optional[str] = None,
        process_name: Optional[str] = None,
        metadata: Optional[Dict[str, Any]] = None
    ) -> Dict[str, Any]:
        """
        Temporarily save a screenshot (encrypted and marked as pending); returns a
        screenshot_id for later commit/abort.
        """
        import base64

        request = {
            'command': 'save_screenshot_temp',
            'image_data': base64.b64encode(image_data).decode('utf-8'),
            'image_hash': image_hash,
            'width': width,
            'height': height,
            'window_title': window_title,
            'process_name': process_name,
            'metadata': metadata
        }

        response = self._send_request(request)

        if response.get('status') == 'success':
            return response.get('data', {})

        return {
            'status': 'error',
            'error': response.get('error', 'Unknown error')
        }

    def commit_screenshot(self, screenshot_id: str, ocr_results: Optional[List[Dict[str, Any]]]) -> Dict[str, Any]:
        """
        Commit a previously saved temporary screenshot and write OCR results and index.
        """
        request = {
            'command': 'commit_screenshot',
            'screenshot_id': screenshot_id,
            'ocr_results': ocr_results
        }

        response = self._send_request(request)

        if response.get('status') == 'success':
            return response.get('data', {})

        return {
            'status': 'error',
            'error': response.get('error', 'Unknown error')
        }

    def abort_screenshot(self, screenshot_id: str, reason: Optional[str] = None) -> Dict[str, Any]:
        """
        Abort a previously saved temporary screenshot (delete temp files and roll back the record).
        """
        request = {
            'command': 'abort_screenshot',
            'screenshot_id': screenshot_id,
            'reason': reason
        }

        response = self._send_request(request)

        if response.get('status') == 'success':
            return response.get('data', {})

        return {
            'status': 'error',
            'error': response.get('error', 'Unknown error')
        }


# Global storage client instance
_storage_client: Optional[StorageClient] = None


def get_storage_client() -> Optional[StorageClient]:
    """Return the global storage client instance."""
    return _storage_client


def init_storage_client(pipe_name: str) -> StorageClient:
    """
    Initialise the global storage client.

    Args:
        pipe_name: Pipe name.

    Returns:
        Storage client instance.
    """
    global _storage_client
    _storage_client = StorageClient(pipe_name)
    return _storage_client
