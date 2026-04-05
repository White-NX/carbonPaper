import threading
import json
import datetime
import logging
import pywintypes
import os
import time as _time

logger = logging.getLogger(__name__)
import win32pipe
import win32file
import win32con
import win32api
import win32security
import ntsecuritycon

from typing import Callable


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

            is_valid = False
            # Only allow the explicit expected PID or the direct parent process
            if expected_pid and client_pid == expected_pid:
                is_valid = True
            elif client_pid == curr_ppid:
                is_valid = True

            if not is_valid:
                logger.warning(f"[SECURITY] Rejecting PID {client_pid}. (Expected: {expected_pid}, PPID: {curr_ppid})")
                error_resp = json.dumps({"error": f"Access denied: PID {client_pid} is not authorized"}).encode('utf-8')
                win32file.WriteFile(handle, error_resp)
                win32file.FlushFileBuffers(handle)
                return

            logger.debug(f"IPC client verified: PID {client_pid}")
            # 2. Read Request
            try:
                # In message mode, one ReadFile typically gets the whole message.
                # We use a 1MB buffer which is plenty for our JSON commands.
                resp_code, data = win32file.ReadFile(handle, 1024 * 1024)
            except pywintypes.error as e:
                if getattr(e, 'winerror', None) == 109: # ERROR_BROKEN_PIPE
                    return
                raise

            if not data:
                return

            payload = data.decode('utf-8').strip()
            try:
                req = json.loads(payload)
            except json.JSONDecodeError:
                logger.error(f"Invalid JSON: {payload[:100]}")
                error_resp = json.dumps({"error": "Invalid JSON in request"}).encode('utf-8')
                win32file.WriteFile(handle, error_resp)
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
            win32file.WriteFile(handle, resp_str.encode('utf-8'))
            win32file.FlushFileBuffers(handle)
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
        win32file.WriteFile(handle, json.dumps(req).encode('utf-8'))
        resp_code, data = win32file.ReadFile(handle, 1024 * 1024)
        return json.loads(data.decode('utf-8').strip()) if data else {}
    finally:
        try:
            win32file.CloseHandle(handle)
        except:
            pass
