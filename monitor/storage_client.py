"""
Storage client module — communicates with the Rust storage service via IPC.
"""
import json
import time
import logging
import threading
import struct
import os
import random
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


def _default_reverse_ipc_timeout_secs() -> float:
    raw = os.environ.get("CARBONPAPER_REVERSE_IPC_TIMEOUT_SECS", "15") or "15"
    try:
        return max(1.0, float(raw))
    except (TypeError, ValueError):
        logger.warning("Invalid CARBONPAPER_REVERSE_IPC_TIMEOUT_SECS=%r; using 15s", raw)
        return 15.0


def _env_float(name: str, default: float, minimum: float) -> float:
    raw = os.environ.get(name, str(default)) or str(default)
    try:
        return max(minimum, float(raw))
    except (TypeError, ValueError):
        logger.warning("Invalid %s=%r; using %s", name, raw, default)
        return float(default)


def _env_int(name: str, default: int, minimum: int) -> int:
    raw = os.environ.get(name, str(default)) or str(default)
    try:
        return max(minimum, int(raw))
    except (TypeError, ValueError):
        logger.warning("Invalid %s=%r; using %s", name, raw, default)
        return int(default)


DEFAULT_REVERSE_IPC_TIMEOUT_SECS = _default_reverse_ipc_timeout_secs()
DEFAULT_REVERSE_IPC_CIRCUIT_FAILURE_THRESHOLD = _env_int(
    "CARBONPAPER_REVERSE_IPC_CIRCUIT_FAILURE_THRESHOLD",
    3,
    1,
)
DEFAULT_REVERSE_IPC_CIRCUIT_COOLDOWN_SECS = _env_float(
    "CARBONPAPER_REVERSE_IPC_CIRCUIT_COOLDOWN_SECS",
    15.0,
    1.0,
)
RETRY_BACKOFF_BASE_SECS = _env_float(
    "CARBONPAPER_REVERSE_IPC_RETRY_BACKOFF_BASE_SECS",
    0.05,
    0.01,
)

READ_RETRY_COMMANDS = {
    'get_public_key',
    'encrypt_for_chromadb',
    'decrypt_from_chromadb',
    'decrypt_many_from_chromadb',
    'decrypt_from_chromadb_silent',
    'decrypt_many_from_chromadb_silent',
    'list_screenshots_for_clustering',
    'get_idle_state',
    'get_auth_status',
    'bge_embed_texts',
}

IDEMPOTENT_RETRY_COMMANDS = {
    'update_screenshot_category',
    'set_ocr_postprocess_status',
    'record_ocr_postprocess_retry',
}

SAFE_RETRY_AFTER_SEND_COMMANDS = READ_RETRY_COMMANDS | IDEMPOTENT_RETRY_COMMANDS


class ReverseIpcTimeoutError(TimeoutError):
    """Raised when a reverse IPC storage request exceeds its deadline."""

    def __init__(self, command: str, timeout_secs: float, phase: str = "request"):
        self.command = command or "<unknown>"
        self.timeout_secs = float(timeout_secs)
        self.phase = phase
        super().__init__(
            f"Reverse IPC {phase} timed out after {self.timeout_secs:.1f}s "
            f"(command={self.command})"
        )


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
        self._last_timeout_at: Optional[float] = None
        self._last_timeout_command: Optional[str] = None
        self._circuit_lock = threading.Lock()
        self._circuit_failure_threshold = DEFAULT_REVERSE_IPC_CIRCUIT_FAILURE_THRESHOLD
        self._circuit_cooldown_secs = DEFAULT_REVERSE_IPC_CIRCUIT_COOLDOWN_SECS
        self._circuit_failure_count = 0
        self._circuit_open_until = 0.0
        self._circuit_opened_at: Optional[float] = None
        self._circuit_half_open_probe = False
        self._circuit_last_failure_at: Optional[float] = None
        self._circuit_last_error: Optional[str] = None
        self._circuit_last_command: Optional[str] = None

    def _timeout_response(self, exc: ReverseIpcTimeoutError) -> Dict[str, Any]:
        self._last_timeout_at = time.time()
        self._last_timeout_command = exc.command
        return {
            'status': 'error',
            'code': 'ipc_timeout',
            'command': exc.command,
            'phase': exc.phase,
            'timeout_secs': exc.timeout_secs,
            'error': str(exc),
        }

    def _build_request_bytes(self, request: Dict[str, Any]) -> bytes:
        framed_request = dict(request)
        framed_request['_ipc_keepalive'] = True
        framed_request['_auth_token'] = self._auth_token
        self._seq_no += 1
        framed_request['_seq_no'] = self._seq_no
        return json.dumps(framed_request).encode('utf-8')

    def _can_retry_after_send(self, command: str) -> bool:
        return command in SAFE_RETRY_AFTER_SEND_COMMANDS

    def _sleep_before_retry(self, attempt: int, command: str) -> None:
        delay = RETRY_BACKOFF_BASE_SECS * (2 ** max(0, attempt))
        delay += random.uniform(0.0, delay * 0.2)
        logger.debug(
            "[storage_client] reverse IPC retry backoff command=%s attempt=%s delay=%.3fs",
            command,
            attempt + 1,
            delay,
        )
        time.sleep(delay)

    def _circuit_state_locked(self, now: Optional[float] = None) -> str:
        now = time.monotonic() if now is None else now
        if self._circuit_open_until > now:
            return "open"
        if self._circuit_half_open_probe or self._circuit_failure_count >= self._circuit_failure_threshold:
            return "half_open"
        return "closed"

    def _circuit_block_response(self, command: str, state: str, retry_after_secs: float) -> Dict[str, Any]:
        return {
            'status': 'error',
            'code': 'ipc_circuit_open',
            'command': command,
            'circuit_state': state,
            'retry_after_secs': max(0.0, retry_after_secs),
            'failure_count': self._circuit_failure_count,
            'failure_threshold': self._circuit_failure_threshold,
            'error': (
                "Reverse IPC circuit breaker is open after repeated transport failures; "
                "skipping reconnect attempt"
            ),
        }

    def _check_circuit_before_request(self, command: str) -> Optional[Dict[str, Any]]:
        with self._circuit_lock:
            now = time.monotonic()
            if self._circuit_open_until > now:
                return self._circuit_block_response(
                    command,
                    "open",
                    self._circuit_open_until - now,
                )
            if self._circuit_failure_count >= self._circuit_failure_threshold:
                if self._circuit_half_open_probe:
                    return self._circuit_block_response(command, "half_open", 0.0)
                self._circuit_half_open_probe = True
                return None
            return None

    def _record_ipc_success(self) -> None:
        with self._circuit_lock:
            self._circuit_failure_count = 0
            self._circuit_open_until = 0.0
            self._circuit_opened_at = None
            self._circuit_half_open_probe = False
            self._circuit_last_error = None
            self._circuit_last_command = None

    def _record_ipc_failure(self, command: str, error: str) -> None:
        with self._circuit_lock:
            now = time.monotonic()
            self._circuit_failure_count += 1
            self._circuit_half_open_probe = False
            self._circuit_last_failure_at = time.time()
            self._circuit_last_error = error
            self._circuit_last_command = command
            if self._circuit_failure_count >= self._circuit_failure_threshold:
                self._circuit_open_until = now + self._circuit_cooldown_secs
                self._circuit_opened_at = time.time()
                logger.error(
                    "[storage_client] reverse IPC circuit opened command=%s failures=%s cooldown=%.1fs last_error=%s",
                    command,
                    self._circuit_failure_count,
                    self._circuit_cooldown_secs,
                    error,
                )

    def ipc_health_snapshot(self) -> Dict[str, Any]:
        with self._circuit_lock:
            now = time.monotonic()
            retry_after = max(0.0, self._circuit_open_until - now)
            return {
                'circuit_state': self._circuit_state_locked(now),
                'failure_count': self._circuit_failure_count,
                'failure_threshold': self._circuit_failure_threshold,
                'cooldown_secs': self._circuit_cooldown_secs,
                'retry_after_secs': retry_after,
                'opened_at': self._circuit_opened_at,
                'last_failure_at': self._circuit_last_failure_at,
                'last_error': self._circuit_last_error,
                'last_command': self._circuit_last_command,
                'last_timeout_at': self._last_timeout_at,
                'last_timeout_command': self._last_timeout_command,
            }

    def _remaining_deadline(self, deadline: float, command: str, timeout_secs: float, phase: str) -> float:
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise ReverseIpcTimeoutError(command, timeout_secs, phase=phase)
        return remaining

    def _trip_request_watchdog(
        self,
        timeout_event: threading.Event,
        command: str,
        timeout_secs: float,
    ) -> None:
        timeout_event.set()
        logger.error(
            "[storage_client] reverse IPC watchdog fired command=%s timeout=%.1fs; closing persistent pipe",
            command,
            timeout_secs,
        )
        self._close_persistent_handle()

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
    
    def _send_request(self, request: Dict[str, Any], timeout: Optional[float] = None) -> Dict[str, Any]:
        """
        Send a request to the Rust storage service.

        Args:
            request: Request payload.

        Returns:
            Response data.
        """
        command = str(request.get('command') or '<unknown>')
        timeout_secs = max(
            0.1,
            float(timeout if timeout is not None else DEFAULT_REVERSE_IPC_TIMEOUT_SECS),
        )
        deadline = time.monotonic() + timeout_secs
        semaphore_acquired = False
        lock_acquired = False
        watchdog_timer = None
        timeout_event = threading.Event()

        try:
            circuit_response = self._check_circuit_before_request(command)
            if circuit_response is not None:
                return circuit_response

            remaining = self._remaining_deadline(deadline, command, timeout_secs, "semaphore")
            semaphore_acquired = self._semaphore.acquire(timeout=remaining)
            if not semaphore_acquired:
                raise ReverseIpcTimeoutError(command, timeout_secs, phase="semaphore")

            remaining = self._remaining_deadline(deadline, command, timeout_secs, "request_lock")
            lock_acquired = self._request_lock.acquire(timeout=remaining)
            if not lock_acquired:
                raise ReverseIpcTimeoutError(command, timeout_secs, phase="request_lock")

            remaining = self._remaining_deadline(deadline, command, timeout_secs, "request")
            watchdog_timer = threading.Timer(
                remaining,
                self._trip_request_watchdog,
                args=(timeout_event, command, timeout_secs),
            )
            watchdog_timer.daemon = True
            watchdog_timer.start()

            for attempt in range(2):
                self._remaining_deadline(deadline, command, timeout_secs, "connect")
                handle = self._connect_persistent_handle()
                request_bytes = self._build_request_bytes(request)
                try:
                    self._remaining_deadline(deadline, command, timeout_secs, "write")
                    _write_framed_json(handle, request_bytes)

                    # Flush pipe to ensure all data has been sent.
                    try:
                        self._remaining_deadline(deadline, command, timeout_secs, "flush")
                        win32file.FlushFileBuffers(handle)
                    except pywintypes.error as e:
                        if e.winerror not in PIPE_CLOSED_WINERRORS:
                            raise
                        logger.debug(
                            "[storage_client] FlushFileBuffers returned %s; continue reading response",
                            e.winerror,
                        )

                    self._remaining_deadline(deadline, command, timeout_secs, "read")
                    response = _read_framed_json(handle)
                    if timeout_event.is_set():
                        raise ReverseIpcTimeoutError(command, timeout_secs, phase="watchdog")
                except pywintypes.error as e:
                    if timeout_event.is_set():
                        raise ReverseIpcTimeoutError(command, timeout_secs, phase="watchdog")
                    can_retry = (
                        e.winerror in PIPE_CLOSED_WINERRORS
                        and attempt < 1
                        and self._can_retry_after_send(command)
                    )
                    if not can_retry:
                        raise
                    logger.warning(
                        "[storage_client] persistent pipe closed during request command=%s winerror=%s; reconnecting and retrying once",
                        request.get('command'),
                        e.winerror,
                    )
                    self._close_persistent_handle()
                    self._sleep_before_retry(attempt, command)
                    continue

                if response == {'status': 'error', 'error': 'Empty response'}:
                    self._close_persistent_handle()
                    if attempt < 1 and self._can_retry_after_send(command):
                        logger.warning(
                            "[storage_client] persistent pipe returned empty response command=%s; reconnecting and retrying once",
                            request.get('command'),
                        )
                        self._sleep_before_retry(attempt, command)
                        continue
                    self._record_ipc_failure(command, "Empty response")
                    return response
                self._record_ipc_success()
                return response

            raise RuntimeError("Failed to send request to pipe")
                
        except ReverseIpcTimeoutError as e:
            self._close_persistent_handle()
            if e.phase not in ("semaphore", "request_lock"):
                self._record_ipc_failure(command, str(e))
            return self._timeout_response(e)
        except pywintypes.error as e:
            self._close_persistent_handle()
            self._record_ipc_failure(command, f"IPC error: {e}")
            return {'status': 'error', 'error': f'IPC error: {e}'}
        except Exception as e:
            self._close_persistent_handle()
            self._record_ipc_failure(command, f"Error: {e}")
            return {'status': 'error', 'error': f'Error: {e}'}
        finally:
            if watchdog_timer is not None:
                watchdog_timer.cancel()
            if lock_acquired:
                try:
                    self._request_lock.release()
                except RuntimeError:
                    pass
            if semaphore_acquired:
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

    def decrypt_from_chromadb_silent(self, encrypted: str) -> Optional[str]:
        """Decrypt one field through the non-interactive background path."""
        if not encrypted:
            return encrypted
        response = self._send_request({
            'command': 'decrypt_from_chromadb_silent',
            'encrypted': encrypted,
        })
        if response.get('status') == 'success':
            return response.get('data', {}).get('decrypted')
        if response.get('error') in ('AUTH_REQUIRED', 'AUTH_REQUIRED: background lease unavailable'):
            raise PermissionError('AUTH_REQUIRED')
        logger.error("[storage_client] Silent decryption failed: %s", response.get('error'))
        return None

    def decrypt_many_from_chromadb_silent(self, encrypted_list: List[str]) -> List[Optional[str]]:
        """Batch decrypt fields without permitting an authentication prompt."""
        if not encrypted_list:
            return []
        response = self._send_request({
            'command': 'decrypt_many_from_chromadb_silent',
            'encrypted_list': encrypted_list,
        })
        if response.get('status') == 'success':
            values = response.get('data', {}).get('decrypted_list')
            if isinstance(values, list) and len(values) == len(encrypted_list):
                return values
        if response.get('error') in ('AUTH_REQUIRED', 'AUTH_REQUIRED: background lease unavailable'):
            raise PermissionError('AUTH_REQUIRED')
        logger.error("[storage_client] Silent batch decryption failed: %s", response.get('error'))
        return [None] * len(encrypted_list)

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

    def embed_bge_texts(self, texts: List[str]) -> Dict[str, Any]:
        """Run BGE text embedding in the shared Rust semantic worker."""
        response = self._send_request(
            {
                'command': 'bge_embed_texts',
                'texts': [str(text) for text in texts],
            },
            timeout=150,
        )
        if response.get('status') == 'success':
            data = response.get('data')
            if isinstance(data, dict):
                return data
        raise RuntimeError(response.get('error', 'Rust BGE inference failed'))

    def get_idle_state(self) -> Dict[str, Any]:
        """Read the current system idle state from Rust.

        Returns {'is_idle': bool, 'idle_secs': int, 'fullscreen_exclusive': bool}.
        Default to "not idle" on any error to fail safe.
        """
        response = self._send_request({'command': 'get_idle_state'})
        if response.get('status') == 'success':
            data = response.get('data')
            if isinstance(data, dict):
                return data
        return {'is_idle': False, 'idle_secs': 0, 'fullscreen_exclusive': True}

    def is_session_valid(self) -> bool:
        """Check whether the Rust credential session is currently unlocked."""
        response = self._send_request({'command': 'get_auth_status'})
        if response.get('status') == 'success':
            data = response.get('data', {})
            return bool(data.get('session_valid', False))
        return False

    def is_background_authorized(self) -> bool:
        """Whether Rust granted this process the unattended processing lease."""
        response = self._send_request({'command': 'get_auth_status'})
        if response.get('status') == 'success':
            return bool(response.get('data', {}).get('background_authorized', False))
        return False

    def set_ocr_postprocess_status(
        self,
        screenshot_id: int,
        status: str,
        error: Optional[str] = None,
    ) -> bool:
        response = self._send_request({
            'command': 'set_ocr_postprocess_status',
            'screenshot_id': int(screenshot_id),
            'status': status,
            'error': error,
        })
        return response.get('status') == 'success'

    def record_ocr_postprocess_retry(self, screenshot_id: int, error: str) -> bool:
        response = self._send_request({
            'command': 'record_ocr_postprocess_retry',
            'screenshot_id': int(screenshot_id),
            'error': str(error or 'OCR postprocess failed'),
        })
        return response.get('status') == 'success'

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
