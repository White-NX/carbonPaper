"""Monitor configuration and state management.

Holds runtime state variables (paused/stopped events, interval) and
exclusion / advanced capture settings that are synchronised from the
Rust host via IPC commands.
"""

import os
import io
import json
import base64
import threading
import logging
from typing import Optional, Dict, Any

logger = logging.getLogger(__name__)

try:
    import win32gui
    import win32ui
    import win32con
    from ctypes import wintypes
except Exception:
    from ctypes import wintypes
    win32gui = None
    win32ui = None
    win32con = None

import ctypes
from PIL import Image


def get_data_dir():
    local_appdata = os.environ.get("LOCALAPPDATA")
    if not local_appdata:
        raise RuntimeError("LOCALAPPDATA environment variable not set")
    return os.path.join(local_appdata, "CarbonPaper", "data")


# ---------------------------------------------------------------------------
# Runtime interval
# ---------------------------------------------------------------------------
INTERVAL = 10  # seconds

# ---------------------------------------------------------------------------
# Exclusion configuration
# ---------------------------------------------------------------------------

# Built-in exclusion keywords (privacy / incognito indicators)
EXCLUSION_KEYWORDS = ["InPrivate", "Incognito"]

EXCLUSION_TITLES = [
    "Windows Default Lock Screen",
    "Search",
    "Program Manager",
    "Task Switching",
]

# User-configurable exclusion sets
USER_EXCLUDED_PROCESSES: set = set()
USER_EXCLUDED_TITLES: set = set()
IGNORE_PROTECTED_WINDOWS: bool = True

# Advanced capture config (synced from Rust CaptureState)
_capture_on_ocr_busy: bool = False
_ocr_queue_max_size: int = 1

FILTER_SETTINGS_PATH = os.path.join(get_data_dir(), "monitor_filters.json")

# ---------------------------------------------------------------------------
# Threading events
# ---------------------------------------------------------------------------
paused_event = threading.Event()   # set == paused
stop_event = threading.Event()     # set == stopped


# ---------------------------------------------------------------------------
# Exclusion settings helpers
# ---------------------------------------------------------------------------

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
        logger.error("Failed to persist capture filters: %s", exc)


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
                logger.error("Failed to load bundled capture filters: %s", exc)
        return
    except json.JSONDecodeError as exc:
        logger.error("Capture filter settings file is invalid JSON: %s", exc)
    except Exception as exc:
        logger.error("Failed to load capture filters: %s", exc)


def update_exclusion_settings(processes=None, titles=None, ignore_protected=None):
    """Update user-defined exclusion rules."""
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
    """Return current exclusion settings as a JSON-serialisable dict."""
    return {
        "processes": sorted(USER_EXCLUDED_PROCESSES),
        "titles": sorted(USER_EXCLUDED_TITLES),
        "ignore_protected": IGNORE_PROTECTED_WINDOWS,
    }


def update_advanced_capture_config(capture_on_ocr_busy: bool, ocr_queue_max_size: int):
    """Update advanced capture configuration (called via IPC, takes effect immediately)."""
    global _capture_on_ocr_busy, _ocr_queue_max_size
    _capture_on_ocr_busy = capture_on_ocr_busy
    _ocr_queue_max_size = max(1, ocr_queue_max_size)


# Load persisted settings on module import
_load_exclusion_settings()


# ---------------------------------------------------------------------------
# Process icon extraction
# ---------------------------------------------------------------------------

_process_icon_cache: Dict[str, Optional[str]] = {}


def _extract_icon_handle(exe_path: str):
    """Try to extract an icon handle from an executable file."""
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
    """Convert an HICON to a PNG Base64 string."""
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
    """Extract and cache a process icon as Base64 PNG."""
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
