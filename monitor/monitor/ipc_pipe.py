import threading
import json
import datetime
import pywintypes
import win32pipe
import win32file
import win32con
import win32api
import win32security
import ntsecuritycon

from typing import Callable


def _make_security_attributes_for_current_user():
    # 创建仅允许当前用户访问的安全属性（SECURITY_ATTRIBUTES）
    sd = win32security.SECURITY_DESCRIPTOR()
    sd.Initialize()

    # 获取当前进程用户的 SID
    hProc = win32api.GetCurrentProcess()
    hToken = win32security.OpenProcessToken(hProc, win32con.TOKEN_QUERY)
    user_sid = win32security.GetTokenInformation(hToken, win32security.TokenUser)[0]

    # 创建 DACL，只给当前用户读写权限
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


def start_pipe_server(handler: Callable[[dict], dict], pipe_name: str = 'carbon_monitor_secure'):
    """
    Start a simple named-pipe server. For each connection, read a JSON object and write back a JSON response.
    The pipe is created with a security descriptor that only allows the current user to connect.
    """
    full_pipe_name = r"\\.\pipe\%s" % pipe_name
    sa = _make_security_attributes_for_current_user()

    server = _NamedPipeServer(full_pipe_name, handler, sa)
    server.start()
    return server


def start_inherited_handle_server(handler: Callable[[dict], dict], handle_value: int):
    """Start a server using an already-created inheritable pipe HANDLE passed from parent.

    `handle_value` should be the numeric Win32 HANDLE value (int). This function wraps
    that handle and processes a single client connection on it. The parent is expected
    to have created the pipe and set it inheritable for the child process.
    """
    server = _InheritedPipeServer(handle_value, handler)
    server.start()
    return server


class _InheritedPipeServer:
    def __init__(self, handle_value, handler):
        self.handle = int(handle_value)
        self.handler = handler
        self._thread = threading.Thread(target=self._serve_once, daemon=True)
        self._stop = threading.Event()

    def start(self):
        self._thread.start()

    def shutdown(self):
        self._stop.set()

    def _serve_once(self):
        # The parent should have created the pipe and (optionally) already connected a client.
        # We try to call ConnectNamedPipe; if it fails with ERROR_PIPE_CONNECTED we proceed.
        try:
            try:
                win32pipe.ConnectNamedPipe(self.handle, None)
            except pywintypes.error as e:
                # ERROR_PIPE_CONNECTED = 535 indicates already connected
                if getattr(e, 'winerror', None) not in (535,):
                    raise

            # Read request
            try:
                res, data = win32file.ReadFile(self.handle, 65536)
                text = data.decode('utf-8').strip()
                if text:
                    req = json.loads(text)
                else:
                    req = {}
            except Exception:
                req = {}

            try:
                resp = self.handler(req) or {}
            except Exception as e:
                resp = {'error': str(e)}

            out = json.dumps(resp, default=_json_default).encode('utf-8')
            try:
                win32file.WriteFile(self.handle, out)
            except Exception:
                pass

        except Exception as e:
            print('Inherited pipe serve error:', e)
        finally:
            try:
                win32file.CloseHandle(self.handle)
            except Exception:
                try:
                    win32pipe.DisconnectNamedPipe(self.handle)
                except Exception:
                    pass


class _NamedPipeServer:
    def __init__(self, pipe_name, handler, security_attrs):
        self.pipe_name = pipe_name
        self.handler = handler
        self.security_attrs = security_attrs
        self._thread = threading.Thread(target=self._serve_loop, daemon=True)
        self._stop = threading.Event()

    def start(self):
        self._thread.start()

    def shutdown(self):
        self._stop.set()
        # TODO: Might need to connect to pipe to unblock ConnectNamedPipe?

    def _client_handler(self, handle):
        """Handle a single client connection in a separate thread"""
        try:
            # 读取请求（假设一次性发送完整 JSON）
            try:
                res, data = win32file.ReadFile(handle, 65536)
                text = data.decode('utf-8').strip()
                if text:
                    req = json.loads(text)
                else:
                    req = {}
            except Exception as e:
                req = {}

            try:
                resp = self.handler(req) or {}
            except Exception as e:
                resp = {'error': str(e)}

            out = json.dumps(resp, default=_json_default).encode('utf-8')
            try:
                win32file.WriteFile(handle, out)
            except Exception:
                pass

        finally:
            try:
                win32file.CloseHandle(handle)
            except Exception:
                try:
                    win32pipe.DisconnectNamedPipe(handle)
                except Exception:
                    pass

    def _serve_loop(self):
        while not self._stop.is_set():
            try:
                handle = win32pipe.CreateNamedPipe(
                    self.pipe_name,
                    win32pipe.PIPE_ACCESS_DUPLEX,
                    win32pipe.PIPE_TYPE_MESSAGE | win32pipe.PIPE_READMODE_MESSAGE | win32pipe.PIPE_WAIT,
                    win32pipe.PIPE_UNLIMITED_INSTANCES,
                    65536,
                    65536,
                    0,
                    self.security_attrs,
                )
            except Exception as e:
                print('创建命名管道失败:', e)
                # Sleep a bit to avoid hot loop on error
                import time
                time.sleep(1)
                continue

            try:
                try:
                    win32pipe.ConnectNamedPipe(handle, None)
                except pywintypes.error as e:
                    # ERROR_PIPE_CONNECTED means a client connected before ConnectNamedPipe was called
                    if e.winerror != 535:
                        win32file.CloseHandle(handle)
                        raise

                # Spawn thread to handle this connection
                t = threading.Thread(target=self._client_handler, args=(handle,))
                t.daemon = True # Ensure client threads don't block shutdown
                t.start()

            except Exception:
                try:
                    win32file.CloseHandle(handle)
                except:
                    pass


def send_command(pipe_name: str, payload: dict) -> dict:
    """Send a command to the named pipe and return the response."""
    full_pipe_name = r"\\.\pipe\%s" % pipe_name
    try:
        handle = win32file.CreateFile(
            full_pipe_name,
            win32file.GENERIC_READ | win32file.GENERIC_WRITE,
            0,
            None,
            win32file.OPEN_EXISTING,
            0,
            None
        )
    except pywintypes.error as e:
        if e.winerror == 2:  # ERROR_FILE_NOT_FOUND
            raise FileNotFoundError(f"Pipe {pipe_name} not found. Is the monitor running?")
        raise

    try:
        data = json.dumps(payload).encode('utf-8')
        win32file.WriteFile(handle, data)
        
        # Read response
        resp_code, data = win32file.ReadFile(handle, 65536)
        text = data.decode('utf-8').strip()
        if not text:
            return {}
        return json.loads(text)
    finally:
        win32file.CloseHandle(handle)

