use crate::monitor::MonitorState;
use crate::storage::{OcrResultInput, SaveScreenshotRequest, StorageState};
use base64::Engine;
use image::codecs::jpeg::JpegEncoder;
use image::{DynamicImage, GenericImageView, ImageEncoder, RgbImage};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use tauri::Manager;

use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_FORMAT, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetClassNameW, GetForegroundWindow, GetWindowDisplayAffinity, GetWindowRect, GetWindowTextW,
    GetWindowThreadProcessId,
};

use windows::core::{Interface, IInspectable};
use windows::Foundation::{EventRegistrationToken, IClosable, TypedEventHandler};
use windows::Graphics::Capture::{
    Direct3D11CaptureFramePool, GraphicsCaptureItem, GraphicsCaptureSession, Direct3D11CaptureFrame
};
use windows::Graphics::DirectX::DirectXPixelFormat;
use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext,
    D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT,
    D3D11_MAPPED_SUBRESOURCE, D3D11_MAP_READ, D3D11_SDK_VERSION,
    D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING, ID3D11Texture2D,
};
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL_11_0};
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::Win32::System::WinRT::Direct3D11::{
    CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess,
};
use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;
use std::sync::mpsc::{sync_channel, Receiver};

// ==================== Configuration ====================

/// Configuration for screen capture behavior (intervals, quality, dedup thresholds).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaptureConfig {
    pub interval_secs: u64,
    pub polling_rate_ms: u64,
    pub max_side: u32,
    pub jpeg_quality: u8,
    pub dhash_threshold: u32,
    pub dhash_history_size: usize,
}

impl Default for CaptureConfig {
    fn default() -> Self {
        Self {
            interval_secs: 10,
            polling_rate_ms: 500,
            max_side: 1600,
            jpeg_quality: 65,
            dhash_threshold: 10,
            dhash_history_size: 3,
        }
    }
}

/// Settings for excluding specific windows and processes from capture.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExclusionSettings {
    pub exclusion_keywords: Vec<String>,
    pub exclusion_titles: Vec<String>,
    pub user_excluded_processes: HashSet<String>,
    pub user_excluded_titles: HashSet<String>,
    pub ignore_protected_windows: bool,
}

impl Default for ExclusionSettings {
    fn default() -> Self {
        Self {
            exclusion_keywords: vec![
                "InPrivate".to_string(),
                "Incognito".to_string(),
                "隐身".to_string(),
                "私密".to_string(),
                "无痕".to_string(),
            ],
            exclusion_titles: vec![
                "Windows Default Lock Screen".to_string(),
                "Search".to_string(),
                "Program Manager".to_string(),
                "Task Switching".to_string(),
            ],
            user_excluded_processes: HashSet::new(),
            user_excluded_titles: HashSet::new(),
            ignore_protected_windows: true,
        }
    }
}

// ==================== WGC Window Capture ====================

pub struct WgcCaptureSession {
    hwnd: isize,
    session: GraphicsCaptureSession,
    frame_pool: Direct3D11CaptureFramePool,
    frame_arrived_token: Option<EventRegistrationToken>,
    rx: Receiver<Direct3D11CaptureFrame>,
    d3d_device: ID3D11Device,
    d3d_context: ID3D11DeviceContext,
    winrt_device: IDirect3DDevice,
    item: GraphicsCaptureItem,
    current_size: windows::Graphics::SizeInt32,
    last_image: Option<CapturedImage>,
}

// Safety: WGC COM objects are agile, D3D11 context usage is serialized by the Mutex.
unsafe impl Send for WgcCaptureSession {}

impl WgcCaptureSession {
    fn teardown(&mut self) {
        if let Some(token) = self.frame_arrived_token.take() {
            if let Err(e) = self.frame_pool.RemoveFrameArrived(token) {
                tracing::debug!("WGC: RemoveFrameArrived failed during teardown: {:?}", e);
            }
        }

        if let Ok(closable) = self.session.cast::<IClosable>() {
            if let Err(e) = closable.Close() {
                tracing::debug!("WGC: closing GraphicsCaptureSession failed: {:?}", e);
            }
        }
        if let Ok(closable) = self.frame_pool.cast::<IClosable>() {
            if let Err(e) = closable.Close() {
                tracing::debug!("WGC: closing Direct3D11CaptureFramePool failed: {:?}", e);
            }
        }

        self.last_image = None;
        while self.rx.try_recv().is_ok() {}
    }
}

impl Drop for WgcCaptureSession {
    fn drop(&mut self) {
        self.teardown();
    }
}

// ==================== Capture State ====================

/// In-memory cache for JPEG bytes awaiting OCR.
/// Keyed by screenshot_id. Entries are inserted before sending to Python
/// and removed after OCR completes (commit or abort). This avoids reading
/// from encrypted storage (which triggers Windows Hello CNG decryption).
pub type OcrImageCache = Arc<Mutex<HashMap<i64, Vec<u8>>>>;

/// Shared state for the capture subsystem, including pause/stop flags and OCR backpressure.
pub struct CaptureState {
    pub paused: AtomicBool,
    pub stopped: AtomicBool,
    pub config: Mutex<CaptureConfig>,
    pub exclusion_settings: Mutex<ExclusionSettings>,
    pub in_flight_ocr_count: AtomicU32,
    pub capture_on_ocr_busy: AtomicBool,
    pub ocr_queue_max_size: AtomicU32,
    pub capture_task: Mutex<Option<tauri::async_runtime::JoinHandle<()>>>,
    pub ocr_image_cache: OcrImageCache,
    pub wgc_state: Mutex<Option<WgcCaptureSession>>,
    /// Game mode: capture paused because a non-browser fullscreen app is in the foreground
    pub game_mode_capture_paused: AtomicBool,
}

impl Default for CaptureState {
    fn default() -> Self {
        Self::new()
    }
}

impl CaptureState {
    /// Creates a new, default-initialized `CaptureState` instance with empty filters and caches.
    pub fn new() -> Self {
        Self {
            paused: AtomicBool::new(false),
            stopped: AtomicBool::new(false),
            config: Mutex::new(CaptureConfig::default()),
            exclusion_settings: Mutex::new(ExclusionSettings::default()),
            in_flight_ocr_count: AtomicU32::new(0),
            capture_on_ocr_busy: AtomicBool::new(false),
            ocr_queue_max_size: AtomicU32::new(1),
            capture_task: Mutex::new(None),
            ocr_image_cache: Arc::new(Mutex::new(HashMap::new())),
            wgc_state: Mutex::new(None),
            game_mode_capture_paused: AtomicBool::new(false),
        }
    }

    /// Explicitly drops the current WGC session and releases capture resources.
    pub fn clear_wgc_session(&self, reason: &str) {
        let mut guard = self.wgc_state.lock().unwrap_or_else(|e| e.into_inner());
        if guard.is_some() {
            tracing::info!("WGC: clearing capture session ({})", reason);
        }
        *guard = None;
    }

    /// Loads user-defined exclusion settings (processes and titles) from the `monitor_filters.json` file.
    pub fn load_exclusion_settings(&self, data_dir: &std::path::Path) {
        let path = data_dir.join("monitor_filters.json");
        if let Ok(content) = std::fs::read_to_string(&path) {
            if let Ok(data) = serde_json::from_str::<serde_json::Value>(&content) {
                let mut settings = self
                    .exclusion_settings
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                if let Some(processes) = data.get("processes").and_then(|v| v.as_array()) {
                    settings.user_excluded_processes = processes
                        .iter()
                        .filter_map(|v| v.as_str())
                        .filter(|s| !s.trim().is_empty())
                        .map(|s| s.trim().to_lowercase())
                        .collect();
                }
                if let Some(titles) = data.get("titles").and_then(|v| v.as_array()) {
                    settings.user_excluded_titles = titles
                        .iter()
                        .filter_map(|v| v.as_str())
                        .filter(|s| !s.trim().is_empty())
                        .map(|s| s.trim().to_lowercase())
                        .collect();
                }
                if let Some(ignore_protected) =
                    data.get("ignore_protected").and_then(|v| v.as_bool())
                {
                    settings.ignore_protected_windows = ignore_protected;
                }
                tracing::info!(
                    "Loaded exclusion settings: {} processes, {} titles",
                    settings.user_excluded_processes.len(),
                    settings.user_excluded_titles.len()
                );
            }
        }
    }

    /// Saves the current exclusion settings to the `monitor_filters.json` file, using a safe temporary file renaming approach.
    pub fn save_exclusion_settings(&self, data_dir: &std::path::Path) {
        let settings = self
            .exclusion_settings
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let payload = serde_json::json!({
            "processes": settings.user_excluded_processes.iter().cloned().collect::<Vec<_>>(),
            "titles": settings.user_excluded_titles.iter().cloned().collect::<Vec<_>>(),
            "ignore_protected": settings.ignore_protected_windows,
        });
        let path = data_dir.join("monitor_filters.json");
        if let Ok(content) = serde_json::to_string_pretty(&payload) {
            let tmp_path = path.with_extension("json.tmp");
            if std::fs::write(&tmp_path, &content).is_ok() {
                let _ = std::fs::rename(&tmp_path, &path);
            }
        }
    }

    /// Updates the exclusion filters in memory with new process names, window titles, or the protected window ignore flag.
    pub fn update_exclusion_settings(
        &self,
        processes: Option<Vec<String>>,
        titles: Option<Vec<String>>,
        ignore_protected: Option<bool>,
    ) {
        let mut settings = self
            .exclusion_settings
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(procs) = processes {
            settings.user_excluded_processes = procs
                .into_iter()
                .filter(|s| !s.trim().is_empty())
                .map(|s| s.trim().to_lowercase())
                .collect();
        }
        if let Some(t) = titles {
            settings.user_excluded_titles = t
                .into_iter()
                .filter(|s| !s.trim().is_empty())
                .map(|s| s.trim().to_lowercase())
                .collect();
        }
        if let Some(ip) = ignore_protected {
            settings.ignore_protected_windows = ip;
        }
    }
}

// ==================== Active Window Detection ====================

/// Information about the currently focused window (handle, title, rect, PID).
pub struct ActiveWindowInfo {
    hwnd_raw: isize,
    title: String,
    rect: RECT,
    pid: u32,
}

/// Retrieves information about the currently focused foreground window,
/// including its handle, title, screen bounds, and the owning process ID.
pub fn get_active_window_info() -> Option<ActiveWindowInfo> {
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.0.is_null() {
            return None;
        }

        // Get window title
        let mut title_buf = [0u16; 512];
        let len = GetWindowTextW(hwnd, &mut title_buf);
        let title = if len > 0 {
            String::from_utf16_lossy(&title_buf[..len as usize])
        } else {
            String::new()
        };

        // Get window rect
        let mut rect = RECT::default();
        if GetWindowRect(hwnd, &mut rect).is_err() {
            return None;
        }

        // Get PID
        let mut pid: u32 = 0;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));

        Some(ActiveWindowInfo {
            hwnd_raw: hwnd.0 as isize,
            title,
            rect,
            pid,
        })
    }
}

/// Known browser executable names (lowercase, without extension).
const BROWSER_EXECUTABLES: &[&str] = &[
    "chrome",
    "chrome.exe",
    "msedge",
    "msedge.exe",
    "firefox",
    "firefox.exe",
    "brave",
    "brave.exe",
    "opera",
    "opera.exe",
    "vivaldi",
    "vivaldi.exe",
    "iexplore",
    "iexplore.exe",
    "360se",
    "360se.exe",
    "sogouexplorer",
    "sogouexplorer.exe",
    "qqbrowser",
    "qqbrowser.exe",
    "2345explorer",
    "2345explorer.exe",
    "maxthon",
    "maxthon.exe",
    "seamonkey",
    "seamonkey.exe",
    "waterfox",
    "waterfox.exe",
    "floorp",
    "floorp.exe",
    "librewolf",
    "librewolf.exe",
    "arc",
    "arc.exe",
];

/// Check if a process name (e.g. "chrome.exe") is a known browser.
pub fn is_browser_process(process_name: &str) -> bool {
    let lower = process_name.to_lowercase();
    BROWSER_EXECUTABLES.iter().any(|&name| lower == name)
}

/// Known system window classes that may appear fullscreen but are not games.
/// These are used to prevent false-positive game detection for elevated processes
/// whose process name cannot be queried.
const SYSTEM_FULLSCREEN_CLASSES: &[&str] = &[
    "progman",                    // Desktop Program Manager
    "workerw",                    // Desktop worker window
    "shell_traywnd",              // Taskbar
    "shell_secondarytraywnd",     // Secondary monitor taskbar
    "windows.ui.core.corewindow", // UWP system windows (Start menu, Action Center)
    "applicationframewindow",     // UWP app host frame
    "lockapp",                    // Lock screen (Windows 10+)
    "foregroundstaging",          // Window transition staging
    "multitaskingviewframe",      // Alt+Tab / Task View
    "ghost",                      // "Not Responding" ghost window
    "tooltips_class32",           // Tooltip
    "#32769",                     // Desktop
    "xaml_windowedpopupclass",    // XAML popup
];

/// Check whether a window class name belongs to a known system/shell window.
pub fn is_system_window_class(class_name: &str) -> bool {
    let lower = class_name.to_lowercase();
    SYSTEM_FULLSCREEN_CLASSES.iter().any(|&name| lower == name)
}

/// Detect whether the foreground window is covering the entire monitor (fullscreen).
/// Returns `Some((process_name, window_class, is_fullscreen))` or `None` if the
/// foreground window cannot be determined.
pub fn check_foreground_fullscreen() -> Option<(String, String, bool)> {
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.0.is_null() {
            return None;
        }

        // Get PID
        let mut pid: u32 = 0;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));

        let process_name = get_process_path_from_pid(pid)
            .map(|p| get_process_name_from_path(&p))
            .unwrap_or_default();

        // Get window class name (for system window filtering)
        let window_class = {
            let mut buf = [0u16; 256];
            let len = GetClassNameW(hwnd, &mut buf);
            if len > 0 {
                String::from_utf16_lossy(&buf[..len as usize])
            } else {
                String::new()
            }
        };

        // Get window rect
        let mut rect = RECT::default();
        if GetWindowRect(hwnd, &mut rect).is_err() {
            return Some((process_name, window_class, false));
        }

        // Get monitor info for the window's monitor
        let hmonitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        let mut monitor_info: MONITORINFO = std::mem::zeroed();
        monitor_info.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
        if !GetMonitorInfoW(hmonitor, &mut monitor_info).as_bool() {
            return Some((process_name, window_class, false));
        }
        let mon_rect = monitor_info.rcMonitor;

        // A window is considered fullscreen if it covers the entire monitor
        let is_fullscreen = rect.left <= mon_rect.left
            && rect.top <= mon_rect.top
            && rect.right >= mon_rect.right
            && rect.bottom >= mon_rect.bottom;

        Some((process_name, window_class, is_fullscreen))
    }
}

/// Retrieves the full executable path of a process given its PID, using Windows `QueryFullProcessImageNameW`.
pub fn get_process_path_from_pid(pid: u32) -> Option<String> {
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 1024];
        let mut size = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_FORMAT(0),
            windows::core::PWSTR(buf.as_mut_ptr()),
            &mut size,
        );
        let _ = windows::Win32::Foundation::CloseHandle(handle);
        if ok.is_ok() && size > 0 {
            Some(String::from_utf16_lossy(&buf[..size as usize]))
        } else {
            None
        }
    }
}

/// Extracts the lowercase file name (e.g., "chrome.exe") from a full executable path.
pub fn get_process_name_from_path(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}

fn get_process_command_line(pid: u32) -> Option<String> {
    use sysinfo::{Pid, ProcessRefreshKind, System, UpdateKind};
    let mut sys = System::new();
    sys.refresh_processes_specifics(ProcessRefreshKind::new().with_cmd(UpdateKind::Always));
    sys.process(Pid::from_u32(pid)).and_then(|p| {
        let cmd = p.cmd();
        if cmd.is_empty() {
            None
        } else {
            Some(
                cmd.iter()
                    .map(|s| s.to_string())
                    .collect::<Vec<_>>()
                    .join(" ")
                    .to_lowercase(),
            )
        }
    })
}

// ==================== Window Exclusion ====================

fn is_window_protected(hwnd_raw: isize) -> bool {
    unsafe {
        let hwnd = HWND(hwnd_raw as *mut _);
        let mut affinity: u32 = 0;
        GetWindowDisplayAffinity(hwnd, &mut affinity).is_ok() && affinity != 0
    }
}

fn is_excluded(info: &ActiveWindowInfo, settings: &ExclusionSettings) -> bool {
    // Empty title
    if info.title.is_empty() {
        return true;
    }

    let title = &info.title;
    let title_lower = title.to_lowercase();

    // Hardcoded keyword matching (case-sensitive, matching Python behavior)
    for kw in &settings.exclusion_keywords {
        if title.contains(kw.as_str()) {
            return true;
        }
    }

    // Hardcoded title exclusion
    for t in &settings.exclusion_titles {
        if title == t || title.starts_with(t.as_str()) {
            return true;
        }
    }

    // User-defined title exclusion (case-insensitive)
    for user_kw in &settings.user_excluded_titles {
        if !user_kw.is_empty() && title_lower.contains(user_kw.as_str()) {
            return true;
        }
    }

    // Protected window check
    if settings.ignore_protected_windows && is_window_protected(info.hwnd_raw) {
        return true;
    }

    // User-defined process name exclusion
    if !settings.user_excluded_processes.is_empty() {
        if let Some(path) = get_process_path_from_pid(info.pid) {
            let pname = get_process_name_from_path(&path);
            if !pname.is_empty() && settings.user_excluded_processes.contains(&pname) {
                return true;
            }
        }
    }

    // Browser incognito command line check
    let browser_keywords = ["edge", "chrome", "firefox", "browser", "浏览器"];
    if browser_keywords.iter().any(|bk| title_lower.contains(bk)) {
        if let Some(cmd_line) = get_process_command_line(info.pid) {
            let privacy_flags = ["--incognito", "-inprivate", "-private", "--private-window"];
            if privacy_flags.iter().any(|flag| cmd_line.contains(flag)) {
                return true;
            }
        }
    }

    false
}

// ==================== dHash ====================

type DHash = [u64; 4];

fn compute_dhash(img: &DynamicImage, hash_size: u32) -> DHash {
    let gray = img.to_luma8();
    let resized = image::imageops::resize(
        &gray,
        hash_size + 1,
        hash_size,
        image::imageops::FilterType::Triangle,
    );

    let mut hash = [0u64; 4];
    let mut bit_index = 0usize;

    for row in 0..hash_size {
        for col in 0..hash_size {
            let left = resized.get_pixel(col, row)[0];
            let right = resized.get_pixel(col + 1, row)[0];
            if left > right {
                let word = bit_index / 64;
                let bit = bit_index % 64;
                hash[word] |= 1u64 << bit;
            }
            bit_index += 1;
        }
    }
    hash
}

fn hamming_distance(a: &DHash, b: &DHash) -> u32 {
    let mut dist = 0u32;
    for i in 0..4 {
        dist += (a[i] ^ b[i]).count_ones();
    }
    dist
}

fn is_redundant(current: &DHash, history: &[DHash], threshold: u32) -> bool {
    for h in history {
        if hamming_distance(current, h) < threshold {
            return true;
        }
    }
    false
}

// ==================== Window Screenshot (DXGI Desktop Duplication) ====================

#[derive(Clone)]
struct CapturedImage {
    jpeg_bytes: Vec<u8>,
    width: u32,
    height: u32,
    dynamic_image: DynamicImage,
}

fn capture_foreground_window(
    hwnd_raw: isize,
    _rect: &RECT, // Not used strictly because WGC directly captures window
    max_side: u32,
    jpeg_quality: u8,
    wgc_state: &Mutex<Option<WgcCaptureSession>>,
) -> Option<CapturedImage> {
    unsafe {
        let mut session_guard = wgc_state.lock().unwrap_or_else(|e| e.into_inner());

        let need_create = match session_guard.as_ref() {
            Some(s) => {
                if s.hwnd != hwnd_raw {
                    true
                } else if let Ok(size) = s.item.Size() {
                    size.Width != s.current_size.Width || size.Height != s.current_size.Height
                } else {
                    true
                }
            },
            None => true,
        };

        if need_create {
            let reused_devices = session_guard.as_ref().map(|s| {
                (
                    s.d3d_device.clone(),
                    s.d3d_context.clone(),
                    s.winrt_device.clone(),
                )
            });

            if session_guard.is_some() {
                tracing::info!("WGC: window changed, recreating session");
            }

            // Explicitly drop previous session first so event handlers/pools are torn down.
            *session_guard = None;

            // 1. Reuse existing D3D device/context when possible.
            let (d3d_device, d3d_context, winrt_device) =
                if let Some((d3d_device, d3d_context, winrt_device)) = reused_devices {
                    (d3d_device, d3d_context, winrt_device)
                } else {
                    let mut d3d_device: Option<ID3D11Device> = None;
                    let mut d3d_context: Option<ID3D11DeviceContext> = None;
                    let mut feature_level = D3D_FEATURE_LEVEL_11_0;

                    let hr = D3D11CreateDevice(
                        None,
                        D3D_DRIVER_TYPE_HARDWARE,
                        None,
                        D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                        Some(&[D3D_FEATURE_LEVEL_11_0]),
                        D3D11_SDK_VERSION,
                        Some(&mut d3d_device),
                        Some(&mut feature_level),
                        Some(&mut d3d_context),
                    );

                    if hr.is_err() {
                        tracing::warn!("D3D11CreateDevice failed: {:?}", hr);
                        *session_guard = None;
                        return None;
                    }

                    let d3d_device = d3d_device.unwrap();
                    let d3d_context = d3d_context.unwrap();

                    let dxgi_device: IDXGIDevice = match d3d_device.cast() {
                        Ok(d) => d,
                        Err(e) => {
                            tracing::warn!("Failed to cast D3D11Device to DXGIDevice: {:?}", e);
                            *session_guard = None;
                            return None;
                        }
                    };

                    let inspectable = match CreateDirect3D11DeviceFromDXGIDevice(&dxgi_device) {
                        Ok(i) => i,
                        Err(e) => {
                            tracing::warn!("CreateDirect3D11DeviceFromDXGIDevice failed: {:?}", e);
                            *session_guard = None;
                            return None;
                        }
                    };

                    let winrt_device: IDirect3DDevice = match inspectable.cast() {
                        Ok(d) => d,
                        Err(e) => {
                            tracing::warn!("Failed to cast inspectable to IDirect3DDevice: {:?}", e);
                            *session_guard = None;
                            return None;
                        }
                    };

                    (d3d_device, d3d_context, winrt_device)
                };

            // 2. Create GraphicsCaptureItem
            let interop = match windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>() {
                Ok(i) => i,
                Err(e) => {
                    tracing::warn!("Failed to get IGraphicsCaptureItemInterop: {:?}", e);
                    *session_guard = None;
                    return None;
                }
            };

            let hwnd = HWND(hwnd_raw as *mut _);
            let item: GraphicsCaptureItem = match interop.CreateForWindow(hwnd) {
                Ok(i) => i,
                Err(e) => {
                    tracing::debug!("CreateForWindow failed for hwnd {:?}: {:?}", hwnd_raw, e);
                    *session_guard = None;
                    return None;
                }
            };

            let item_size = match item.Size() {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!("Failed to get item Size: {:?}", e);
                    *session_guard = None;
                    return None;
                }
            };

            if item_size.Width <= 0 || item_size.Height <= 0 {
                tracing::debug!("Target window size is 0x0, skipping capture");
                *session_guard = None;
                return None;
            }

            // 3. Create frame pool and session
            let frame_pool = match Direct3D11CaptureFramePool::CreateFreeThreaded(
                &winrt_device,
                DirectXPixelFormat::B8G8R8A8UIntNormalized,
                1,
                item_size,
            ) {
                Ok(p) => p,
                Err(e) => {
                    tracing::warn!("CreateFreeThreaded frame pool failed: {:?}", e);
                    *session_guard = None;
                    return None;
                }
            };

            let session = match frame_pool.CreateCaptureSession(&item) {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!("CreateCaptureSession failed: {:?}", e);
                    *session_guard = None;
                    return None;
                }
            };

            if let Err(e) = session.SetIsBorderRequired(false) {
                tracing::debug!("Failed to hide capture border (maybe older OS): {:?}", e);
            }
            if let Err(e) = session.SetIsCursorCaptureEnabled(false) {
                tracing::debug!("Failed to hide capture cursor: {:?}", e);
            }

            let (tx, rx) = sync_channel(1);
            let handler = TypedEventHandler::new(
                move |pool: &Option<Direct3D11CaptureFramePool>, _: &Option<IInspectable>| {
                    if let Some(pool) = pool {
                        if let Ok(frame) = pool.TryGetNextFrame() {
                            let _ = tx.try_send(frame);
                        }
                    }
                    Ok(())
                },
            );

            let frame_arrived_token = match frame_pool.FrameArrived(&handler) {
                Ok(token) => token,
                Err(e) => {
                    tracing::warn!("Failed to register FrameArrived event: {:?}", e);
                    *session_guard = None;
                    return None;
                }
            };

            if let Err(e) = session.StartCapture() {
                tracing::warn!("StartCapture failed: {:?}", e);
                let _ = frame_pool.RemoveFrameArrived(frame_arrived_token);
                *session_guard = None;
                return None;
            }

            *session_guard = Some(WgcCaptureSession {
                hwnd: hwnd_raw,
                session,
                frame_pool,
                frame_arrived_token: Some(frame_arrived_token),
                rx,
                d3d_device,
                d3d_context,
                winrt_device,
                item,
                current_size: item_size,
                last_image: None,
            });
        }

        let session = session_guard.as_mut().unwrap();

        // 4. Wait for a frame (up to 500ms)
        let frame = match session.rx.recv_timeout(std::time::Duration::from_millis(500)) {
            Ok(f) => f,
            Err(_) => {
                // Timeout means the window hasn't updated its content. Very common.
                // DXGI used to uniformly return the last desktop frame, so we replicate it
                // by returning the cached frame. This helps the fixed-interval polling correctly
                // trigger OCR retries if the scene hasn't visually changed.
                return session.last_image.clone();
            }
        };

        let content_size = match frame.ContentSize() {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("Failed to get frame ContentSize: {:?}", e);
                *session_guard = None;
                return None;
            }
        };

        let width = content_size.Width as u32;
        let height = content_size.Height as u32;

        if width == 0 || height == 0 {
            tracing::warn!("WGC frame has 0x0 resolution");
            *session_guard = None;
            return None;
        }

        let surface = match frame.Surface() {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("Failed to get frame Surface: {:?}", e);
                *session_guard = None;
                return None;
            }
        };

        let dxgi_interface: IDirect3DDxgiInterfaceAccess = match surface.cast() {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!("Failed to cast surface to IDirect3DDxgiInterfaceAccess: {:?}", e);
                *session_guard = None;
                return None;
            }
        };

        let source_texture: ID3D11Texture2D = match dxgi_interface.GetInterface() {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!("Failed to get ID3D11Texture2D: {:?}", e);
                *session_guard = None;
                return None;
            }
        };

        // 5. Create staging texture to read pixels to CPU
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        source_texture.GetDesc(&mut desc);

        // The canonical pattern to handle window resizing:
        // Verification that ContentSize matches our locally allocated texture bounds.
        // If the window grew or shrunk, ContentSize differs from the Surface (desc) dimension.
        if width != desc.Width || height != desc.Height {
            tracing::info!(
                "WGC: Window content size changed ({}x{} -> {}x{}), invalidating session to force recreation.",
                desc.Width,
                desc.Height,
                width,
                height
            );
            *session_guard = None;
            return None;
        }

        desc.Usage = D3D11_USAGE_STAGING;
        desc.BindFlags = 0;
        desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
        desc.MiscFlags = 0;

        let mut staging_texture: Option<ID3D11Texture2D> = None;
        if let Err(e) = session
            .d3d_device
            .CreateTexture2D(&desc, None, Some(&mut staging_texture))
        {
            tracing::warn!("Failed to create staging texture: {:?}", e);
            *session_guard = None;
            return None;
        }
        let staging_texture = staging_texture.unwrap();

        // Copy resource from GPU to staging
        session.d3d_context.CopyResource(&staging_texture, &source_texture);

        // 6. Map and extract pixels
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        if let Err(e) = session
            .d3d_context
            .Map(&staging_texture, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
        {
            tracing::warn!("Failed to map staging texture: {:?}", e);
            *session_guard = None;
            return None;
        }

        // WGC uses B8G8R8A8 normalized
        let row_pitch = mapped.RowPitch as usize;
        let mut rgb_pixels = Vec::with_capacity((width * height * 3) as usize);
        let raw = std::slice::from_raw_parts(mapped.pData as *const u8, row_pitch * height as usize);

        for row in 0..height {
            let row_start = (row as usize) * row_pitch;
            for col in 0..width {
                let offset = row_start + (col as usize) * 4;
                let b = raw[offset];
                let g = raw[offset + 1];
                let r = raw[offset + 2];
                rgb_pixels.push(r);
                rgb_pixels.push(g);
                rgb_pixels.push(b);
            }
        }

        session.d3d_context.Unmap(&staging_texture, 0);

        // 7. Create image and scale if needed
        let rgb_image = match RgbImage::from_raw(width, height, rgb_pixels) {
            Some(img) => img,
            None => {
                tracing::warn!("Failed to create RgbImage from WGC pixels");
                *session_guard = None;
                return None;
            }
        };

        let mut dynamic = DynamicImage::ImageRgb8(rgb_image);
        let max_dim = width.max(height);
        if max_dim > max_side {
            let ratio = max_side as f64 / max_dim as f64;
            let new_w = (width as f64 * ratio) as u32;
            let new_h = (height as f64 * ratio) as u32;
            dynamic = dynamic.resize(new_w, new_h, image::imageops::FilterType::Lanczos3);
        }

        let (final_w, final_h) = dynamic.dimensions();

        // 8. Encode as JPEG
        let mut jpeg_buf = Vec::new();
        {
            let mut encoder = JpegEncoder::new_with_quality(&mut jpeg_buf, jpeg_quality);
            if let Err(e) = encoder.encode_image(&dynamic) {
                tracing::warn!("JPEG encoding failed: {}", e);
                *session_guard = None;
                return None;
            }
        }

        let captured = CapturedImage {
            jpeg_bytes: jpeg_buf,
            width: final_w,
            height: final_h,
            dynamic_image: dynamic,
        };

        session.last_image = Some(captured.clone());
        Some(captured)
    }
}


// ==================== Process Icon Extraction ====================

fn extract_process_icon_base64(exe_path: &str) -> Option<String> {
    use windows::Win32::Graphics::Gdi::*;
    use windows::Win32::UI::Shell::ExtractIconExW;
    use windows::Win32::UI::WindowsAndMessaging::DestroyIcon;

    unsafe {
        // Convert path to wide string
        let wide_path: Vec<u16> = exe_path.encode_utf16().chain(std::iter::once(0)).collect();

        let mut icon_large = [windows::Win32::UI::WindowsAndMessaging::HICON::default(); 1];
        let mut icon_small = [windows::Win32::UI::WindowsAndMessaging::HICON::default(); 1];

        let count = ExtractIconExW(
            windows::core::PCWSTR(wide_path.as_ptr()),
            0,
            Some(icon_large.as_mut_ptr()),
            Some(icon_small.as_mut_ptr()),
            1,
        );

        if count == 0 {
            return None;
        }

        let hicon = if !icon_large[0].is_invalid() {
            icon_large[0]
        } else if !icon_small[0].is_invalid() {
            icon_small[0]
        } else {
            return None;
        };

        // Convert HICON to PNG base64
        let size: i32 = 32;
        let hdc_screen = GetDC(None);
        if hdc_screen.is_invalid() {
            let _ = DestroyIcon(hicon);
            return None;
        }

        let hdc_mem = CreateCompatibleDC(hdc_screen);
        if hdc_mem.is_invalid() {
            ReleaseDC(None, hdc_screen);
            let _ = DestroyIcon(hicon);
            return None;
        }

        let hbm = CreateCompatibleBitmap(hdc_screen, size, size);
        if hbm.is_invalid() {
            let _ = DeleteDC(hdc_mem);
            ReleaseDC(None, hdc_screen);
            let _ = DestroyIcon(hicon);
            return None;
        }

        let old_bm = SelectObject(hdc_mem, hbm);

        // Clear background to transparent
        let _ = PatBlt(hdc_mem, 0, 0, size, size, BLACKNESS);

        // Draw icon
        let draw_ok = windows::Win32::UI::WindowsAndMessaging::DrawIconEx(
            hdc_mem,
            0,
            0,
            hicon,
            size,
            size,
            0,
            None,
            windows::Win32::UI::WindowsAndMessaging::DI_NORMAL,
        );

        if draw_ok.is_err() {
            SelectObject(hdc_mem, old_bm);
            let _ = DeleteObject(hbm);
            let _ = DeleteDC(hdc_mem);
            ReleaseDC(None, hdc_screen);
            let _ = DestroyIcon(hicon);
            return None;
        }

        // Extract pixel data using GetDIBits
        let mut bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: size,
                biHeight: -size, // top-down
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };

        let mut pixels = vec![0u8; (size * size * 4) as usize];
        let got_bits = GetDIBits(
            hdc_mem,
            hbm,
            0,
            size as u32,
            Some(pixels.as_mut_ptr() as *mut _),
            &mut bmi,
            DIB_RGB_COLORS,
        );

        SelectObject(hdc_mem, old_bm);
        let _ = DeleteObject(hbm);
        let _ = DeleteDC(hdc_mem);
        ReleaseDC(None, hdc_screen);
        let _ = DestroyIcon(hicon);

        // Destroy the other icon if both were extracted
        if !icon_large[0].is_invalid() && !icon_small[0].is_invalid() {
            let _ = DestroyIcon(icon_small[0]);
        }

        if got_bits == 0 {
            return None;
        }

        // Convert BGRA to RGBA
        let mut rgba_pixels = vec![0u8; pixels.len()];
        for i in (0..pixels.len()).step_by(4) {
            rgba_pixels[i] = pixels[i + 2]; // R
            rgba_pixels[i + 1] = pixels[i + 1]; // G
            rgba_pixels[i + 2] = pixels[i]; // B
            rgba_pixels[i + 3] = pixels[i + 3]; // A
        }

        // Create PNG from RGBA pixels
        let img = image::RgbaImage::from_raw(size as u32, size as u32, rgba_pixels)?;
        let mut png_buf = Vec::new();
        let encoder = image::codecs::png::PngEncoder::new(&mut png_buf);
        if ImageEncoder::write_image(
            encoder,
            img.as_raw(),
            size as u32,
            size as u32,
            image::ExtendedColorType::Rgba8,
        )
        .is_err()
        {
            return None;
        }

        Some(base64::engine::general_purpose::STANDARD.encode(&png_buf))
    }
}

// ==================== Main Capture Loop ====================

/// Main loop that periodically captures screenshots of the active window,
/// deduplicates via dHash, and dispatches OCR tasks to the Python backend.
pub async fn run_capture_loop(
    capture_state: Arc<CaptureState>,
    storage: Arc<StorageState>,
    app: tauri::AppHandle,
) {
    tracing::info!("Rust capture loop started");

    let mut last_hwnd_raw: isize = 0;
    // Use checked_sub to avoid panic when system uptime < 999s (Instant can't go before boot)
    let mut last_capture_time = std::time::Instant::now()
        .checked_sub(std::time::Duration::from_secs(999))
        .unwrap_or(std::time::Instant::now());
    let mut force_first_capture = true;
    let mut history_hashes: Vec<DHash> = Vec::new();
    let mut icon_cache: HashMap<String, Option<String>> = HashMap::new();

    // Load config
    let (
        interval_secs,
        polling_rate_ms,
        max_side,
        jpeg_quality,
        dhash_threshold,
        dhash_history_size,
    ) = {
        let config = capture_state
            .config
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        (
            config.interval_secs,
            config.polling_rate_ms,
            config.max_side,
            config.jpeg_quality,
            config.dhash_threshold,
            config.dhash_history_size,
        )
    };

    let polling_duration = tokio::time::Duration::from_millis(polling_rate_ms);

    loop {
        tokio::time::sleep(polling_duration).await;

        // Check stop
        if capture_state.stopped.load(Ordering::SeqCst) {
            tracing::info!("Capture loop: stop signal received");
            break;
        }

        // Check pause
        if capture_state.paused.load(Ordering::SeqCst) {
            continue;
        }

        // Check game mode fullscreen pause
        if capture_state
            .game_mode_capture_paused
            .load(Ordering::SeqCst)
        {
            continue;
        }

        // Get active window
        let window_info = match get_active_window_info() {
            Some(info) => info,
            None => continue,
        };

        let current_hwnd_raw = window_info.hwnd_raw;

        // Exclusion check
        {
            let settings = capture_state
                .exclusion_settings
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if is_excluded(&window_info, &settings) {
                last_hwnd_raw = current_hwnd_raw;
                continue;
            }
        }

        // Backpressure check
        let in_flight = capture_state.in_flight_ocr_count.load(Ordering::SeqCst);
        let capture_on_busy = capture_state.capture_on_ocr_busy.load(Ordering::SeqCst);
        let max_queue = capture_state.ocr_queue_max_size.load(Ordering::SeqCst);

        let mut should_capture = false;
        let mut scan_reason = "";

        // Focus change detection
        if current_hwnd_raw != last_hwnd_raw {
            if !capture_on_busy {
                // Conservative: skip if any OCR in flight
                if in_flight == 0 {
                    should_capture = true;
                    scan_reason = "focus_change";
                }
            } else {
                // Relaxed: skip only if over max queue
                if in_flight <= max_queue {
                    should_capture = true;
                    scan_reason = "focus_change";
                }
            }
        }
        // Interval trigger
        else if force_first_capture || last_capture_time.elapsed().as_secs() >= interval_secs {
            if !capture_on_busy && in_flight > 0 {
                // Conservative: skip
            } else if in_flight > max_queue {
                // Over max queue: skip
            } else {
                should_capture = true;
                scan_reason = "interval";
            }
        }

        if !should_capture {
            last_hwnd_raw = current_hwnd_raw;
            continue;
        }

        force_first_capture = false;

        // Focus change: wait for window to stabilize
        if scan_reason == "focus_change" {
            tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

            // Re-check that we haven't been stopped/paused during the wait
            if capture_state.stopped.load(Ordering::SeqCst) {
                break;
            }
            if capture_state.paused.load(Ordering::SeqCst) {
                continue;
            }
            if capture_state
                .game_mode_capture_paused
                .load(Ordering::SeqCst)
            {
                continue;
            }
        }

        // Capture screenshot
        let captured = match capture_foreground_window(
            current_hwnd_raw,
            &window_info.rect,
            max_side,
            jpeg_quality,
            &capture_state.wgc_state,
        ) {
            Some(c) => c,
            None => {
                last_hwnd_raw = current_hwnd_raw;
                continue;
            }
        };

        // dHash dedup
        let current_hash = compute_dhash(&captured.dynamic_image, 16);
        if is_redundant(&current_hash, &history_hashes, dhash_threshold) {
            last_capture_time = std::time::Instant::now();
            last_hwnd_raw = current_hwnd_raw;
            continue;
        }

        // Update history
        history_hashes.push(current_hash);
        if history_hashes.len() > dhash_history_size {
            history_hashes.remove(0);
        }

        // Get process metadata
        let process_path = get_process_path_from_pid(window_info.pid).unwrap_or_default();
        let process_name = if !process_path.is_empty() {
            get_process_name_from_path(&process_path)
        } else {
            String::new()
        };

        // Request extension capture for browsers with extension enhancement enabled
        // The browser extension captures with richer metadata (URL, title, favicon, links)
        // If extension capture fails, fall through to normal capture path
        if crate::reverse_ipc::is_process_extension_enhanced(&process_name) {
            tracing::debug!("Requesting extension capture for: {}", process_name);
            match crate::reverse_ipc::request_extension_capture(&process_name).await {
                Ok(()) => {
                    // Extension capture request sent successfully, skip normal capture
                    last_capture_time = std::time::Instant::now();
                    last_hwnd_raw = current_hwnd_raw;
                    continue;
                }
                Err(e) => {
                    // Extension capture failed, fall back to normal capture path
                    tracing::warn!(
                        "Extension capture failed, falling back to normal capture: {}",
                        e
                    );
                }
            }
        }

        // Get/cache process icon
        let process_icon = if !process_path.is_empty() {
            if let Some(cached) = icon_cache.get(&process_path) {
                cached.clone()
            } else {
                let icon = extract_process_icon_base64(&process_path);
                icon_cache.insert(process_path.clone(), icon.clone());
                icon
            }
        } else {
            None
        };

        // Compute image hash (MD5)
        let image_hash = md5_hash(&captured.jpeg_bytes);

        let ts_str = chrono::Local::now().format("%Y%m%d_%H%M%S").to_string();

        let mut s = std::collections::hash_map::DefaultHasher::new();

        let window_title = &window_info.title.chars().take(50).collect::<String>();

        // Hash the combination of window title for identity and privacy protection
        std::hash::Hash::hash(&window_title, &mut s);

        let title_hash = std::hash::Hasher::finish(&s);

        tracing::info!(
            "[{}] Captured ({}): {} bytes, {}x{} - {} ({})",
            ts_str,
            scan_reason,
            captured.jpeg_bytes.len(),
            captured.width,
            captured.height,
            &title_hash,
            &process_name
        );

        // Build metadata
        let metadata = serde_json::json!({
            "monitor": {
                "left": window_info.rect.left,
                "top": window_info.rect.top,
                "width": window_info.rect.right - window_info.rect.left,
                "height": window_info.rect.bottom - window_info.rect.top,
            },
            "process_path": process_path,
            "process_icon": process_icon,
            "timestamp": ts_str,
        });

        // Save screenshot temp (directly, no IPC needed)
        let image_data_b64 = base64::engine::general_purpose::STANDARD.encode(&captured.jpeg_bytes);

        let save_request = SaveScreenshotRequest {
            image_data: image_data_b64.clone(),
            image_hash: image_hash.clone(),
            width: captured.width as i32,
            height: captured.height as i32,
            window_title: Some(window_info.title.clone()),
            process_name: Some(process_name.clone()),
            metadata: Some(metadata.clone()),
            ocr_results: None,
            source: Some("capture".to_string()),
            page_url: None,
            page_icon: None,
            visible_links: None,
        };

        let screenshot_id = match storage.save_screenshot_temp(&save_request) {
            Ok(resp) => {
                if resp.status == "duplicate" {
                    tracing::debug!("Duplicate screenshot, skipping OCR");
                    last_capture_time = std::time::Instant::now();
                    last_hwnd_raw = current_hwnd_raw;
                    continue;
                }
                match resp.screenshot_id {
                    Some(id) => id,
                    None => {
                        tracing::error!("save_screenshot_temp returned no ID");
                        last_capture_time = std::time::Instant::now();
                        last_hwnd_raw = current_hwnd_raw;
                        continue;
                    }
                }
            }
            Err(e) => {
                tracing::error!("save_screenshot_temp failed: {}", e);
                last_capture_time = std::time::Instant::now();
                last_hwnd_raw = current_hwnd_raw;
                continue;
            }
        };

        // Spawn async OCR task
        let storage_clone = storage.clone();
        let capture_state_clone = capture_state.clone();
        let jpeg_bytes = captured.jpeg_bytes.clone();
        let image_hash_clone = image_hash.clone();
        let window_title_clone = window_info.title.clone();
        let process_name_clone = process_name.clone();
        let timestamp_ms = chrono::Utc::now().timestamp_millis();
        let app_clone = app.clone();

        tokio::spawn(async move {
            let monitor_state = app_clone.state::<MonitorState>();
            process_ocr_async(
                &monitor_state,
                storage_clone,
                capture_state_clone,
                screenshot_id,
                jpeg_bytes,
                image_hash_clone,
                window_title_clone,
                process_name_clone,
                timestamp_ms,
            )
            .await;
        });

        last_capture_time = std::time::Instant::now();
        last_hwnd_raw = current_hwnd_raw;
    }

    capture_state.clear_wgc_session("capture_loop_ended");
    tracing::info!("Rust capture loop ended");
}

async fn process_ocr_async(
    monitor_state: &MonitorState,
    storage: Arc<StorageState>,
    capture_state: Arc<CaptureState>,
    screenshot_id: i64,
    jpeg_bytes: Vec<u8>,
    image_hash: String,
    window_title: String,
    process_name: String,
    timestamp_ms: i64,
) {
    const OCR_ASYNC_WARN_MS: u128 = 60_000;
    let in_flight_after_inc = capture_state
        .in_flight_ocr_count
        .fetch_add(1, Ordering::SeqCst)
        + 1;

    let task_started = std::time::Instant::now();
    tracing::debug!(
        "[DIAG:CAPTURE] process_ocr_async start screenshot_id={} in_flight={} process={}",
        screenshot_id,
        in_flight_after_inc,
        process_name
    );

    // Store JPEG bytes in in-memory cache so Python can fetch via get_temp_image
    // without triggering CNG decryption (Windows Hello PIN).
    {
        let mut cache = capture_state
            .ocr_image_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        cache.insert(screenshot_id, jpeg_bytes.clone());
    }

    let result = process_ocr_inner(
        monitor_state,
        &storage,
        screenshot_id,
        &jpeg_bytes,
        &image_hash,
        &window_title,
        &process_name,
        timestamp_ms,
    )
    .await;

    // Always remove from cache regardless of success/failure
    {
        let mut cache = capture_state
            .ocr_image_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        cache.remove(&screenshot_id);
    }

    if let Err(e) = result {
        tracing::error!(
            "OCR processing failed for screenshot {}: {}",
            screenshot_id,
            e
        );
        // Abort the pending screenshot
        if let Err(abort_err) = storage.abort_screenshot(screenshot_id, Some(&e)) {
            tracing::error!("abort_screenshot also failed: {}", abort_err);
        }
    }

    let in_flight_after_dec = capture_state
        .in_flight_ocr_count
        .fetch_sub(1, Ordering::SeqCst)
        .saturating_sub(1);

    let total_ms = task_started.elapsed().as_millis();
    if total_ms >= OCR_ASYNC_WARN_MS {
        tracing::warn!(
            "[DIAG:CAPTURE] process_ocr_async slow screenshot_id={} total={}ms in_flight_after={}",
            screenshot_id,
            total_ms,
            in_flight_after_dec
        );
    } else {
        tracing::debug!(
            "[DIAG:CAPTURE] process_ocr_async end screenshot_id={} total={}ms in_flight_after={}",
            screenshot_id,
            total_ms,
            in_flight_after_dec
        );
    }
}

async fn process_ocr_inner(
    monitor_state: &MonitorState,
    storage: &StorageState,
    screenshot_id: i64,
    _jpeg_bytes: &[u8],
    image_hash: &str,
    window_title: &str,
    process_name: &str,
    timestamp_ms: i64,
) -> Result<(), String> {
    const OCR_IPC_ROUNDTRIP_WARN_MS: u128 = 60_000;
    const COMMIT_SLOW_WARN_MS: u128 = 2_000;

    let cmd_started = std::time::Instant::now();

    // Get pipe info for sending to Python
    let pipe_name = {
        let guard = monitor_state
            .pipe_name
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        guard
            .clone()
            .ok_or_else(|| "Monitor pipe not available".to_string())?
    };

    // Send process_ocr command to Python with only screenshot_id (small payload).
    // Python will fetch the image via reverse IPC (get_temp_image) from in-memory cache.
    let req = serde_json::json!({
        "command": "process_ocr",
        "screenshot_id": screenshot_id,
        "image_hash": image_hash,
        "window_title": window_title,
        "process_name": process_name,
        "timestamp": timestamp_ms,
    });

    let (auth_token, seq_no) = {
        let token = monitor_state.auth_token.lock().unwrap_or_else(|e| e.into_inner()).clone()
            .ok_or_else(|| "Auth token not available".to_string())?;
        let seq = monitor_state.request_counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        (token, seq)
    };

    tracing::debug!(
        "[DIAG:CAPTURE] process_ocr IPC send screenshot_id={} seq_no={} pipe={}",
        screenshot_id,
        seq_no,
        pipe_name
    );

    let ipc_started = std::time::Instant::now();
    let response = crate::monitor::send_ipc_request(&pipe_name, &auth_token, seq_no, req).await?;

    let ipc_ms = ipc_started.elapsed().as_millis();
    // NOTE: This is IPC roundtrip time and includes Python OCR inference + post-processing.
    // Use a high threshold to avoid mislabeling normal OCR cost as transport slowness.
    if ipc_ms >= OCR_IPC_ROUNDTRIP_WARN_MS {
        tracing::warn!(
            "[DIAG:CAPTURE] process_ocr IPC roundtrip very slow screenshot_id={} seq_no={} elapsed={}ms",
            screenshot_id,
            seq_no,
            ipc_ms
        );
    } else {
        tracing::debug!(
            "[DIAG:CAPTURE] process_ocr IPC recv screenshot_id={} seq_no={} elapsed={}ms",
            screenshot_id,
            seq_no,
            ipc_ms
        );
    }

    // Check response
    if let Some(error) = response.get("error").and_then(|v| v.as_str()) {
        return Err(format!("Python OCR error: {}", error));
    }

    // Extract OCR results from response
    let ocr_results: Option<Vec<OcrResultInput>> = response
        .get("ocr_results")
        .and_then(|v| serde_json::from_value(v.clone()).ok());

    // Extract category from Python response
    let category = response
        .get("category")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let category_confidence = response.get("category_confidence").and_then(|v| v.as_f64());

    // Commit screenshot with OCR results and category
    let commit_started = std::time::Instant::now();
    tracing::debug!(
        "[DIAG:CAPTURE] commit_screenshot start screenshot_id={} ocr_results={} category={}",
        screenshot_id,
        ocr_results.as_ref().map(|r| r.len()).unwrap_or(0),
        category.as_deref().unwrap_or("")
    );

    storage.commit_screenshot(
        screenshot_id,
        ocr_results.as_ref(),
        category.as_deref(),
        category_confidence,
    )?;

    let commit_ms = commit_started.elapsed().as_millis();
    let total_ms = cmd_started.elapsed().as_millis();
    if commit_ms >= COMMIT_SLOW_WARN_MS {
        tracing::warn!(
            "[DIAG:CAPTURE] commit_screenshot slow screenshot_id={} commit={}ms total={}ms",
            screenshot_id,
            commit_ms,
            total_ms
        );
    } else {
        tracing::debug!(
            "[DIAG:CAPTURE] commit_screenshot done screenshot_id={} commit={}ms total={}ms",
            screenshot_id,
            commit_ms,
            total_ms
        );
    }

    tracing::debug!(
        "Screenshot {} committed with {} OCR results",
        screenshot_id,
        ocr_results.as_ref().map(|r| r.len()).unwrap_or(0)
    );

    Ok(())
}

// ==================== Utility ====================

fn md5_hash(data: &[u8]) -> String {
    use md5::{Digest, Md5};
    let mut hasher = Md5::new();
    hasher.update(data);
    let result = hasher.finalize();
    hex::encode(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hamming_distance_identical() {
        let hash = [0u64; 4];
        assert_eq!(hamming_distance(&hash, &hash), 0);
    }

    #[test]
    fn test_hamming_distance_all_different() {
        let a = [0u64; 4];
        let b = [u64::MAX; 4];
        assert_eq!(hamming_distance(&a, &b), 256);
    }

    #[test]
    fn test_hamming_distance_one_bit() {
        let a = [0u64; 4];
        let mut b = [0u64; 4];
        b[0] = 1;
        assert_eq!(hamming_distance(&a, &b), 1);
    }

    #[test]
    fn test_get_process_name_from_path_exe() {
        // get_process_name_from_path returns lowercase
        assert_eq!(
            get_process_name_from_path(r"C:\Program Files\app.exe"),
            "app.exe"
        );
    }

    #[test]
    fn test_get_process_name_from_path_empty() {
        assert_eq!(get_process_name_from_path(""), "");
    }

    #[test]
    fn test_get_process_name_from_path_no_dir() {
        assert_eq!(get_process_name_from_path("notepad.exe"), "notepad.exe");
    }

    #[test]
    fn test_get_process_name_from_path_mixed_case() {
        assert_eq!(
            get_process_name_from_path(r"C:\Windows\System32\Notepad.EXE"),
            "notepad.exe"
        );
    }

    #[test]
    fn test_md5_hash_known() {
        assert_eq!(md5_hash(b"hello"), "5d41402abc4b2a76b9719d911017c592");
    }

    #[test]
    fn test_md5_hash_empty() {
        assert_eq!(md5_hash(b""), "d41d8cd98f00b204e9800998ecf8427e");
    }

    #[test]
    fn test_compute_dhash_uniform_image() {
        // A uniform white image should produce all-zero hash (no gradient differences)
        let img =
            DynamicImage::ImageRgb8(RgbImage::from_pixel(16, 16, image::Rgb([255, 255, 255])));
        let hash = compute_dhash(&img, 8);
        assert_eq!(hash, [0u64; 4]);
    }

    #[test]
    fn test_compute_dhash_uniform_black() {
        let img = DynamicImage::ImageRgb8(RgbImage::from_pixel(16, 16, image::Rgb([0, 0, 0])));
        let hash = compute_dhash(&img, 8);
        assert_eq!(hash, [0u64; 4]);
    }

    #[test]
    fn test_is_redundant_empty_history() {
        let hash = [0u64; 4];
        assert!(!is_redundant(&hash, &[], 10));
    }

    #[test]
    fn test_is_redundant_identical() {
        let hash = [0u64; 4];
        assert!(is_redundant(&hash, &[hash], 10));
    }

    #[test]
    fn test_is_redundant_above_threshold() {
        let a = [0u64; 4];
        let b = [u64::MAX; 4]; // distance = 256
                               // threshold=10: distance(256) >= threshold(10) so NOT redundant
        assert!(!is_redundant(&a, &[b], 10));
    }
}
