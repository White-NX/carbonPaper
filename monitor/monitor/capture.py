import os
import time
import datetime
import threading
import json
import base64
import io
from typing import Optional, Callable, Tuple, Any, Dict

from mss import mss
from PIL import Image

import ctypes

try:
    import win32gui
    import win32ui
    import win32con
    from ctypes import wintypes
except Exception:
    from ctypes import wintypes

    # Set win32gui to None for check in _get_active_window_info
    win32gui = None
    win32ui = None
    win32con = None


def get_data_dir():
    local_appdata = os.environ.get("LOCALAPPDATA")
    if not local_appdata:
        raise RuntimeError("LOCALAPPDATA 环境变量未设置")
    return os.path.join(local_appdata, "CarbonPaper", "data")


SCREENSHOT_DIR = os.path.join(get_data_dir(), "screenshots")
os.makedirs(SCREENSHOT_DIR, exist_ok=True)

# 配置
INTERVAL = 10  # 秒
MAX_SIDE = 1600  # 最大边长
JPEG_QUALITY = 75  

# 忽略配置
EXCLUSION_KEYWORDS = ["InPrivate", "Incognito", "隐身", "私密", "无痕"]
EXCLUSION_TITLES = [
    "Windows Default Lock Screen",
    "Search",
    "Program Manager",
    "Task Switching",
]

# 用户可配置的忽略项
USER_EXCLUDED_PROCESSES = set()
USER_EXCLUDED_TITLES = set()
IGNORE_PROTECTED_WINDOWS = True

FILTER_SETTINGS_PATH = os.path.join(get_data_dir(), "monitor_filters.json")


def _get_default_filter_settings_path() -> Optional[str]:
    """Locate bundled default filter settings file if present."""
    try:
        candidate = os.path.abspath(
            os.path.join(os.path.dirname(__file__), os.pardir, "monitor_filters.json")
        )
        if os.path.isfile(candidate):
            return candidate
    except Exception:
        return None
    return None


def _apply_exclusion_settings(data: Dict[str, Any]):
    """Apply exclusion settings from a parsed JSON payload."""
    global USER_EXCLUDED_PROCESSES, USER_EXCLUDED_TITLES, IGNORE_PROTECTED_WINDOWS
    processes = data.get("processes") if isinstance(data, dict) else None
    titles = data.get("titles") if isinstance(data, dict) else None
    ignore_protected = data.get("ignore_protected") if isinstance(data, dict) else None
    if processes is not None:
        USER_EXCLUDED_PROCESSES = {
            p.strip().lower() for p in processes if isinstance(p, str) and p.strip()
        }
    if titles is not None:
        USER_EXCLUDED_TITLES = {
            t.strip().lower() for t in titles if isinstance(t, str) and t.strip()
        }
    if ignore_protected is not None:
        IGNORE_PROTECTED_WINDOWS = bool(ignore_protected)


def _persist_exclusion_settings():
    """Write current exclusion rules to disk for reuse after restarts."""
    payload = get_exclusion_settings()
    try:
        os.makedirs(os.path.dirname(FILTER_SETTINGS_PATH), exist_ok=True)
        tmp_path = FILTER_SETTINGS_PATH + ".tmp"
        with open(tmp_path, "w", encoding="utf-8") as tmp_file:
            json.dump(payload, tmp_file, ensure_ascii=True, indent=2)
        os.replace(tmp_path, FILTER_SETTINGS_PATH)
    except Exception as exc:
        print(f"Failed to persist capture filters: {exc}")


def _load_exclusion_settings():
    """Load exclusion rules from disk if present."""
    try:
        with open(FILTER_SETTINGS_PATH, "r", encoding="utf-8") as settings_file:
            data = json.load(settings_file)
        _apply_exclusion_settings(data)
    except FileNotFoundError:
        default_path = _get_default_filter_settings_path()
        if default_path:
            try:
                with open(default_path, "r", encoding="utf-8") as settings_file:
                    data = json.load(settings_file)
                if isinstance(data, dict):
                    _apply_exclusion_settings(data)
                    _persist_exclusion_settings()
            except Exception as exc:
                print(f"Failed to load bundled capture filters: {exc}")
        return
    except json.JSONDecodeError as exc:
        print(f"Capture filter settings file is invalid JSON: {exc}")
    except Exception as exc:
        print(f"Failed to load capture filters: {exc}")


def update_exclusion_settings(processes=None, titles=None, ignore_protected=None):
    """更新用户定义的忽略规则"""
    global USER_EXCLUDED_PROCESSES, USER_EXCLUDED_TITLES, IGNORE_PROTECTED_WINDOWS

    if processes is not None:
        USER_EXCLUDED_PROCESSES = {
            p.strip().lower() for p in processes if isinstance(p, str) and p.strip()
        }
    if titles is not None:
        USER_EXCLUDED_TITLES = {
            t.strip().lower() for t in titles if isinstance(t, str) and t.strip()
        }
    if ignore_protected is not None:
        IGNORE_PROTECTED_WINDOWS = bool(ignore_protected)

    _persist_exclusion_settings()


def get_exclusion_settings():
    return {
        "processes": sorted(USER_EXCLUDED_PROCESSES),
        "titles": sorted(USER_EXCLUDED_TITLES),
        "ignore_protected": IGNORE_PROTECTED_WINDOWS,
    }


_load_exclusion_settings()

# 控制事件
paused_event = threading.Event()  # set 表示已暂停
stop_event = threading.Event()  # set 表示已停止

_worker: Optional[threading.Thread] = None
# 回调签名: (image_bytes: bytes, image_pil: Image, info: dict) -> None
_on_screenshot_captured: Optional[Callable[[bytes, Image.Image, dict], None]] = None
_process_icon_cache: Dict[str, Optional[str]] = {}


def set_screenshot_callback(callback: Callable[[bytes, Image.Image, dict], None]):
    """设置截图回调函数
    
    Args:
        callback: 回调函数，接收三个参数：
            - image_bytes: JPEG 格式的图片字节数据（用于发送到存储服务）
            - image_pil: PIL Image 对象（用于 OCR 处理）
            - info: 包含 window_title, process_name, metadata, width, height 的字典
    """
    global _on_screenshot_captured
    _on_screenshot_captured = callback


def _get_active_window_info() -> Tuple[Any, str, Tuple[int, int, int, int]]:
    """获取当前活动窗口的句柄、标题和坐标 rect (left, top, right, bottom)"""
    hwnd = None
    window_title = "Unknown"
    rect = (0, 0, 0, 0)

    try:
        if win32gui:
            hwnd = win32gui.GetForegroundWindow()
            window_title = win32gui.GetWindowText(hwnd)
            rect = win32gui.GetWindowRect(hwnd)
        else:
            # Ctypes fallback
            user32 = ctypes.windll.user32
            hwnd = user32.GetForegroundWindow()
            length = user32.GetWindowTextLengthW(hwnd)
            buff = ctypes.create_unicode_buffer(length + 1)
            user32.GetWindowTextW(hwnd, buff, length + 1)
            window_title = buff.value

            r = wintypes.RECT()
            user32.GetWindowRect(hwnd, ctypes.byref(r))
            rect = (r.left, r.top, r.right, r.bottom)
    except Exception:
        pass

    return hwnd, window_title, rect


def _get_window_process_path(hwnd: int) -> str:
    """获取窗口所属进程的可执行路径"""
    try:
        import win32process
        import psutil

        _, pid = win32process.GetWindowThreadProcessId(hwnd)
        proc = psutil.Process(pid)
        return proc.exe()
    except Exception:
        return ""


def _extract_icon_handle(exe_path: str):
    """尝试从可执行文件中提取图标句柄"""
    if not exe_path:
        return None
    try:
        icon_large = (wintypes.HICON * 1)()
        icon_small = (wintypes.HICON * 1)()
        count = ctypes.windll.shell32.ExtractIconExW(
            exe_path, 0, icon_large, icon_small, 1
        )
        if count > 0:
            return icon_large[0] or icon_small[0]
    except Exception:
        return None
    return None


def _hicon_to_base64(hicon, size: int = 32) -> Optional[str]:
    """将 HICON 转换为 PNG Base64"""
    if not hicon or not win32gui or not win32ui or not win32con:
        return None
    hdc = None
    memdc = None
    bmp = None
    try:
        desktop_dc = win32gui.GetDC(0)
        hdc = win32ui.CreateDCFromHandle(desktop_dc)
        memdc = hdc.CreateCompatibleDC()
        bmp = win32ui.CreateBitmap()
        bmp.CreateCompatibleBitmap(hdc, size, size)
        memdc.SelectObject(bmp)
        win32gui.DrawIconEx(
            memdc.GetHandleOutput(),
            0,
            0,
            hicon,
            size,
            size,
            0,
            None,
            win32con.DI_NORMAL,
        )
        bmpinfo = bmp.GetInfo()
        bmpstr = bmp.GetBitmapBits(True)
        img = Image.frombuffer(
            "RGBA",
            (bmpinfo["bmWidth"], bmpinfo["bmHeight"]),
            bmpstr,
            "raw",
            "BGRA",
            0,
            1,
        )
        buffer = io.BytesIO()
        img.save(buffer, format="PNG")
        return base64.b64encode(buffer.getvalue()).decode("utf-8")
    except Exception:
        return None
    finally:
        try:
            if hicon and win32gui:
                win32gui.DestroyIcon(hicon)
        except Exception:
            pass
        try:
            if bmp:
                win32gui.DeleteObject(bmp.GetHandle())
        except Exception:
            pass
        try:
            if memdc:
                memdc.DeleteDC()
        except Exception:
            pass
        try:
            if hdc:
                win32gui.ReleaseDC(0, hdc.GetHandleOutput())
                hdc.DeleteDC()
        except Exception:
            pass


def _get_process_icon_base64(exe_path: str) -> Optional[str]:
    """提取并缓存进程图标的 Base64"""
    if not exe_path:
        return None

    cached = _process_icon_cache.get(exe_path)
    if cached is not None:
        return cached

    icon_b64 = None
    try:
        hicon = _extract_icon_handle(exe_path)
        if hicon:
            icon_b64 = _hicon_to_base64(hicon)
    except Exception:
        icon_b64 = None

    _process_icon_cache[exe_path] = icon_b64
    return icon_b64


def _capture_window_image_data() -> Tuple[Optional[Image.Image], dict, str, Any]:
    """
    捕获当前焦点的截图
    Returns: (PIL.Image, monitor_dict, window_title, hwnd)
    """
    with mss() as sct:
        hwnd, window_title, (left, top, right, bottom) = _get_active_window_info()

        if left < 0:
            left = 0
        if top < 0:
            top = 0
        width = max(1, right - left)
        height = max(1, bottom - top)

        monitor = {"left": left, "top": top, "width": width, "height": height}

        # MSS grab
        try:
            img = sct.grab(monitor)
            img_pil = Image.frombytes("RGB", img.size, img.rgb)

            if max(img_pil.size) > MAX_SIDE:
                ratio = MAX_SIDE / max(img_pil.size)
                new_size = (int(img_pil.width * ratio), int(img_pil.height * ratio))
                img_pil = img_pil.resize(new_size, Image.LANCZOS)

            return img_pil, monitor, window_title, hwnd
        except Exception as e:
            # print(f"MSS Capture failed: {e}")
            return None, monitor, window_title, hwnd


def capture_focused_window(save_path: str):
    """
    旧接口兼容：捕获并保存到文件
    """
    img_pil, monitor, window_title, _ = _capture_window_image_data()
    if img_pil:
        img_pil.save(
            save_path,
            format="JPEG",
            quality=JPEG_QUALITY,
            optimize=True,
            progressive=True,
        )
        return monitor, window_title
    else:
        monitor = {"left": 0, "top": 0, "width": 0, "height": 0}
        return monitor, "Capture Failed"


# --- Similarity and Filtering Logic ---


def _get_window_process_command_line(hwnd: int) -> str:
    """获取窗口所属进程的命令行参数 (如果可用)"""
    try:
        import win32process
        import win32com.client

        _, pid = win32process.GetWindowThreadProcessId(hwnd)

        # 使用 WMI 查询命令行，这比较慢，可以考虑加缓存优化
        wmi = win32com.client.GetObject("winmgmts:")
        processes = wmi.ExecQuery(
            f"SELECT CommandLine FROM Win32_Process WHERE ProcessId = {pid}"
        )

        for p in processes:
            return (p.CommandLine or "").lower()

    except ImportError:
        pass  # pywin32 not installed or com error
    except Exception:
        pass

    return ""


# 获取窗口的进程名称
def _get_window_process_name(hwnd: int) -> str:
    """获取窗口所属进程的可执行文件名 (如果可用)"""
    try:
        import win32process
        import win32api
        import psutil

        _, pid = win32process.GetWindowThreadProcessId(hwnd)
        proc = psutil.Process(pid)
        return os.path.basename(proc.exe()).lower()

    except ImportError:
        pass  # pywin32 or psutil not installed
    except Exception:
        pass

    return ""


def _is_window_protected(hwnd: int) -> bool:
    """检查窗口是否设置了 WDA_EXCLUDEFROMCAPTURE 或 WDA_MONITOR"""
    if not hwnd:
        return False
    try:
        user32 = ctypes.windll.user32
        dw_affinity = ctypes.c_ulong()
        # BOOL GetWindowDisplayAffinity(HWND hWnd, DWORD *pdwAffinity);
        if user32.GetWindowDisplayAffinity(hwnd, ctypes.byref(dw_affinity)):
            # WDA_NONE = 0
            return dw_affinity.value != 0
    except Exception:
        pass
    return False


def _get_dhash(image: Image.Image, hash_size=16) -> int:
    """计算图片的 dHash (差异哈希)"""
    # 转换为灰度，调整大小为 (hash_size+1, hash_size)
    if not image:
        return 0
    try:
        image = image.convert("L").resize((hash_size + 1, hash_size), Image.LANCZOS)
    except Exception:
        # Fallback to BILINEAR if LANCZOS fails (e.g. old PIL)
        image = image.convert("L").resize((hash_size + 1, hash_size), Image.BILINEAR)

    pixels = list(image.getdata())

    diff = []
    width = hash_size + 1
    for row in range(hash_size):
        for col in range(hash_size):
            pixel_left = pixels[row * width + col]
            pixel_right = pixels[row * width + col + 1]
            diff.append(pixel_left > pixel_right)

    decimal_value = 0
    for index, value in enumerate(diff):
        if value:
            decimal_value += 2**index
    return decimal_value


def _hamming_distance(hash1: int, hash2: int) -> int:
    return bin(hash1 ^ hash2).count("1")


def _is_excluded(title: str, hwnd: int = None, process_name: str = None) -> bool:
    """检查窗口是否应被忽略"""
    # 1. 标题检查
    if not title:
        return True  # Ignore empty titles

    title_lower = title.lower()

    for kw in EXCLUSION_KEYWORDS:
        if kw in title:
            return True

    for t in EXCLUSION_TITLES:
        if t == title or title.startswith(t):
            return True

    for user_kw in USER_EXCLUDED_TITLES:
        if user_kw and user_kw in title_lower:
            return True

    if not hwnd:
        return False

    # 2. 防截屏保护检查 (SetWindowDisplayAffinity)
    if IGNORE_PROTECTED_WINDOWS and _is_window_protected(hwnd):
        # 如果窗口明确拒绝被截屏，我们应尊重并忽略
        return True

    # 3. 进程名忽略
    if USER_EXCLUDED_PROCESSES:
        pn = process_name
        if not pn:
            pn = _get_window_process_name(hwnd)
        pn = (pn or "").lower()
        if pn and pn in USER_EXCLUDED_PROCESSES:
            return True

    # 3. 命令行参数深度检查 (Edge/Chrome Incognito)
    # 这个操作比较重，只有当标题看起来像浏览器但没包含关键词时才做，或者周期性做

    # 简易优化：只有当标题包含 "Edge", "Chrome", "Firefox", "Browser" 时才查命令行
    browser_keywords = ["edge", "chrome", "firefox", "browser", "浏览器"]
    title_lower = title.lower()
    if any(bk in title_lower for bk in browser_keywords):
        cmd_line = _get_window_process_command_line(hwnd)
        privacy_flags = ["--incognito", "-inprivate", "-private", "--private-window"]
        if any(flag in cmd_line for flag in privacy_flags):
            return True

    return False


def _is_redundant(current_hash: int, history: list, threshold: int = 10) -> bool:
    """检查是否与历史记录中的图片过于相似"""
    if not history:
        return False

    # Check against recent 3 cycles
    for h_hash in history:
        dist = _hamming_distance(current_hash, h_hash)
        if dist < threshold:
            return True
    return False


def capture_loop(interval: int = INTERVAL):
    print(f"智能截图循环已启动，基础间隔 {interval} 秒。")
    print(f"忽略关键词: {EXCLUSION_KEYWORDS}")

    last_hwnd = None
    last_capture_time = 0

    # 历史记录: 存储最近3次的 dHash
    history_hashes = []

    polling_rate = 0.5  # 快速轮询检测焦点变化

    while not stop_event.is_set():
        if paused_event.is_set():
            time.sleep(0.5)
            continue

        now = time.time()

        # 1. 获取当前状态
        hwnd, title, _ = _get_active_window_info()

        process_name_for_filter = None
        if USER_EXCLUDED_PROCESSES and hwnd:
            process_name_for_filter = _get_window_process_name(hwnd)

        # 2. 检查忽略列表
        if _is_excluded(title, hwnd, process_name_for_filter):
            last_hwnd = hwnd
            time.sleep(polling_rate)
            continue

        should_capture = False
        scan_reason = ""

        # 3. 焦点切换触发（仅在 OCR 队列未过载时）
        if hwnd != last_hwnd:
            # 动态导入以避免导入循环
            try:
                from monitor import _ocr_worker
            except Exception:
                _ocr_worker = None

            # 从共享常量读取最大允许 pending 数量
            try:
                from monitor.constants import MAX_PENDING
            except Exception:
                MAX_PENDING = 1

            pending = 0
            try:
                if _ocr_worker:
                    pending = _ocr_worker.pending_count()
            except Exception:
                pending = 0

            # 当队列中有多于 MAX_PENDING 张图片时，跳过焦点触发的截图
            if pending > MAX_PENDING:
                should_capture = False
            else:
                should_capture = True
                scan_reason = f"focus_change"

        # 4. 时间间隔触发
        elif now - last_capture_time >= interval:
            should_capture = True
            scan_reason = "interval"

        if should_capture:
            try:

                if scan_reason == "focus_change":
                    time.sleep(0.5)  # 等待窗口稳定

                img_pil, monitor, captured_title, captured_hwnd = (
                    _capture_window_image_data()
                )

                if img_pil:
                    # 5. 相似度去重
                    current_hash = _get_dhash(img_pil)

                    if _is_redundant(current_hash, history_hashes):
                        pass
                    else:
                        ts_str = datetime.datetime.now().strftime("%Y%m%d_%H%M%S")
                        
                        # 将图片转换为内存中的 JPEG 字节数据（不写入磁盘）
                        img_buffer = io.BytesIO()
                        img_pil.save(
                            img_buffer,
                            format="JPEG",
                            quality=JPEG_QUALITY,
                            optimize=True,
                            progressive=True,
                        )
                        img_bytes = img_buffer.getvalue()
                        
                        print(
                            f"[{ts_str}] 截图捕获 ({scan_reason}): {len(img_bytes)} bytes - {captured_title}"
                        )

                        # 更新历史
                        history_hashes.append(current_hash)
                        if len(history_hashes) > 3:
                            history_hashes.pop(0)

                        # 获取进程信息
                        process_name = (
                            process_name_for_filter
                            or _get_window_process_name(captured_hwnd)
                        )
                        process_path = _get_window_process_path(captured_hwnd)
                        process_icon = _get_process_icon_base64(process_path)

                        metadata = {
                            "monitor": monitor,
                            "dhash": current_hash,
                            "process_path": process_path,
                            "process_icon": process_icon,
                            "timestamp": ts_str,
                        }

                        # 回调 - 传递内存中的图片数据和 PIL Image 对象（用于 OCR）
                        if _on_screenshot_captured:
                            _on_screenshot_captured(
                                img_bytes,  # 内存中的 JPEG 数据
                                img_pil,    # PIL Image 对象（用于 OCR）
                                {
                                    "window_title": captured_title,
                                    "process_name": process_name,
                                    "metadata": metadata,
                                    "width": img_pil.width,
                                    "height": img_pil.height,
                                },
                            )

                    last_capture_time = now
                    last_hwnd = captured_hwnd

            except Exception as e:
                print(f"截图处理异常: {e}")

        # 快速轮询
        time.sleep(polling_rate)


def start_capture_thread():
    global _worker
    if _worker and _worker.is_alive():
        return _worker
    _worker = threading.Thread(target=capture_loop, daemon=True)
    _worker.start()
    return _worker
