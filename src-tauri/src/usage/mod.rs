//! Usage tracking is independent of screenshot/OCR admission. One native MTA
//! thread owns all sampling and batches compact interval checkpoints.
pub(crate) mod aggregate;
mod input;
mod picture;
mod recorder;
mod sampler;
pub(crate) mod types;

use crate::{
    calendar,
    capture::{self, ActiveWindowInfo, CaptureState},
    monitor::MonitorState,
    storage::{StorageState, UsageWriter},
};
use input::Gamepads;
use picture::PictureSignals;
use recorder::Recorder;
pub(crate) use sampler::frame_observed_at;
use sampler::SceneSampler;
use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Condvar, Mutex,
    },
    thread,
    time::{Duration, Instant},
};
use tauri::{AppHandle, Manager};
use types::*;

const IDLE_THRESHOLD_MS: u64 = 180_000;
const CHECKPOINT_INTERVAL: Duration = Duration::from_secs(30);

struct BrowserPage {
    nmh_pid: u32,
    browser_pid: u32,
    title: String,
    url: Option<String>,
    private: bool,
    at: Instant,
}

#[derive(Default)]
pub struct UsageRuntime {
    live: Mutex<LiveUsage>,
    pictures: Mutex<PictureSignals>,
    pages: Mutex<VecDeque<BrowserPage>>,
    started: AtomicBool,
    shutdown: AtomicBool,
    wake: (Mutex<()>, Condvar),
    thread: Mutex<Option<thread::JoinHandle<()>>>,
}
impl UsageRuntime {
    pub(crate) fn snapshot(&self) -> LiveUsage {
        self.live.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
    pub(crate) fn observe_normal(
        &self,
        hwnd: isize,
        pid: u32,
        image: &image::RgbImage,
        at: Instant,
    ) {
        if capture::get_active_window_info()
            .is_some_and(|info| info.hwnd_raw == hwnd && info.pid == pid)
        {
            self.pictures
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .observe(hwnd, pid, picture::rgb_grid(image), true, at);
        }
    }

    /// Called only after native pipe peer, token, ancestry and session checks.
    /// Browser APIs do not expose HWNDs, so the reader also matches the actual
    /// foreground PID and window title before associating a website with time.
    pub(crate) fn browser_metadata(
        &self,
        nmh_pid: u32,
        browser_pid: u32,
        data: &serde_json::Value,
    ) -> Result<(), String> {
        let mut pages = self.pages.lock().unwrap_or_else(|e| e.into_inner());
        pages.retain(|p| p.nmh_pid != nmh_pid && p.at.elapsed() < Duration::from_secs(45));
        if data.get("focused").and_then(|v| v.as_bool()) != Some(true) {
            return Ok(());
        }
        let private = data
            .get("private")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let title = data.get("title").and_then(|v| v.as_str()).unwrap_or("");
        if title.len() > 4096 {
            return Err("Browser metadata title is too long".into());
        }
        let url = data
            .get("url")
            .and_then(|v| v.as_str())
            .filter(|s| s.len() <= 8192)
            .and_then(|s| url::Url::parse(s).ok())
            .filter(|u| matches!(u.scheme(), "http" | "https"))
            .map(|mut u| {
                let _ = u.set_username("");
                let _ = u.set_password(None);
                u.set_fragment(None);
                u.to_string()
            });
        if !private && (title.trim().is_empty() || url.is_none()) {
            return Ok(());
        }
        pages.push_back(BrowserPage {
            nmh_pid,
            browser_pid,
            title: if private {
                String::new()
            } else {
                title.chars().take(512).collect()
            },
            url: if private { None } else { url },
            private,
            at: Instant::now(),
        });
        while pages.len() > 16 {
            pages.pop_front();
        }
        Ok(())
    }
    pub(crate) fn clear_browser(&self, nmh_pid: u32) {
        self.pages
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|p| p.nmh_pid != nmh_pid);
    }
    fn page_for_window(&self, info: &ActiveWindowInfo) -> (Option<String>, bool) {
        self.page_for_foreground(info.pid, &info.title)
    }
    fn page_for_foreground(&self, pid: u32, title: &str) -> (Option<String>, bool) {
        let mut pages = self.pages.lock().unwrap_or_else(|e| e.into_inner());
        pages.retain(|p| p.at.elapsed() < Duration::from_secs(45));
        pages
            .iter()
            .rev()
            .find(|p| p.browser_pid == pid && (p.private || title_matches_page(title, &p.title)))
            .map(|p| (p.url.clone(), p.private))
            .unwrap_or((None, false))
    }
    fn wait(&self, duration: Duration) {
        let guard = self.wake.0.lock().unwrap_or_else(|e| e.into_inner());
        if !self.shutdown.load(Ordering::Acquire) {
            drop(
                self.wake
                    .1
                    .wait_timeout(guard, duration)
                    .unwrap_or_else(|e| e.into_inner()),
            );
        }
    }
    pub(crate) fn stop(&self) {
        self.shutdown.store(true, Ordering::Release);
        self.wake.1.notify_all();
        if let Some(thread) = self.thread.lock().unwrap_or_else(|e| e.into_inner()).take() {
            let _ = thread.join();
        }
        self.pages.lock().unwrap_or_else(|e| e.into_inner()).clear();
        self.pictures
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .reset();
    }
}

pub(crate) fn start(app: AppHandle) {
    let state = app.state::<Arc<UsageRuntime>>().inner().clone();
    if state.started.swap(true, Ordering::AcqRel) {
        return;
    }
    let runtime = state.clone();
    match thread::Builder::new()
        .name("usage-recorder".into())
        .spawn(move || {
            use windows::Win32::System::WinRT::{
                RoInitialize, RoUninitialize, RO_INIT_MULTITHREADED,
            };
            // SAFETY: this dedicated thread initializes/uninitializes its own MTA;
            // all native capture/controller objects are dropped before uninitialize.
            let initialized = unsafe { RoInitialize(RO_INIT_MULTITHREADED).is_ok() };
            if initialized {
                run(&app, &runtime);
                unsafe {
                    RoUninitialize();
                }
            } else {
                tracing::warn!("[USAGE] Native activity thread could not initialize");
            }
        }) {
        Ok(thread) => *state.thread.lock().unwrap_or_else(|e| e.into_inner()) = Some(thread),
        Err(error) => tracing::warn!("[USAGE] Could not start recorder: {error}"),
    }
}

struct ProcessCache {
    pid: u32,
    hwnd: isize,
    path: String,
    app: Arc<UsageAppIdentity>,
    private: bool,
    checked: Instant,
}
fn process_for_window(
    info: &ActiveWindowInfo,
    cache: &mut VecDeque<ProcessCache>,
) -> Option<(Arc<UsageAppIdentity>, bool)> {
    if let Some(entry) = cache.iter().find(|p| {
        p.pid == info.pid && p.hwnd == info.hwnd_raw && p.checked.elapsed() < Duration::from_secs(5)
    }) {
        return Some((entry.app.clone(), entry.private));
    }
    let path = capture::get_process_path_from_pid(info.pid)?;
    let name = capture::get_process_name_from_path(&path);
    if name.is_empty() {
        return None;
    }
    let previous = cache
        .iter()
        .find(|p| p.path.eq_ignore_ascii_case(&path))
        .map(|p| p.app.clone());
    let app = previous.unwrap_or_else(|| {
        Arc::new(UsageAppIdentity {
            process_name: name.clone(),
            name: application_name(&name),
            process_path: path.clone(),
            icon: capture::extract_process_icon_base64(&path),
        })
    });
    let private = capture::is_browser_process(&name) && capture::process_has_privacy_flag(info.pid);
    cache.retain(|p| p.pid != info.pid);
    cache.push_back(ProcessCache {
        pid: info.pid,
        hwnd: info.hwnd_raw,
        path,
        app: app.clone(),
        private,
        checked: Instant::now(),
    });
    while cache.len() > 64 {
        cache.pop_front();
    }
    Some((app, private))
}
fn application_name(process: &str) -> String {
    match process {
        "code.exe" => "Visual Studio Code",
        "msedge.exe" => "Microsoft Edge",
        "chrome.exe" => "Google Chrome",
        "firefox.exe" => "Firefox",
        "windowsterminal.exe" => "Windows Terminal",
        "endfield.exe" => "Endfield",
        "winword.exe" => "Microsoft Word",
        "excel.exe" => "Microsoft Excel",
        "powerpnt.exe" => "Microsoft PowerPoint",
        "obsidian.exe" => "Obsidian",
        "carbonpaper.exe" => "CarbonPaper",
        _ => return process.strip_suffix(".exe").unwrap_or(process).to_owned(),
    }
    .into()
}

fn run(app: &AppHandle, runtime: &UsageRuntime) {
    let storage = app.state::<Arc<StorageState>>().inner().clone();
    let capture = app.state::<Arc<CaptureState>>().inner().clone();
    let Ok(zone) = calendar::time_zone(None) else {
        tracing::warn!("[USAGE] System time zone is unavailable");
        return;
    };
    let mut generation = storage.db_generation();
    let mut recorder = Recorder::new();
    let mut writer = UsageWriter::new(generation, zone);
    let mut pads = Gamepads::new();
    let mut processes = VecDeque::new();
    let mut sampler: Option<SceneSampler> = None;
    let mut next_probe = Instant::now();
    let mut checkpoint_at = Instant::now();
    let mut last_target = None;
    let mut other_activity: Option<Instant> = None;
    let mut last_error_at = Instant::now()
        .checked_sub(Duration::from_secs(60))
        .unwrap_or_else(Instant::now);
    let mut last_observation_at = Instant::now();
    let mut filters_ready = false;
    let mut filters_retry_at = Instant::now();
    while !runtime.shutdown.load(Ordering::Acquire)
        && !crate::IS_QUITTING.load(Ordering::Relaxed)
        && !crate::IS_UPDATING.load(Ordering::Relaxed)
    {
        let now = chrono::Utc::now().timestamp_millis();
        if generation != storage.db_generation() {
            generation = storage.db_generation();
            recorder = Recorder::new();
            writer = UsageWriter::new(generation, zone);
            processes.clear();
            filters_ready = false;
            filters_retry_at = Instant::now();
            sampler = None;
            last_target = None;
            other_activity = None;
            runtime
                .pictures
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .reset();
        }
        if last_observation_at.elapsed().as_millis() > recorder::MAX_OBSERVATION_GAP_MS as u128 {
            sampler = None;
            last_target = None;
            other_activity = None;
            runtime
                .pictures
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .reset();
        }
        last_observation_at = Instant::now();
        let ready = storage.is_initialized() && crate::storage_startup::is_ready(app);
        let maintenance =
            crate::maintenance::is_active() || storage.is_database_maintenance_pending();
        let intended = app.state::<MonitorState>().usage_recording_intended()
            && !crate::onboarding::capture_held();
        if ready && intended && !maintenance && !filters_ready && Instant::now() >= filters_retry_at
        {
            let directory = storage
                .data_dir
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            filters_ready = capture.ensure_exclusion_settings_loaded(&directory).is_ok();
            filters_retry_at = Instant::now() + Duration::from_secs(30);
        }
        let mut state = UsageState::Unknown;
        let mut identity = None;
        let mut idle_boundary = None;
        let mut observing_window = false;
        let battery = !crate::power::is_ac_power_connected();
        if ready && !maintenance && intended && filters_ready {
            if !input::interactive_desktop() {
                state = UsageState::Idle;
            } else if let Some(info) = capture::get_active_window_info() {
                if let Some((process, private)) = process_for_window(&info, &mut processes) {
                    let (url, private_page) = if capture::is_browser_process(&process.process_name)
                    {
                        runtime.page_for_window(&info)
                    } else {
                        (None, false)
                    };
                    let excluded = private
                        || private_page
                        || crate::tray_panel::is_panel_window(app, info.hwnd_raw)
                        || capture::usage_window_excluded(
                            &info,
                            &capture
                                .exclusion_settings
                                .lock()
                                .unwrap_or_else(|e| e.into_inner()),
                            &process.process_name,
                        );
                    if excluded {
                        state = UsageState::Paused;
                    } else {
                        observing_window = true;
                        let target = (info.hwnd_raw, info.pid);
                        if last_target.is_some_and(|old| old != target) {
                            other_activity = Some(Instant::now());
                        }
                        last_target = Some(target);
                        if pads.activity() {
                            other_activity = Some(Instant::now());
                        }
                        let input_age = crate::idle::input_idle_ms();
                        let input_recent = activity_age(
                            input_age,
                            other_activity.map(|at| at.elapsed().as_millis() as u64),
                            None,
                        )
                        .is_some_and(|age| age < 15_000);
                        let normal_recent = runtime
                            .pictures
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .normal_is_recent(target.0, target.1);
                        let interval = if battery
                            || capture.game_mode_capture_paused.load(Ordering::Relaxed)
                        {
                            10
                        } else {
                            5
                        };
                        if sampler.as_ref().is_some_and(|s| {
                            s.target != target || s.interval_secs != interval || s.expired()
                        }) {
                            sampler = None;
                            next_probe = Instant::now();
                        }
                        // Initial input activity needs no picture probe. Once
                        // throttled capture exists, retain it on intermittent
                        // input to avoid repeatedly allocating a D3D device.
                        if normal_recent || (input_recent && sampler.is_none()) {
                            sampler = None;
                            next_probe = Instant::now() + Duration::from_secs(1);
                        } else {
                            if sampler.is_none() && Instant::now() >= next_probe {
                                match SceneSampler::new(target, interval) {
                                    Ok(created) => sampler = Some(created),
                                    Err(error) => {
                                        if last_error_at.elapsed() >= Duration::from_secs(60) {
                                            tracing::debug!(
                                                "[USAGE] Picture probe unavailable: {}",
                                                error.code()
                                            );
                                            last_error_at = Instant::now();
                                        }
                                        next_probe = Instant::now() + Duration::from_secs(30);
                                    }
                                }
                            }
                            if let Some(probe) = sampler.as_mut() {
                                let burst = !probe.throttled;
                                let wait = if burst {
                                    Duration::from_millis(200)
                                } else {
                                    Duration::ZERO
                                };
                                match probe.poll(wait) {
                                    Ok(Some((grid, at))) => {
                                        if capture::get_active_window_info().is_some_and(
                                            |current| {
                                                current.hwnd_raw == target.0
                                                    && current.pid == target.1
                                            },
                                        ) {
                                            runtime
                                                .pictures
                                                .lock()
                                                .unwrap_or_else(|e| e.into_inner())
                                                .observe(target.0, target.1, grid, false, at);
                                        }
                                    }
                                    Ok(None) => {}
                                    Err(_) => {
                                        sampler = None;
                                        next_probe = Instant::now() + Duration::from_secs(interval);
                                    }
                                }
                                if burst {
                                    sampler = None;
                                    next_probe = Instant::now() + Duration::from_secs(10);
                                }
                            }
                        }
                        let picture_age = runtime
                            .pictures
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .activity_age_ms(target.0, target.1);
                        let age = activity_age(
                            input_age,
                            other_activity.map(|at| at.elapsed().as_millis() as u64),
                            picture_age,
                        );
                        state = match age {
                            Some(age) if age >= IDLE_THRESHOLD_MS => UsageState::Idle,
                            Some(_) => UsageState::Active,
                            None => UsageState::Unknown,
                        };
                        if state == UsageState::Idle {
                            idle_boundary = age.map(|age| {
                                now - (age - IDLE_THRESHOLD_MS).min(i64::MAX as u64) as i64
                            });
                        }
                        if state == UsageState::Active {
                            let category = crate::classification::scoring::process_category(
                                &process.process_name,
                            )
                            .unwrap_or_else(|| {
                                if capture::is_browser_process(&process.process_name) {
                                    "网页浏览"
                                } else {
                                    "未分类"
                                }
                            });
                            identity = Some(Arc::new(UsageIdentity {
                                app: process,
                                title: info.title,
                                url,
                                category: category.into(),
                            }));
                        }
                    }
                }
            }
        } else if ready && !maintenance && !intended {
            state = UsageState::Paused;
        }
        if !observing_window || !intended || !ready || maintenance {
            sampler = None;
            last_target = None;
            other_activity = None;
            runtime
                .pictures
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .reset();
        }
        if ready && !maintenance {
            recorder.observe(now, state, identity, idle_boundary);
        } else {
            recorder.finish();
        }
        let spans = recorder.snapshot();
        *runtime.live.lock().unwrap_or_else(|e| e.into_inner()) = LiveUsage {
            generation,
            time_zone: Some(zone.name().into()),
            spans: spans.clone(),
        };
        if ready && !maintenance && checkpoint_at.elapsed() >= CHECKPOINT_INTERVAL {
            match writer.write(&storage, &spans) {
                Ok(()) => {
                    recorder.committed();
                    checkpoint_at = Instant::now();
                }
                Err(error) => {
                    if last_error_at.elapsed() >= Duration::from_secs(60) {
                        tracing::warn!("[USAGE] Checkpoint deferred: {error}");
                        last_error_at = Instant::now();
                    }
                    checkpoint_at = Instant::now()
                        .checked_sub(Duration::from_secs(25))
                        .unwrap_or_else(Instant::now);
                }
            }
        }
        let next_observation = Instant::now() + Duration::from_secs(if battery { 2 } else { 1 });
        while Instant::now() < next_observation && !runtime.shutdown.load(Ordering::Acquire) {
            let remaining = next_observation.saturating_duration_since(Instant::now());
            let polling_pad = observing_window && pads.connected();
            runtime.wait(if polling_pad {
                remaining.min(Duration::from_millis(100))
            } else {
                remaining
            });
            if polling_pad && pads.activity() {
                other_activity = Some(Instant::now());
            }
        }
    }
    drop(sampler);
    recorder.finish();
    if generation == storage.db_generation() && storage.is_initialized() {
        if let Err(error) = writer.write(&storage, &recorder.snapshot()) {
            tracing::debug!("[USAGE] Final checkpoint deferred: {error}");
        }
    }
    *runtime.live.lock().unwrap_or_else(|e| e.into_inner()) = LiveUsage::default();
}

fn activity_age(input: Option<u64>, other: Option<u64>, picture: Option<u64>) -> Option<u64> {
    input.into_iter().chain(other).chain(picture).min()
}

fn title_matches_page(window: &str, page: &str) -> bool {
    let clean = |value: &str| {
        value.chars().filter(|c| !matches!(*c, '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')).collect::<String>()
    };
    let window = clean(window);
    let page = clean(page);
    let window = window.trim();
    let page = page.trim();
    !page.is_empty()
        && (window == page
            || window.strip_prefix(page).is_some_and(|suffix| {
                [" - ", " — ", " – ", " | "]
                    .iter()
                    .any(|separator| suffix.starts_with(separator))
            }))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn any_supported_activity_keeps_the_three_minute_clock_running() {
        assert_eq!(activity_age(Some(250_000), None, Some(9_000)), Some(9_000));
        assert_eq!(activity_age(Some(250_000), Some(500), None), Some(500));
        assert_eq!(activity_age(Some(180_000), None, None), Some(180_000));
        assert_eq!(activity_age(None, None, None), None);
    }
    #[test]
    fn browser_pages_require_matching_foreground_identity_and_expire_without_heartbeats() {
        let runtime = UsageRuntime::default();
        runtime.browser_metadata(10, 20, &serde_json::json!({"focused": true, "title": "Example", "url": "https://user:password@example.com/page#fragment"})).unwrap();
        assert_eq!(
            runtime
                .page_for_foreground(20, "Example\u{200f} - Microsoft Edge")
                .0
                .as_deref(),
            Some("https://example.com/page")
        );
        assert_eq!(
            runtime.page_for_foreground(21, "Example - Microsoft Edge"),
            (None, false)
        );
        assert_eq!(
            runtime.page_for_foreground(20, "Example 2 - Microsoft Edge"),
            (None, false)
        );
        runtime.pages.lock().unwrap().front_mut().unwrap().at =
            Instant::now() - Duration::from_secs(46);
        assert_eq!(runtime.page_for_foreground(20, "Example"), (None, false));
        runtime.browser_metadata(10,20,&serde_json::json!({"focused":true,"private":true,"title":"Secret","url":"https://secret.example"})).unwrap();
        let pages = runtime.pages.lock().unwrap();
        assert!(pages[0].title.is_empty());
        assert!(pages[0].url.is_none());
        drop(pages);
        runtime.clear_browser(10);
        assert_eq!(runtime.page_for_foreground(20, "Example"), (None, false));
    }
}
