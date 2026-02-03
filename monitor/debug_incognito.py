import ctypes
import time
import win32process
import win32gui
import win32api
import sys
import win32com.client

def get_window_display_affinity(hwnd):
    """
    Wraps GetWindowDisplayAffinity.
    
    WDA_NONE = 0x00000000
    WDA_MONITOR = 0x00000001 (The window content is displayed only on the monitor)
    WDA_EXCLUDEFROMCAPTURE = 0x00000011 (displayed on monitor but not captured)
    """
    user32 = ctypes.windll.user32
    # BOOL GetWindowDisplayAffinity(HWND hWnd, DWORD *pdwAffinity);
    dw_affinity = ctypes.c_ulong()
    
    # Define argument types for safety
    user32.GetWindowDisplayAffinity.argtypes = [ctypes.c_void_p, ctypes.POINTER(ctypes.c_ulong)]
    user32.GetWindowDisplayAffinity.restype = ctypes.c_bool
    
    success = user32.GetWindowDisplayAffinity(hwnd, ctypes.byref(dw_affinity))
    if success:
        return dw_affinity.value
    else:
        return None

def get_process_command_line(pid):
    """
    Retreives command line arguments for a given PID using WMI directly via win32com.
    """
    try:
        wmi = win32com.client.GetObject("winmgmts:")
        processes = wmi.ExecQuery(f"SELECT CommandLine FROM Win32_Process WHERE ProcessId = {pid}")
        for p in processes:
            return p.CommandLine
    except Exception as e:
        return f"Error fetching command line: {e}"
    return None

def analyze_foreground_window():
    hwnd = win32gui.GetForegroundWindow()
    title = win32gui.GetWindowText(hwnd)
    _, pid = win32process.GetWindowThreadProcessId(hwnd)
    
    print("-" * 20)
    print(f"Window Handle: {hwnd}")
    print(f"Title: {title}")
    print(f"PID: {pid}")
    
    # 1. Check Display Affinity
    affinity = get_window_display_affinity(hwnd)
    app_affinity_str = "Unknown"
    if affinity is not None:
        if affinity == 0: app_affinity_str = "WDA_NONE (0)"
        elif affinity == 1: app_affinity_str = "WDA_MONITOR (1)"
        elif affinity == 0x11: app_affinity_str = "WDA_EXCLUDEFROMCAPTURE (0x11)"
        else: app_affinity_str = f"Wait... custom? ({affinity})"
    
    print(f"Display Affinity: {app_affinity_str}")
    
    # 2. Check Command Line (Look for incognito flags)
    cmd_line = get_process_command_line(pid)
    print(f"Command Line: {cmd_line}")
    
    is_private_arg = False
    if cmd_line:
        lower_cmd = cmd_line.lower()
        if "--incognito" in lower_cmd or "-private" in lower_cmd or "-inprivate" in lower_cmd:
            is_private_arg = True
    
    print(f"Detected Private Flag in CLI: {is_private_arg}")
    print("-" * 20)

if __name__ == "__main__":
    print("Focus on a window to analyze it in 3 seconds...")
    time.sleep(3)
    analyze_foreground_window()
