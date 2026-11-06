//! Native input, media, display and Windows-session signals. No input hooks.
use super::{foreground_title_matches, Observation, UsageIdentity};
use crate::capture::{self, CaptureState};
use std::cell::Cell;
use std::time::{Duration, Instant};
use windows::core::{w, RuntimeType, PCWSTR, PWSTR};
use windows::Foundation::{AsyncStatus, IAsyncOperation};
use windows::Media::Control::{
    GlobalSystemMediaTransportControlsSessionManager as MediaManager,
    GlobalSystemMediaTransportControlsSessionPlaybackStatus as PlaybackStatus,
};
use windows::Win32::Foundation::{BOOL, HANDLE, HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Storage::Packaging::Appx::GetApplicationUserModelId;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Power::{
    RegisterPowerSettingNotification, UnregisterPowerSettingNotification, HPOWERNOTIFY,
    POWERBROADCAST_SETTING,
};
use windows::Win32::System::RemoteDesktop::{
    WTSActive, WTSFreeMemory, WTSQuerySessionInformationW, WTSRegisterSessionNotification,
    WTSSessionInfoEx, WTSUnRegisterSessionNotification, NOTIFY_FOR_THIS_SESSION, WTSINFOEXW,
    WTS_CURRENT_SESSION, WTS_SESSIONSTATE_UNLOCK,
};
use windows::Win32::System::StationsAndDesktops::{
    CloseDesktop, GetUserObjectInformationW, OpenInputDesktop, DESKTOP_CONTROL_FLAGS,
    DESKTOP_READOBJECTS, UOI_NAME,
};
use windows::Win32::System::SystemInformation::GetTickCount64;
use windows::Win32::System::SystemServices::GUID_SESSION_DISPLAY_STATUS;
use windows::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
use windows::Win32::System::WinRT::{RoInitialize, RoUninitialize, RO_INIT_MULTITHREADED};
use windows::Win32::UI::WindowsAndMessaging::*;

#[derive(Default)]
struct Boundaries {
    revision: Cell<u64>,
    suspended: Cell<bool>,
    display_off: Cell<bool>,
    session_blocked: Cell<bool>,
}

pub(super) struct NativeProbe {
    boundaries: Box<Boundaries>,
    hwnd: Option<HWND>,
    power_notification: Option<HPOWERNOTIFY>,
    initialized_winrt: bool,
    media: Option<MediaManager>,
    media_retry_at: Option<Instant>,
}

impl NativeProbe {
    pub(super) fn new() -> Self {
        // SAFETY: this dedicated observer thread owns and balances its WinRT apartment.
        let initialized_winrt = unsafe { RoInitialize(RO_INIT_MULTITHREADED) }.is_ok();
        let mut probe = Self {
            boundaries: Box::new(Boundaries::default()),
            hwnd: None,
            power_notification: None,
            initialized_winrt,
            media: None,
            media_retry_at: None,
        };
        // SAFETY: the window is hidden, its callback and class live for the process,
        // and userdata points into the owned Box until DestroyWindow returns.
        unsafe {
            if let Ok(module) = GetModuleHandleW(PCWSTR::null()) {
                let instance = HINSTANCE(module.0);
                let class = w!("CarbonPaperUsageObserver");
                let definition = WNDCLASSW {
                    lpfnWndProc: Some(window_proc),
                    hInstance: instance,
                    lpszClassName: class,
                    ..Default::default()
                };
                RegisterClassW(&definition);
                if let Ok(hwnd) = CreateWindowExW(
                    WS_EX_TOOLWINDOW,
                    class,
                    w!(""),
                    WS_POPUP,
                    0,
                    0,
                    0,
                    0,
                    HWND::default(),
                    HMENU::default(),
                    instance,
                    Some((&*probe.boundaries as *const Boundaries).cast()),
                ) {
                    let _ = WTSRegisterSessionNotification(hwnd, NOTIFY_FOR_THIS_SESSION);
                    probe.power_notification = RegisterPowerSettingNotification(
                        HANDLE(hwnd.0),
                        &GUID_SESSION_DISPLAY_STATUS,
                        DEVICE_NOTIFY_WINDOW_HANDLE,
                    )
                    .ok();
                    probe.hwnd = Some(hwnd);
                }
            }
        }
        probe
    }

    pub(super) fn pump_events(&self) {
        // SAFETY: this thread dispatches only its own message queue; MSG is writable.
        unsafe {
            let mut message = MSG::default();
            while PeekMessageW(&mut message, HWND::default(), 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
    }

    pub(super) fn wait(&self) {
        // SAFETY: no borrowed handles are supplied. Power/session messages wake the wait.
        unsafe {
            MsgWaitForMultipleObjectsEx(None, 1_000, QS_ALLINPUT, MWMO_INPUTAVAILABLE);
        }
    }

    pub(super) fn apply_boundaries(&self, observation: &mut Observation) {
        observation.boundary = self.boundaries.revision.get();
        if self.boundaries.session_blocked.get() {
            observation.unavailable_reason = Some("session_unavailable");
        } else if self.boundaries.suspended.get() || self.boundaries.display_off.get() {
            observation.unavailable_reason = Some("display_unavailable");
        }
    }

    pub(super) fn observe(&mut self, capture_state: &CaptureState) -> Observation {
        self.boundaries.session_blocked.set(!session_unlocked());
        let available = !self.boundaries.session_blocked.get()
            && !self.boundaries.suspended.get()
            && !self.boundaries.display_off.get();
        let mut foreground = available
            .then(capture::get_active_window_info)
            .flatten()
            .filter(|window| {
                !capture::is_excluded(
                    window,
                    &capture_state
                        .exclusion_settings
                        .lock()
                        .unwrap_or_else(|e| e.into_inner()),
                )
            })
            .and_then(|window| {
                let (pid, path) = foreground_owner(&window)?;
                let name = capture::get_process_name_from_path(&path);
                if name.is_empty() {
                    return None;
                }
                Some(UsageIdentity {
                    process_name: name,
                    process_path: path,
                    window_title: window.title,
                    hwnd: window.hwnd_raw,
                    pid,
                })
            });
        let hint = foreground
            .as_ref()
            .and_then(crate::reverse_ipc::foreground_media_hint);
        let media_playing = match hint {
            Some(crate::reverse_ipc::ForegroundMediaHint::Private) => {
                foreground = None;
                false
            }
            Some(crate::reverse_ipc::ForegroundMediaHint::Playback(playing)) => playing,
            None => foreground
                .as_ref()
                .is_some_and(|window| self.native_media_playing(window)),
        };
        // Timestamp after the probe so slow API calls create a detectable coverage gap.
        let idle_ms = crate::idle::input_idle_ms();
        // SAFETY: GetTickCount64 has no pointers or retained resources.
        let tick_ms = unsafe { GetTickCount64() };
        Observation {
            utc_ms: chrono::Utc::now().timestamp_millis(),
            tick_ms,
            boundary: self.boundaries.revision.get(),
            idle_ms,
            foreground,
            media_playing,
            unavailable_reason: (!available).then_some("session_unavailable"),
        }
    }

    fn native_media_playing(&mut self, window: &UsageIdentity) -> bool {
        if !self.initialized_winrt {
            return false;
        }
        if self.media.is_none() {
            if self
                .media_retry_at
                .is_some_and(|deadline| Instant::now() < deadline)
            {
                return false;
            }
            self.media_retry_at = Some(Instant::now() + Duration::from_secs(60));
            self.media = MediaManager::RequestAsync().ok().and_then(wait_async);
        }
        let Some(manager) = &self.media else {
            return false;
        };
        let Ok(sessions) = manager.GetSessions() else {
            self.media = None;
            return false;
        };
        let aumid = application_id(window.pid);
        for index in 0..sessions.Size().unwrap_or(0) {
            let Ok(session) = sessions.GetAt(index) else {
                continue;
            };
            let Ok(source) = session.SourceAppUserModelId() else {
                continue;
            };
            if !media_source_matches(&source.to_string(), &window.process_name, aumid.as_deref()) {
                continue;
            }
            let playing = session
                .GetPlaybackInfo()
                .and_then(|info| info.PlaybackStatus())
                .is_ok_and(|status| status == PlaybackStatus::Playing);
            if !playing {
                continue;
            }
            // Browser media sessions can belong to background tabs. Extension reports
            // take priority; fallback also requires the focused page's media title.
            if capture::is_browser_process(&window.process_name)
                || crate::reverse_ipc::has_nmh_session_for_exe(&window.process_name)
            {
                let Some(properties) = session
                    .TryGetMediaPropertiesAsync()
                    .ok()
                    .and_then(wait_async)
                else {
                    continue;
                };
                let Ok(title) = properties.Title() else {
                    continue;
                };
                if !foreground_title_matches(&window.window_title, &title.to_string()) {
                    continue;
                }
            }
            return true;
        }
        false
    }
}

impl Drop for NativeProbe {
    fn drop(&mut self) {
        // SAFETY: every handle belongs to this probe and the userdata Box is still alive.
        unsafe {
            if let Some(notification) = self.power_notification.take() {
                let _ = UnregisterPowerSettingNotification(notification);
            }
            if let Some(hwnd) = self.hwnd.take() {
                let _ = WTSUnRegisterSessionNotification(hwnd);
                let _ = DestroyWindow(hwnd);
            }
            self.media = None;
            if self.initialized_winrt {
                RoUninitialize();
            }
        }
    }
}

fn wait_async<T: RuntimeType + 'static>(operation: IAsyncOperation<T>) -> Option<T> {
    let deadline = Instant::now() + Duration::from_millis(300);
    while operation.Status().ok()? == AsyncStatus::Started {
        if Instant::now() >= deadline {
            let _ = operation.Cancel();
            return None;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    operation.GetResults().ok()
}

fn media_source_matches(source: &str, process: &str, aumid: Option<&str>) -> bool {
    let source = source.to_lowercase();
    let process = process.to_lowercase();
    source.strip_suffix(".exe").unwrap_or(&source)
        == process.strip_suffix(".exe").unwrap_or(&process)
        || aumid.is_some_and(|id| id.eq_ignore_ascii_case(&source))
}

fn application_id(pid: u32) -> Option<String> {
    // SAFETY: the query-only process handle and UTF-16 output buffer live through the call.
    unsafe {
        let process = crate::windows_platform::OwnedHandle(
            OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?,
        );
        let mut length = 512u32;
        let mut buffer = vec![0u16; length as usize];
        if GetApplicationUserModelId(process.0, &mut length, PWSTR(buffer.as_mut_ptr())).0 != 0 {
            return None;
        }
        let end = buffer
            .iter()
            .position(|&value| value == 0)
            .unwrap_or(buffer.len());
        String::from_utf16(&buffer[..end]).ok()
    }
}

fn foreground_owner(window: &capture::ActiveWindowInfo) -> Option<(u32, String)> {
    let path = capture::get_process_path_from_pid(window.pid)?;
    if !capture::get_process_name_from_path(&path).eq_ignore_ascii_case("ApplicationFrameHost.exe")
    {
        return Some((window.pid, path));
    }
    struct Owner {
        host: u32,
        child: Option<u32>,
    }
    unsafe extern "system" fn find_core_window(hwnd: HWND, parameter: LPARAM) -> BOOL {
        // SAFETY: EnumChildWindows borrows the stack-owned Owner for this synchronous
        // enumeration only. All native output buffers are initialized and writable.
        unsafe {
            let owner = &mut *(parameter.0 as *mut Owner);
            let mut name = [0u16; 64];
            let length = GetClassNameW(hwnd, &mut name).max(0) as usize;
            if IsWindowVisible(hwnd).as_bool()
                && String::from_utf16_lossy(&name[..length]) == "Windows.UI.Core.CoreWindow"
            {
                let mut pid = 0;
                GetWindowThreadProcessId(hwnd, Some(&mut pid));
                if pid != 0 && pid != owner.host {
                    owner.child = Some(pid);
                    return BOOL(0);
                }
            }
            BOOL(1)
        }
    }
    let mut owner = Owner {
        host: window.pid,
        child: None,
    };
    // SAFETY: the callback only inspects child windows and retains no references.
    unsafe {
        let _ = EnumChildWindows(
            HWND(window.hwnd_raw as *mut std::ffi::c_void),
            Some(find_core_window),
            LPARAM((&mut owner as *mut Owner) as isize),
        );
    }
    if let Some(pid) = owner.child {
        capture::get_process_path_from_pid(pid).map(|path| (pid, path))
    } else {
        Some((window.pid, path))
    }
}

fn session_unlocked() -> bool {
    // SAFETY: WTS allocates aligned data, its byte length and level are checked,
    // and it is released after copying flags, including on unknown results.
    unsafe {
        let mut buffer = PWSTR::null();
        let mut size = 0u32;
        if WTSQuerySessionInformationW(
            HANDLE::default(),
            WTS_CURRENT_SESSION,
            WTSSessionInfoEx,
            &mut buffer,
            &mut size,
        )
        .is_err()
        {
            return input_desktop_unlocked();
        }
        let available = if !buffer.is_null() && size as usize >= std::mem::size_of::<WTSINFOEXW>() {
            let info = &*buffer.0.cast::<WTSINFOEXW>();
            info.Level == 1 && {
                let data = info.Data.WTSInfoExLevel1;
                data.SessionState == WTSActive
                    && data.SessionFlags == WTS_SESSIONSTATE_UNLOCK as i32
            }
        } else {
            false
        };
        WTSFreeMemory(buffer.0.cast());
        available
    }
}

fn input_desktop_unlocked() -> bool {
    // SAFETY: this query-only desktop handle is closed on every successful open;
    // its name buffer is initialized and the API retains no pointer.
    unsafe {
        let Ok(desktop) = OpenInputDesktop(DESKTOP_CONTROL_FLAGS(0), false, DESKTOP_READOBJECTS)
        else {
            return false;
        };
        let mut name = [0u16; 256];
        let queried = GetUserObjectInformationW(
            HANDLE(desktop.0),
            UOI_NAME,
            Some(name.as_mut_ptr().cast()),
            std::mem::size_of_val(&name) as u32,
            None,
        )
        .is_ok();
        let _ = CloseDesktop(desktop);
        let end = name
            .iter()
            .position(|&value| value == 0)
            .unwrap_or(name.len());
        queried && String::from_utf16_lossy(&name[..end]).eq_ignore_ascii_case("Default")
    }
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // SAFETY: Windows supplies CREATESTRUCTW during WM_NCCREATE and userdata
    // remains owned by NativeProbe through all callbacks and window destruction.
    unsafe {
        if message == WM_NCCREATE {
            let create = &*(lparam.0 as *const CREATESTRUCTW);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, create.lpCreateParams as isize);
        }
        let pointer = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const Boundaries;
        if let Some(state) = pointer.as_ref() {
            if message == WM_WTSSESSION_CHANGE {
                state.revision.set(state.revision.get().wrapping_add(1));
                match wparam.0 as u32 {
                    WTS_SESSION_LOCK
                    | WTS_SESSION_LOGOFF
                    | WTS_CONSOLE_DISCONNECT
                    | WTS_REMOTE_DISCONNECT => {
                        state.session_blocked.set(true);
                    }
                    WTS_SESSION_UNLOCK => state.session_blocked.set(false),
                    _ => {}
                }
            } else if message == WM_POWERBROADCAST {
                match wparam.0 as u32 {
                    PBT_APMSUSPEND => {
                        state.suspended.set(true);
                        state.revision.set(state.revision.get().wrapping_add(1));
                    }
                    PBT_APMRESUMEAUTOMATIC | PBT_APMRESUMESUSPEND | PBT_APMRESUMECRITICAL => {
                        state.suspended.set(false);
                        state.revision.set(state.revision.get().wrapping_add(1));
                    }
                    PBT_POWERSETTINGCHANGE if lparam.0 != 0 => {
                        let setting = &*(lparam.0 as *const POWERBROADCAST_SETTING);
                        if setting.PowerSetting == GUID_SESSION_DISPLAY_STATUS
                            && setting.DataLength >= 4
                        {
                            let value =
                                std::ptr::read_unaligned(setting.Data.as_ptr().cast::<u32>());
                            if state.display_off.replace(value == 0) != (value == 0) {
                                state.revision.set(state.revision.get().wrapping_add(1));
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        DefWindowProcW(hwnd, message, wparam, lparam)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_requires_the_foreground_process_and_browser_page() {
        assert!(media_source_matches("Player.EXE", "player.exe", None));
        assert!(media_source_matches(
            "Video.Package!App",
            "host.exe",
            Some("Video.Package!App")
        ));
        assert!(!media_source_matches("spotify.exe", "editor.exe", None));
        assert!(foreground_title_matches(
            "Film - YouTube - Google Chrome",
            "Film"
        ));
        assert!(!foreground_title_matches("Notes - Google Chrome", "Film"));
        assert!(!foreground_title_matches("Notes - Google Chrome", ""));
    }
}
