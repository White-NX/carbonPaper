import threading
import json
import datetime
import logging
import pywintypes
import os
import time as _time
import struct

logger = logging.getLogger(__name__)
import win32pipe
import win32file
import win32con
import win32api
import win32security
import ntsecuritycon

from typing import Callable


MAX_PIPE_MESSAGE_BYTES = 16 * 1024 * 1024
IPC_PROTOCOL_VERSION = 2


def _is_authorized_client_pid(client_pid: int, expected_pid: int | None, curr_ppid: int) -> bool:
    """Return whether a named-pipe client PID is authorized."""
    if expected_pid and client_pid == expected_pid:
        return True
    if client_pid == curr_ppid:
        return True
    return False


def _read_framed_json_message(handle, chunk_size=1024 * 1024, max_bytes=MAX_PIPE_MESSAGE_BYTES):
    """Read a v2 length-prefixed JSON message."""
    try:
        resp_code, first = win32file.ReadFile(handle, 4)
    except pywintypes.error as e:
        if getattr(e, 'winerror', None) == 109:
            return None
        raise

    if not first:
        return None

    if len(first) == 4:
        frame_len = struct.unpack('<I', first)[0]
        if 0 < frame_len <= max_bytes:
            body = bytearray()
            remaining = frame_len
            while remaining:
                resp_code, chunk = win32file.ReadFile(handle, min(chunk_size, remaining))
                if chunk:
                    body.extend(chunk)
                    remaining -= len(chunk)
                if resp_code not in (0, 234):
                    raise RuntimeError(f"ReadFile returned unexpected status code: {resp_code}")
                if not chunk:
                    break
            if len(body) != frame_len:
                raise RuntimeError(f"Incomplete IPC frame: expected {frame_len}, got {len(body)}")
            return bytes(body).decode('utf-8').strip()
    padded = first.ljust(4, b'\0')
    frame_len = struct.unpack('<I', padded)[0]
    raise ValueError(f"Invalid IPC v{IPC_PROTOCOL_VERSION} frame length: {frame_len}")


def _write_framed_json(handle, payload: bytes):
    if len(payload) > MAX_PIPE_MESSAGE_BYTES:
        raise ValueError(f"Response too large (max {MAX_PIPE_MESSAGE_BYTES} bytes)")
    win32file.WriteFile(handle, struct.pack('<I', len(payload)))
    offset = 0
    while offset < len(payload):
        _, written = win32file.WriteFile(handle, payload[offset:offset + 64 * 1024])
        if not isinstance(written, int) or written <= 0:
            raise RuntimeError("Named pipe write returned no progress")
        offset += written


def _make_security_attributes_for_current_user():
    """Create SECURITY_ATTRIBUTES that only allow the current user to access the pipe."""
    sd = win32security.SECURITY_DESCRIPTOR()
    sd.Initialize()

    # Get the SID of the current process user
    hProc = win32api.GetCurrentProcess()
    hToken = win32security.OpenProcessToken(hProc, win32con.TOKEN_QUERY)
    user_sid = win32security.GetTokenInformation(hToken, win32security.TokenUser)[0]

    # Create a DACL granting only the current user read/write access
    dacl = win32security.ACL()
    mask = ntsecuritycon.FILE_GENERIC_READ | ntsecuritycon.FILE_GENERIC_WRITE
    dacl.AddAccessAllowedAce(win32security.ACL_REVISION, mask, user_sid)

    sd.SetSecurityDescriptorDacl(True, dacl, False)

    sa = pywintypes.SECURITY_ATTRIBUTES()
    sa.SECURITY_DESCRIPTOR = sd
    return sa


def _json_default(obj):
    if isinstance(obj, (datetime.datetime, datetime.date)):
        return obj.isoformat()
    raise TypeError(f"Type {type(obj)} not serializable")


class _NamedPipeServer(threading.Thread):
    """Multi-instance named pipe server for concurrent IPC requests"""
    def __init__(self, handler, pipe_name):
        super().__init__(name="NamedPipeServer", daemon=True)
        self.handler = handler
        self.pipe_name = pipe_name
        self.full_pipe_name = f'\\\\.\\pipe\\{pipe_name}'
        self.stop_event = threading.Event()
        self.security_attrs = _make_security_attributes_for_current_user()

    def run(self):
        logger.info(f"NamedPipeServer starting on {self.full_pipe_name}")
        while not self.stop_event.is_set():
            handle = None
            try:
                # DACL: only current user can connect (OS-level access control)
                # PIPE_REJECT_REMOTE_CLIENTS: block network connections
                handle = win32pipe.CreateNamedPipe(
                    self.full_pipe_name,
                    win32pipe.PIPE_ACCESS_DUPLEX,
                    win32pipe.PIPE_TYPE_MESSAGE | win32pipe.PIPE_READMODE_MESSAGE | win32pipe.PIPE_WAIT | win32pipe.PIPE_REJECT_REMOTE_CLIENTS,
                    win32pipe.PIPE_UNLIMITED_INSTANCES,
                    1024 * 1024, 1024 * 1024, 0,
                    self.security_attrs
                )

                if handle == win32file.INVALID_HANDLE_VALUE:
                    logger.error("Failed to create named pipe instance")
                    handle = None
                    _time.sleep(1)
                    continue

                # Wait for client connection (blocking)
                try:
                    win32pipe.ConnectNamedPipe(handle, None)
                except pywintypes.error as e:
                    # ERROR_PIPE_CONNECTED (535): client connected between Create and Connect — proceed normally
                    if getattr(e, 'winerror', None) != 535:
                        raise

                # Delegate handling to a new thread to keep server listening
                t = threading.Thread(target=self._client_handler, args=(handle,), daemon=True)
                t.start()
                handle = None  # ownership transferred to _client_handler

            except Exception as e:
                if not self.stop_event.is_set():
                    logger.error(f"Error in NamedPipeServer loop: {e}")
                _time.sleep(0.1)
            finally:
                # Close handle if it was not handed off to a client thread
                if handle is not None:
                    try:
                        win32file.CloseHandle(handle)
                    except Exception:
                        pass

    def shutdown(self):
        self.stop_event.set()

    def _client_handler(self, handle):
        """Handle a single client connection"""
        import pywintypes
        handler_started = _time.perf_counter()
        try:
            # 1. Security Verification
            client_pid = win32pipe.GetNamedPipeClientProcessId(handle)
            parent_pid_env = os.environ.get('CARBON_PARENT_PID')
            expected_pid = int(parent_pid_env) if parent_pid_env else None
            curr_ppid = os.getppid()

            is_valid = _is_authorized_client_pid(client_pid, expected_pid, curr_ppid)

            if not is_valid:
                logger.warning(f"[SECURITY] Rejecting PID {client_pid}. (Expected: {expected_pid}, PPID: {curr_ppid})")
                error_resp = json.dumps({"error": f"Access denied: PID {client_pid} is not authorized"}).encode('utf-8')
                _write_framed_json(handle, error_resp)
                win32file.FlushFileBuffers(handle)
                return

            logger.debug(f"IPC client verified: PID {client_pid}")
            # 2. Read Request
            try:
                payload = _read_framed_json_message(handle)
            except ValueError as e:
                logger.error(str(e))
                error_resp = json.dumps({"error": str(e)}).encode('utf-8')
                _write_framed_json(handle, error_resp)
                win32file.FlushFileBuffers(handle)
                return
            except pywintypes.error as e:
                if getattr(e, 'winerror', None) == 109: # ERROR_BROKEN_PIPE
                    return
                raise

            if not payload:
                return

            try:
                req = json.loads(payload)
            except json.JSONDecodeError:
                logger.error(f"Invalid JSON: {payload[:100]}")
                error_resp = json.dumps({"error": "Invalid JSON in request"}).encode('utf-8')
                _write_framed_json(handle, error_resp)
                win32file.FlushFileBuffers(handle)
                return

            command = req.get('command')
            is_process_ocr = command == 'process_ocr'
            if is_process_ocr:
                logger.debug(
                    '[DIAG:PIPE] request received command=%s bytes=%s from_pid=%s',
                    command,
                    len(payload),
                    client_pid,
                )

            # 3. Execute Command
            exec_started = _time.perf_counter()
            result = self.handler(req)
            handler_elapsed = _time.perf_counter() - exec_started
            if handler_elapsed >= 3.0:
                logger.warning(
                    '[DIAG:PIPE] slow handler command=%s exec=%.3fs',
                    command,
                    handler_elapsed,
                )
            elif is_process_ocr:
                logger.debug(
                    '[DIAG:PIPE] handler finished command=%s in %.3fs',
                    command,
                    handler_elapsed,
                )

            # 4. Write Response
            def json_serial(obj):
                if isinstance(obj, (datetime.datetime, datetime.date)):
                    return obj.isoformat()
                raise TypeError(f"Type {type(obj)} not serializable")

            write_started = _time.perf_counter()
            resp_str = json.dumps(result, default=json_serial)
            try:
                _write_framed_json(handle, resp_str.encode('utf-8'))
                win32file.FlushFileBuffers(handle)
            except pywintypes.error as e:
                if getattr(e, 'winerror', None) in (109, 232):
                    logger.debug(
                        "[DIAG:PIPE] client closed before response command=%s winerror=%s",
                        command,
                        e.winerror,
                    )
                    return
                raise
            if is_process_ocr:
                logger.debug(
                    '[DIAG:PIPE] response sent command=%s write=%.3fs total=%.3fs resp_bytes=%s',
                    command,
                    _time.perf_counter() - write_started,
                    _time.perf_counter() - handler_started,
                    len(resp_str),
                )

        except Exception as e:
            logger.error(f"Error handling IPC client: {e}", exc_info=True)
        finally:
            try:
                # Do NOT call DisconnectNamedPipe here. 
                # It invalidates the client's handle before they can finish reading the response.
                # Simply closing the handle sends a proper EOF.
                win32file.CloseHandle(handle)
            except:
                pass


def start_pipe_server(handler, pipe_name):
    server = _NamedPipeServer(handler, pipe_name)
    server.start()
    return server


def send_ipc_request(pipe_name, req):
    """Client utility"""
    full_pipe_name = f'\\\\.\\pipe\\{pipe_name}'
    try:
        handle = win32file.CreateFile(
            full_pipe_name,
            win32file.GENERIC_READ | win32file.GENERIC_WRITE,
            0, None,
            win32file.OPEN_EXISTING,
            0, None
        )
        win32pipe.SetNamedPipeHandleState(handle, win32pipe.PIPE_READMODE_MESSAGE, None, None)
        framed_req = dict(req)
        framed_req['ipc_protocol_version'] = IPC_PROTOCOL_VERSION
        _write_framed_json(handle, json.dumps(framed_req).encode('utf-8'))
        payload = _read_framed_json_message(handle)
        return json.loads(payload) if payload else {}
    finally:
        try:
            win32file.CloseHandle(handle)
        except:
            pass
