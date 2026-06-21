"""
Storage client module — communicates with the Rust storage service via IPC.
"""
import json
import time
import logging
import threading
import struct
import os
from collections import OrderedDict

logger = logging.getLogger(__name__)
import win32file
import win32pipe
import pywintypes
from typing import Optional, Dict, Any, List


IPC_PROTOCOL_VERSION = 2
MAX_PIPE_MESSAGE_BYTES = 16 * 1024 * 1024
MAX_PIPE_BINARY_BYTES = 64 * 1024 * 1024
PIPE_CLOSED_WINERRORS = (109, 232)


def _write_framed_json(handle, payload: bytes) -> None:
    if len(payload) > MAX_PIPE_MESSAGE_BYTES:
        raise ValueError(f"Request too large (max {MAX_PIPE_MESSAGE_BYTES} bytes)")
    win32file.WriteFile(handle, struct.pack('<I', len(payload)))
    offset = 0
    chunk_size = 64 * 1024
    while offset < len(payload):
        chunk = payload[offset:offset + chunk_size]
        _, written = win32file.WriteFile(handle, chunk)
        if not isinstance(written, int) or written <= 0:
            raise RuntimeError("Named pipe write returned no progress")
        offset += written


def _read_exact_frame(handle, max_bytes: int) -> bytes:
    _, prefix = win32file.ReadFile(handle, 4)
    if len(prefix) != 4:
        raise RuntimeError(f"Incomplete frame prefix: {len(prefix)} bytes")
    frame_len = struct.unpack('<I', prefix)[0]
    if frame_len > max_bytes:
        raise RuntimeError(f"Frame too large: {frame_len} bytes (max {max_bytes})")
    chunks = []
    remaining = frame_len
    while remaining:
        _, chunk = win32file.ReadFile(handle, min(64 * 1024, remaining))
        if not chunk:
            break
        chunks.append(chunk)
        remaining -= len(chunk)
    body = b''.join(chunks)
    if len(body) != frame_len:
        raise RuntimeError(f"Incomplete frame: expected {frame_len}, got {len(body)}")
    return body


def _read_framed_json(handle) -> Dict[str, Any]:
    try:
        _, first = win32file.ReadFile(handle, 4)
    except pywintypes.error as e:
        if e.winerror == 109:
            return {'status': 'error', 'error': 'Empty response'}
        raise
    if not first:
        return {'status': 'error', 'error': 'Empty response'}

    if len(first) == 4:
        frame_len = struct.unpack('<I', first)[0]
        if 0 < frame_len <= MAX_PIPE_MESSAGE_BYTES:
            chunks = []
            remaining = frame_len
            while remaining:
                _, chunk = win32file.ReadFile(handle, min(64 * 1024, remaining))
                if not chunk:
                    break
                chunks.append(chunk)
                remaining -= len(chunk)
            response_bytes = b''.join(chunks)
            if len(response_bytes) != frame_len:
                return {'status': 'error', 'error': f'Incomplete response frame: expected {frame_len}, got {len(response_bytes)}'}
            response = json.loads(response_bytes.decode('utf-8'))
            if response.get('status') == 'success' and response.get('data', {}).get('binary_frame'):
                try:
                    response['_binary_body'] = _read_exact_frame(handle, MAX_PIPE_BINARY_BYTES)
                except Exception as e:
                    return {'status': 'error', 'error': f'Binary response read failed: {e}'}
            return response

    return {
        'status': 'error',
        'error': f'Invalid IPC v{IPC_PROTOCOL_VERSION} frame length: {struct.unpack("<I", first)[0]}',
    }


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
        self._request_lock = threading.RLock()
        self._persistent_handle = None
        self._decrypt_cache = OrderedDict()
        self._encrypt_cache = OrderedDict()
        self._cache_limit = 512
        self._auth_token = os.environ.get("CARBONPAPER_REVERSE_IPC_TOKEN", "")
        self._seq_no = 0

    def _close_persistent_handle(self) -> None:
        handle = self._persistent_handle
        self._persistent_handle = None
        if handle is not None:
            try:
                win32file.CloseHandle(handle)
            except Exception:
                pass

    def _connect_persistent_handle(self):
        if self._persistent_handle is not None:
            return self._persistent_handle

        handle = None
        last_error = None
        for attempt in range(6):
            try:
                handle = win32file.CreateFile(
                    self.full_pipe_name,
                    win32file.GENERIC_READ | win32file.GENERIC_WRITE,
                    0,
                    None,
                    win32file.OPEN_EXISTING,
                    0,
                    None,
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
            raise RuntimeError("Failed to connect to pipe")

        win32pipe.SetNamedPipeHandleState(
            handle,
            win32pipe.PIPE_READMODE_BYTE,
            None,
            None,
        )
        self._persistent_handle = handle
        logger.debug("[storage_client] persistent IPC connection established: %s", self.full_pipe_name)
        return handle

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
            with self._request_lock:
                framed_request = dict(request)
                framed_request['_ipc_keepalive'] = True
                framed_request['_auth_token'] = self._auth_token
                self._seq_no += 1
                framed_request['_seq_no'] = self._seq_no
                request_bytes = json.dumps(framed_request).encode('utf-8')

                for attempt in range(2):
                    handle = self._connect_persistent_handle()
                    try:
                        _write_framed_json(handle, request_bytes)

                        # Flush pipe to ensure all data has been sent.
                        try:
                            win32file.FlushFileBuffers(handle)
                        except pywintypes.error as e:
                            if e.winerror not in PIPE_CLOSED_WINERRORS:
                                raise
                            logger.debug(
                                "[storage_client] FlushFileBuffers returned %s; continue reading response",
                                e.winerror,
                            )
                        response = _read_framed_json(handle)
                    except pywintypes.error as e:
                        if e.winerror not in PIPE_CLOSED_WINERRORS or attempt >= 1:
                            raise
                        logger.warning(
                            "[storage_client] persistent pipe closed during request command=%s winerror=%s; reconnecting and retrying once",
                            request.get('command'),
                            e.winerror,
                        )
                        self._close_persistent_handle()
                        continue

                    if response == {'status': 'error', 'error': 'Empty response'}:
                        self._close_persistent_handle()
                        if attempt < 1:
                            logger.warning(
                                "[storage_client] persistent pipe returned empty response command=%s; reconnecting and retrying once",
                                request.get('command'),
                            )
                            continue
                    return response

                raise RuntimeError("Failed to send request to pipe")
                
        except pywintypes.error as e:
            self._close_persistent_handle()
            return {'status': 'error', 'error': f'IPC error: {e}'}
        except Exception as e:
            self._close_persistent_handle()
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

        error_message = str(response.get('error', ''))
        if response.get('status') != 'success' and 'Invalid JSON' in error_message:
            logger.warning("[storage_client] Batch decryption got Invalid JSON, retrying once")
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

    def get_screenshots_with_ocr_by_ids(self, ids: List[int]) -> Dict[str, Any]:
        """Fetch screenshots + OCR text for a specific set of IDs (single round-trip).

        Used by the NL-cluster reranker path to enrich candidate text before
        cross-encoder scoring. Returns {'screenshots': [...]} (no 'total').
        """
        if not ids:
            return {'screenshots': []}
        response = self._send_request({
            'command': 'get_screenshots_with_ocr_by_ids',
            'ids': [int(i) for i in ids],
        })
        if response.get('status') == 'success':
            return response.get('data', {'screenshots': []})
        raise RuntimeError(response.get('error', 'Unknown error during IPC get_screenshots_with_ocr_by_ids'))

    def update_screenshot_category(
        self,
        screenshot_id: int,
        category: str,
        category_confidence: Optional[float] = None,
    ) -> bool:
        """Update a screenshot category after asynchronous classification."""
        request = {
            'command': 'update_screenshot_category',
            'screenshot_id': int(screenshot_id),
            'category': category,
        }
        if category_confidence is not None:
            request['category_confidence'] = float(category_confidence)
        response = self._send_request(request)
        return response.get('status') == 'success'

    # ---- Smart Cluster reverse IPC --------------------------------------

    def get_idle_state(self) -> Dict[str, Any]:
        """Read the current system idle state from Rust.

        Returns {'is_idle': bool, 'idle_secs': int, 'fullscreen_exclusive': bool}.
        Default to "not idle" on any error to fail safe.
        """
        response = self._send_request({'command': 'get_idle_state'})
        if response.get('status') == 'success':
            return response.get('data', {'is_idle': False, 'idle_secs': 0, 'fullscreen_exclusive': True})
        return {'is_idle': False, 'idle_secs': 0, 'fullscreen_exclusive': True}

    def smart_cluster_list_enabled(self) -> List[Dict[str, Any]]:
        """Return enabled smart clusters with anchor text and threshold."""
        response = self._send_request({'command': 'smart_cluster_list_enabled'})
        if response.get('status') == 'success':
            return response.get('data', {}).get('clusters', [])
        return []

    def smart_cluster_enqueue_pending(self, screenshot_id: int) -> bool:
        response = self._send_request({
            'command': 'smart_cluster_enqueue_pending',
            'screenshot_id': int(screenshot_id),
        })
        return response.get('status') == 'success'

    def smart_cluster_peek_pending(self, limit: int = 32) -> List[int]:
        """Read up to ``limit`` pending screenshot ids WITHOUT removing them.

        Rust applies a 30-day TTL filter and opportunistically prunes
        expired rows in the same transaction. The caller must invoke
        :meth:`smart_cluster_delete_pending` for ids it has fully processed;
        on any failure path the ids stay in the queue and are retried on
        the next idle window.
        """
        response = self._send_request({
            'command': 'smart_cluster_peek_pending',
            'limit': int(limit),
        })
        if response.get('status') == 'success':
            return response.get('data', {}).get('ids', [])
        return []

    def smart_cluster_delete_pending(self, ids: List[int]) -> bool:
        """Remove pending ids after they have been scored and assignments persisted."""
        if not ids:
            return True
        response = self._send_request({
            'command': 'smart_cluster_delete_pending',
            'ids': [int(i) for i in ids],
        })
        return response.get('status') == 'success'

    def smart_cluster_count_pending(self) -> int:
        response = self._send_request({'command': 'smart_cluster_count_pending'})
        if response.get('status') == 'success':
            return int(response.get('data', {}).get('count', 0))
        return 0

    def smart_cluster_record_assignment(
        self,
        smart_cluster_id: int,
        screenshot_id: int,
        rerank_score: float,
    ) -> bool:
        response = self._send_request({
            'command': 'smart_cluster_record_assignment',
            'smart_cluster_id': int(smart_cluster_id),
            'screenshot_id': int(screenshot_id),
            'rerank_score': float(rerank_score),
        })
        return response.get('status') == 'success'

    def is_session_valid(self) -> bool:
        """Check whether the Rust credential session is currently unlocked."""
        response = self._send_request({'command': 'get_auth_status'})
        if response.get('status') == 'success':
            data = response.get('data', {})
            return bool(data.get('session_valid', False))
        return False

    def get_temp_image_bytes(self, screenshot_id: int) -> Dict[str, Any]:
        """Fetch temporary OCR image bytes using v2 binary response framing."""
        response = self._send_request({
            'command': 'get_temp_image',
            'screenshot_id': int(screenshot_id),
        })
        if response.get('status') != 'success':
            return response

        data = response.get('data', {})
        image_bytes = response.get('_binary_body')
        if image_bytes is None:
            return {'status': 'error', 'error': 'Binary image response missing body frame'}

        return {
            'status': 'success',
            'data': {
                'image_bytes': image_bytes,
                'mime_type': data.get('mime_type', 'image/jpeg'),
            },
        }

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
