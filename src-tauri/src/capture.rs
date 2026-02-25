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
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_FORMAT, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetForegroundWindow, GetWindowDisplayAffinity, GetWindowRect, GetWindowTextW,
    GetWindowThreadProcessId,
};

// ==================== Configuration ====================

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

// ==================== Capture State ====================

/// In-memory cache for JPEG bytes awaiting OCR.
/// Keyed by screenshot_id. Entries are inserted before sending to Python
/// and removed after OCR completes (commit or abort). This avoids reading
/// from encrypted storage (which triggers Windows Hello CNG decryption).
pub type OcrImageCache = Arc<Mutex<HashMap<i64, Vec<u8>>>>;

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
    pub focus_window: Mutex<Option<ActiveWindowInfo>>,
}

impl Default for CaptureState {
    fn default() -> Self {
        Self::new()
    }
}

impl CaptureState {
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
            focus_window: Mutex::new(None),
        }
    }

    pub fn load_exclusion_settings(&self, data_dir: &std::path::Path) {
        let path = data_dir.join("monitor_filters.json");
        if let Ok(content) = std::fs::read_to_string(&path) {
            if let Ok(data) = serde_json::from_str::<serde_json::Value>(&content) {
                let mut settings = self.exclusion_settings.lock().unwrap();
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
                if let Some(ignore_protected) = data.get("ignore_protected").and_then(|v| v.as_bool()) {
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

    pub fn save_exclusion_settings(&self, data_dir: &std::path::Path) {
        let settings = self.exclusion_settings.lock().unwrap();
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

    pub fn update_exclusion_settings(
        &self,
        processes: Option<Vec<String>>,
        titles: Option<Vec<String>>,
        ignore_protected: Option<bool>,
    ) {
        let mut settings = self.exclusion_settings.lock().unwrap();
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

pub struct ActiveWindowInfo {
    hwnd_raw: isize,
    title: String,
    rect: RECT,
    pid: u32,
}

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

pub fn get_process_name_from_path(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}

fn get_process_command_line(pid: u32) -> Option<String> {
    use sysinfo::{Pid, System, ProcessRefreshKind, UpdateKind};
    let mut sys = System::new();
    sys.refresh_processes_specifics(
        ProcessRefreshKind::new().with_cmd(UpdateKind::Always),
    );
    sys.process(Pid::from_u32(pid))
        .and_then(|p| {
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

// ==================== Window Screenshot (xcap) ====================

struct CapturedImage {
    jpeg_bytes: Vec<u8>,
    width: u32,
    height: u32,
    dynamic_image: DynamicImage,
}

fn capture_foreground_window(
    hwnd_raw: isize,
    _rect: &RECT,
    max_side: u32,
    jpeg_quality: u8,
) -> Option<CapturedImage> {
    // Use xcap to capture the specific window
    let windows = match xcap::Window::all() {
        Ok(w) => w,
        Err(e) => {
            tracing::warn!("xcap::Window::all() failed: {}", e);
            return None;
        }
    };

    // Find the window matching our HWND
    let target = windows.into_iter().find(|w| {
        w.id().ok() == Some(hwnd_raw as u32)
    });

    let window = match target {
        Some(w) => w,
        None => {
            tracing::debug!("xcap: window HWND {} not found in window list", hwnd_raw);
            return None;
        }
    };

    // Capture the window image
    let rgba_image = match window.capture_image() {
        Ok(img) => img,
        Err(e) => {
            tracing::debug!("xcap: capture_image failed: {}", e);
            return None;
        }
    };

    // Convert RGBA to RGB (JPEG doesn't support alpha)
    let (w, h) = rgba_image.dimensions();
    if w == 0 || h == 0 {
        return None;
    }

    let rgb_image: RgbImage = RgbImage::from_fn(w, h, |x, y| {
        let pixel = rgba_image.get_pixel(x, y);
        image::Rgb([pixel[0], pixel[1], pixel[2]])
    });

    let mut dynamic = DynamicImage::ImageRgb8(rgb_image);

    // Scale down if needed
    let max_dim = w.max(h);
    if max_dim > max_side {
        let ratio = max_side as f64 / max_dim as f64;
        let new_w = (w as f64 * ratio) as u32;
        let new_h = (h as f64 * ratio) as u32;
        dynamic = dynamic.resize(new_w, new_h, image::imageops::FilterType::Lanczos3);
    }

    let (final_w, final_h) = dynamic.dimensions();

    // Encode as JPEG
    let mut jpeg_buf = Vec::new();
    {
        let mut encoder = JpegEncoder::new_with_quality(&mut jpeg_buf, jpeg_quality);
        if let Err(e) = encoder.encode_image(&dynamic) {
            tracing::warn!("JPEG encoding failed: {}", e);
            return None;
        }
    }

    Some(CapturedImage {
        jpeg_bytes: jpeg_buf,
        width: final_w,
        height: final_h,
        dynamic_image: dynamic,
    })
}

// ==================== Process Icon Extraction ====================

fn extract_process_icon_base64(exe_path: &str) -> Option<String> {
    use windows::Win32::UI::Shell::ExtractIconExW;
    use windows::Win32::UI::WindowsAndMessaging::DestroyIcon;
    use windows::Win32::Graphics::Gdi::*;

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
                biCompression: BI_RGB.0 as u32,
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

pub async fn run_capture_loop(
    capture_state: Arc<CaptureState>,
    storage: Arc<StorageState>,
    app: tauri::AppHandle,
) {
    tracing::info!("Rust capture loop started");

    let mut last_hwnd_raw: isize = 0;
    let mut last_capture_time = std::time::Instant::now() - std::time::Duration::from_secs(999);
    let mut history_hashes: Vec<DHash> = Vec::new();
    let mut icon_cache: HashMap<String, Option<String>> = HashMap::new();

    // Load config
    let (interval_secs, polling_rate_ms, max_side, jpeg_quality, dhash_threshold, dhash_history_size) = {
        let config = capture_state.config.lock().unwrap();
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

        // Get active window
        let window_info = match get_active_window_info() {
            Some(info) => info,
            None => continue,
        };

        let current_hwnd_raw = window_info.hwnd_raw;

        // Exclusion check
        {
            let settings = capture_state.exclusion_settings.lock().unwrap();
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
        else if last_capture_time.elapsed().as_secs() >= interval_secs {
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
        }

        // Capture screenshot
        let captured = match capture_foreground_window(
            current_hwnd_raw,
            &window_info.rect,
            max_side,
            jpeg_quality,
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
                    tracing::warn!("Extension capture failed, falling back to normal capture: {}", e);
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
        let image_data_b64 =
            base64::engine::general_purpose::STANDARD.encode(&captured.jpeg_bytes);

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
    capture_state.in_flight_ocr_count.fetch_add(1, Ordering::SeqCst);

    // Store JPEG bytes in in-memory cache so Python can fetch via get_temp_image
    // without triggering CNG decryption (Windows Hello PIN).
    {
        let mut cache = capture_state.ocr_image_cache.lock().unwrap();
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
        let mut cache = capture_state.ocr_image_cache.lock().unwrap();
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

    capture_state.in_flight_ocr_count.fetch_sub(1, Ordering::SeqCst);
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
    // Get pipe info for sending to Python
    let pipe_name = {
        let guard = monitor_state.pipe_name.lock().unwrap();
        guard.clone().ok_or_else(|| "Monitor pipe not available".to_string())?
    };
    let auth_token = {
        let guard = monitor_state.auth_token.lock().unwrap();
        guard.clone().ok_or_else(|| "Auth token not available".to_string())?
    };
    let seq_no = monitor_state.request_counter.fetch_add(1, Ordering::SeqCst);

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

    let response = crate::monitor::send_ipc_request(&pipe_name, &auth_token, seq_no, req).await?;

    // Check response
    if let Some(error) = response.get("error").and_then(|v| v.as_str()) {
        return Err(format!("Python OCR error: {}", error));
    }

    // Extract OCR results from response
    let ocr_results: Option<Vec<OcrResultInput>> = response
        .get("ocr_results")
        .and_then(|v| serde_json::from_value(v.clone()).ok());

    // Commit screenshot with OCR results
    storage.commit_screenshot(screenshot_id, ocr_results.as_ref())?;

    tracing::debug!(
        "Screenshot {} committed with {} OCR results",
        screenshot_id,
        ocr_results.as_ref().map(|r| r.len()).unwrap_or(0)
    );

    Ok(())
}

// ==================== Utility ====================

fn md5_hash(data: &[u8]) -> String {
    use md5::{Md5, Digest};
    let mut hasher = Md5::new();
    hasher.update(data);
    let result = hasher.finalize();
    hex::encode(result)
}
