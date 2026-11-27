//! A resident Win32 tray panel. Showing it never creates a WebView or waits on
//! storage; authenticated thumbnails are prepared on the blocking runtime.

mod model;
#[cfg(windows)]
mod native;
mod navigation;
#[cfg(windows)]
mod render;

use base64::Engine;
use chrono::{DateTime, Local};
use model::{Action, Labels, PanelModel, ReadIdentity, Record, Thumbnail};
pub(crate) use navigation::Navigation;
use navigation::PendingNavigation;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::{Emitter, Manager};
use zeroize::Zeroizing;

#[derive(Default)]
pub(crate) struct TrayPanelState {
    model: Arc<Mutex<PanelModel>>,
    hwnd: AtomicIsize,
    visible: AtomicBool,
    refreshing: AtomicBool,
    dirty: AtomicBool,
    records_revision: AtomicU64,
    handoff: Mutex<Option<Instant>>,
    navigation: Mutex<Option<PendingNavigation>>,
}

pub(crate) fn prepare(app: &tauri::AppHandle) -> Result<(), String> {
    let state = Arc::new(TrayPanelState::default());
    app.manage(state.clone());
    refresh_appearance(app);
    refresh_public(app);
    #[cfg(windows)]
    {
        let actions_app = app.clone();
        let guard_app = app.clone();
        let hwnd = native::create(
            state.model.clone(),
            Box::new(move |action| dispatch(&actions_app, action)),
            Box::new(move |identity| current_identity(&guard_app) == Some(identity)),
        )?;
        state.hwnd.store(hwnd, Ordering::Release);
        Ok(())
    }
    #[cfg(not(windows))]
    Err("Native tray panel requires Windows".into())
}

pub(crate) fn available(app: &tauri::AppHandle) -> bool {
    app.try_state::<Arc<TrayPanelState>>()
        .is_some_and(|state| state.hwnd.load(Ordering::Acquire) != 0)
}

pub(crate) fn is_panel_window(app: &tauri::AppHandle, hwnd: isize) -> bool {
    hwnd != 0
        && app
            .try_state::<Arc<TrayPanelState>>()
            .is_some_and(|state| state.hwnd.load(Ordering::Acquire) == hwnd)
}

pub(crate) fn is_visible_or_handoff(app: &tauri::AppHandle) -> bool {
    app.try_state::<Arc<TrayPanelState>>().is_some_and(|state| {
        state.visible.load(Ordering::Acquire)
            || state
                .handoff
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .is_some_and(|until| Instant::now() < until)
    })
}

fn begin_handoff(app: &tauri::AppHandle) {
    let state = app.state::<Arc<TrayPanelState>>();
    let until = Instant::now() + Duration::from_secs(15);
    *state.handoff.lock().unwrap_or_else(|e| e.into_inner()) = Some(until);
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_secs(15)).await;
        let state = app.state::<Arc<TrayPanelState>>();
        let mut handoff = state.handoff.lock().unwrap_or_else(|e| e.into_inner());
        if *handoff == Some(until) {
            *handoff = None;
            drop(handoff);
            crate::settings_window::refresh_ui_foreground(&app);
        }
    });
}

pub(crate) fn finish_handoff(app: &tauri::AppHandle) {
    if let Some(state) = app.try_state::<Arc<TrayPanelState>>() {
        *state.handoff.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
}

pub(crate) fn show(app: &tauri::AppHandle, position: (i32, i32)) {
    if !available(app) {
        return;
    }
    refresh_public(app);
    let state = app.state::<Arc<TrayPanelState>>();
    let hwnd = state.hwnd.load(Ordering::Acquire);
    #[cfg(windows)]
    if let Err(error) = native::show(hwnd, position) {
        tracing::warn!("Native tray panel could not be shown: {error}");
    }
    records_changed(app);
}

fn hide(app: &tauri::AppHandle) {
    #[cfg(windows)]
    if let Some(state) = app.try_state::<Arc<TrayPanelState>>() {
        native::hide(state.hwnd.load(Ordering::Acquire));
    }
}

pub(crate) fn destroy(app: &tauri::AppHandle) {
    if let Some(state) = app.try_state::<Arc<TrayPanelState>>() {
        let hwnd = state.hwnd.swap(0, Ordering::AcqRel);
        #[cfg(windows)]
        native::destroy(hwnd);
        state
            .model
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear_records();
        *state.navigation.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
}

fn notify(state: &TrayPanelState) {
    #[cfg(windows)]
    native::refresh(state.hwnd.load(Ordering::Acquire));
}

/// CNG can leave a native owner disabled when its credential dialog returns.
/// Restore that owner before slower post-verification service work completes.
pub(crate) fn authentication_dialog_finished(app: &tauri::AppHandle, owner_hwnd: isize) {
    if !is_panel_window(app, owner_hwnd) {
        return;
    }
    let state = app.state::<Arc<TrayPanelState>>();
    state
        .model
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .authenticating = false;
    #[cfg(windows)]
    native::restore_after_authentication(owner_hwnd);
}

fn current_identity(app: &tauri::AppHandle) -> Option<ReadIdentity> {
    let credentials = app.try_state::<Arc<crate::CredentialManagerState>>()?;
    // Background processing authority never grants access to this UI.
    crate::commands::check_auth_required(&credentials).ok()?;
    let session = credentials.session_marker()?;
    let storage = app.try_state::<Arc<crate::StorageState>>()?;
    storage.is_initialized().then(|| ReadIdentity {
        session,
        database: storage.db_generation(),
    })
}

fn labels() -> Labels {
    let language = crate::registry_config::get_string("language").unwrap_or_default();
    let keys = [
        "recent",
        "today",
        "latest",
        "search",
        "searchLocked",
        "searchSubmit",
        "view",
        "unlock",
        "locked",
        "unlocking",
        "empty",
        "loading",
        "lightweight",
        "lightweightOn",
        "lightweightOff",
        "more",
        "running",
        "paused",
        "pausedUntil",
        "pausedManual",
        "stopped",
        "preparing",
        "powerSaving",
        "fullscreen",
        "pause15",
        "pause30",
        "pause60",
        "pauseMore",
        "resume",
        "manualResume",
        "autoResumeAt",
        "lock",
        "settings",
        "restart",
        "quit",
        "timeline",
        "ignoreApp",
        "recordApp",
        "actionFailed",
        "unlockFailed",
        "unknownApp",
        "untitled",
        "previewUnavailable",
        "lockedDescription",
        "emptyDescription",
        "recordingDescription",
        "start",
        "pauseManual",
        "fileExplorer",
    ];
    Labels(
        keys.into_iter()
            .map(|key| (key, crate::i18n::t(&language, &format!("trayPanel.{key}"))))
            .collect(),
    )
}

/// Public state only: bounded mutex/atomic reads, no database or image I/O.
pub(crate) fn refresh_public(app: &tauri::AppHandle) {
    let Some(state) = app.try_state::<Arc<TrayPanelState>>() else {
        return;
    };
    let identity = current_identity(app);
    let monitor = app.try_state::<crate::MonitorState>();
    let capture = app.try_state::<Arc<crate::CaptureState>>();
    let mut model = state.model.lock().unwrap_or_else(|e| e.into_inner());
    model.lightweight = app
        .try_state::<Arc<crate::LightweightModeState>>()
        .is_some_and(|mode| {
            *mode
                .is_lightweight
                .lock()
                .unwrap_or_else(|e| e.into_inner())
        });
    model.ready = crate::storage_startup::is_ready(app);
    model.running = monitor.as_ref().is_some_and(|monitor| monitor.is_running());
    model.paused = capture
        .as_ref()
        .is_some_and(|capture| capture.paused.load(Ordering::Acquire));
    model.pause_until_ms = monitor
        .as_ref()
        .and_then(|monitor| monitor.pause_until_ms());
    model.power_saving = crate::power::is_saving_power(app);
    model.controls_enabled = model.ready && !crate::maintenance::is_active();
    model.status = if !model.ready {
        "preparing"
    } else if model.power_saving {
        "powerSaving"
    } else if !model.running {
        "stopped"
    } else if model.paused {
        "paused"
    } else if capture
        .as_ref()
        .is_some_and(|capture| capture.game_mode_capture_paused.load(Ordering::Acquire))
    {
        "fullscreen"
    } else {
        "running"
    };
    let identity_changed = model.identity != identity;
    if identity_changed {
        model.clear_records();
        model.identity = identity;
    }
    if let (Some(capture), Some(record)) = (capture.as_ref(), model.selected()) {
        let excluded = capture
            .exclusion_settings
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .user_excluded_processes
            .contains(&record.process);
        model.excluded = excluded;
    }
    drop(model);
    notify(&state);
    if identity_changed && identity.is_some() {
        records_changed(app);
    }
}

pub(crate) fn refresh_appearance(app: &tauri::AppHandle) {
    let Some(state) = app.try_state::<Arc<TrayPanelState>>() else {
        return;
    };
    let labels = labels();
    let dark = crate::registry_config::get_string("theme")
        .map(|theme| theme == "dark")
        .unwrap_or_else(system_dark_theme);
    let mut model = state.model.lock().unwrap_or_else(|e| e.into_inner());
    model.labels = labels;
    model.dark = dark;
    drop(model);
    notify(&state);
}

fn system_dark_theme() -> bool {
    #[cfg(windows)]
    {
        use winreg::{enums::HKEY_CURRENT_USER, RegKey};
        RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize")
            .and_then(|key| key.get_value::<u32, _>("AppsUseLightTheme"))
            .is_ok_and(|value| value == 0)
    }
    #[cfg(not(windows))]
    false
}

pub(crate) fn auth_changed(app: &tauri::AppHandle) {
    let Some(state) = app.try_state::<Arc<TrayPanelState>>() else {
        return;
    };
    let current = current_identity(app);
    {
        let mut pending = state.navigation.lock().unwrap_or_else(|e| e.into_inner());
        if pending
            .as_ref()
            .is_some_and(|pending| pending.identity.is_some() && pending.identity != current)
        {
            *pending = None;
        }
    }
    refresh_public(app);
}

/// Capture calls only mark dirty and schedule one blocking job. Repeated
/// captures during a read are coalesced, including when the panel is hidden.
pub(crate) fn records_changed(app: &tauri::AppHandle) {
    let Some(state) = app.try_state::<Arc<TrayPanelState>>() else {
        return;
    };
    if state.hwnd.load(Ordering::Acquire) == 0 {
        return;
    }
    state.records_revision.fetch_add(1, Ordering::AcqRel);
    state.dirty.store(true, Ordering::Release);
    if state.refreshing.swap(true, Ordering::AcqRel) {
        return;
    }
    let state = state.inner().clone();
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            state.dirty.store(false, Ordering::Release);
            let revision = state.records_revision.load(Ordering::Acquire);
            if state.hwnd.load(Ordering::Acquire) == 0 {
                state.refreshing.store(false, Ordering::Release);
                break;
            }
            if let Some(identity) = current_identity(&app) {
                let read_app = app.clone();
                let read_state = state.clone();
                let result = tokio::task::spawn_blocking(move || {
                    load_records(&read_app, &read_state, identity, revision)
                })
                .await;
                match result {
                    Ok(Ok(records))
                        if current_identity(&app) == Some(identity)
                            && state.records_revision.load(Ordering::Acquire) == revision =>
                    {
                        let mut model = state.model.lock().unwrap_or_else(|e| e.into_inner());
                        // Recheck after acquiring the publication lock as well.
                        if current_identity(&app) == Some(identity)
                            && state.records_revision.load(Ordering::Acquire) == revision
                            && state.hwnd.load(Ordering::Acquire) != 0
                        {
                            let selected_id = model.selected().map(|record| record.id);
                            let followed_latest = model.selected + 1 >= model.records.len();
                            model.selected = if followed_latest {
                                records.len().saturating_sub(1)
                            } else {
                                records
                                    .iter()
                                    .position(|record| Some(record.id) == selected_id)
                                    .unwrap_or_else(|| records.len().saturating_sub(1))
                            };
                            model.identity = Some(identity);
                            model.records = records;
                            model.records_loaded = true;
                        }
                    }
                    Ok(Err(error))
                        if current_identity(&app) == Some(identity)
                            && state.records_revision.load(Ordering::Acquire) == revision =>
                    {
                        tracing::debug!("Native tray preview refresh deferred: {error}");
                        let mut model = state.model.lock().unwrap_or_else(|e| e.into_inner());
                        model.records_loaded = true;
                    }
                    Err(error) => tracing::warn!("Native tray preview task failed: {error}"),
                    _ => {}
                }
                refresh_public(&app);
            }
            if state.dirty.swap(false, Ordering::AcqRel) {
                continue;
            }
            state.refreshing.store(false, Ordering::Release);
            // Close the race between observing a clean queue and releasing it.
            if state.dirty.load(Ordering::Acquire) && !state.refreshing.swap(true, Ordering::AcqRel)
            {
                continue;
            }
            break;
        }
    });
}

/// A deleted record must disappear from the resident surface immediately, and
/// an older in-flight read must not republish it after the deletion.
pub(crate) fn invalidate_records(app: &tauri::AppHandle) {
    let Some(state) = app.try_state::<Arc<TrayPanelState>>() else {
        return;
    };
    let mut model = state.model.lock().unwrap_or_else(|e| e.into_inner());
    state.records_revision.fetch_add(1, Ordering::AcqRel);
    model.clear_records();
    model.identity = current_identity(app);
    drop(model);
    notify(&state);
    records_changed(app);
}

fn load_records(
    app: &tauri::AppHandle,
    state: &TrayPanelState,
    identity: ReadIdentity,
    revision: u64,
) -> Result<Vec<Arc<Record>>, String> {
    if current_identity(app) != Some(identity) {
        return Err("AUTH_REQUIRED".into());
    }
    let storage = app.state::<Arc<crate::StorageState>>();
    let data_dir = storage
        .data_dir
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    app.state::<Arc<crate::CaptureState>>()
        .refresh_exclusion_settings(&data_dir, || {
            current_identity(app) == Some(identity)
                && state.records_revision.load(Ordering::Acquire) == revision
        })?;
    let previous = {
        let model = state.model.lock().unwrap_or_else(|e| e.into_inner());
        if model.identity == Some(identity) {
            model.records.clone()
        } else {
            Vec::new()
        }
    };
    let rows = storage.screenshots().recent(3)?;
    let mut records = Vec::with_capacity(rows.len());
    for row in rows.into_iter().rev() {
        if state.records_revision.load(Ordering::Acquire) != revision {
            return Err("Tray preview changed during its read".into());
        }
        if current_identity(app) != Some(identity) {
            return Err("AUTH_REQUIRED".into());
        }
        if let Some(previous) = previous
            .iter()
            .find(|record| record.id == row.screenshot_id && record.thumbnail.is_some())
        {
            records.push(previous.clone());
            continue;
        }
        let time = DateTime::parse_from_rfc3339(&row.created_at)
            .map_err(|error| format!("Invalid screenshot timestamp: {error}"))?
            .with_timezone(&Local);
        let title = row
            .window_title
            .filter(|title| !title.trim().is_empty())
            .unwrap_or_else(|| labels().get("untitled").to_string());
        let process = row.process_name.unwrap_or_default().trim().to_lowercase();
        let app_name = friendly_app(&process);
        let thumbnail = storage
            .read_thumbnail_by_id(row.screenshot_id)
            .ok()
            .and_then(|(encoded, _)| decode_thumbnail(encoded).ok())
            .map(Arc::new);
        records.push(Arc::new(Record {
            id: row.screenshot_id,
            timestamp_ms: time.timestamp_millis(),
            time: time.format("%H:%M").to_string(),
            date: time.format("%Y-%m-%d").to_string(),
            title,
            app: app_name,
            process,
            thumbnail,
        }));
    }
    if current_identity(app) != Some(identity) {
        return Err("AUTH_REQUIRED".into());
    }
    Ok(records)
}

fn decode_thumbnail(encoded: String) -> Result<Thumbnail, String> {
    let encoded = Zeroizing::new(encoded);
    let bytes = Zeroizing::new(
        base64::engine::general_purpose::STANDARD
            .decode(encoded.as_bytes())
            .map_err(|e| e.to_string())?,
    );
    let decoded = image::load_from_memory(&bytes).map_err(|e| e.to_string())?;
    let resized = decoded.thumbnail(668, 328).to_rgba8();
    let (width, height) = resized.dimensions();
    let mut pixels = resized.into_raw();
    for pixel in pixels.as_chunks_mut::<4>().0 {
        pixel.swap(0, 2);
        let alpha = u16::from(pixel[3]);
        for channel in &mut pixel[..3] {
            *channel = (u16::from(*channel) * alpha / 255) as u8;
        }
    }
    Ok(Thumbnail {
        width,
        height,
        pixels,
    })
}

fn friendly_app(process: &str) -> String {
    match process.strip_suffix(".exe").unwrap_or(process) {
        "msedge" => "Microsoft Edge".into(),
        "chrome" => "Google Chrome".into(),
        "firefox" => "Firefox".into(),
        "code" => "Visual Studio Code".into(),
        "obsidian" => "Obsidian".into(),
        "notion" => "Notion".into(),
        "winword" => "Word".into(),
        "excel" => "Excel".into(),
        "powerpnt" => "PowerPoint".into(),
        "explorer" => labels().get("fileExplorer").to_string(),
        "cc-switch" => "CC Switch".into(),
        "" => labels().get("unknownApp").to_string(),
        _ => process.strip_suffix(".exe").unwrap_or(process).to_string(),
    }
}

pub(crate) fn take_navigation(app: &tauri::AppHandle) -> Option<Navigation> {
    let state = app.try_state::<Arc<TrayPanelState>>()?;
    let pending = state
        .navigation
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take()?;
    pending
        .admitted(current_identity(app))
        .then(|| pending.intent.clone())
}

fn navigate(app: &tauri::AppHandle, intent: Navigation, identity: Option<ReadIdentity>) {
    let state = app.state::<Arc<TrayPanelState>>();
    *state.navigation.lock().unwrap_or_else(|e| e.into_inner()) =
        Some(PendingNavigation::new(intent, identity));
    begin_handoff(app);
    hide(app);
    crate::open_main_window_from_tray(app);
    // Only a notification. Protected arguments travel through a guarded command.
    let _ = app.emit_to("main", "tray-navigate", ());
}

fn dispatch(app: &tauri::AppHandle, action: Action) {
    let state = app.state::<Arc<TrayPanelState>>();
    match action {
        Action::Visibility(visible) => {
            state.visible.store(visible, Ordering::Release);
            crate::settings_window::refresh_ui_foreground(app);
            if visible {
                records_changed(app);
            }
        }
        Action::Refresh => refresh_public(app),
        Action::Select(index) => {
            let mut model = state.model.lock().unwrap_or_else(|e| e.into_inner());
            if model.identity.is_some()
                && model.identity == current_identity(app)
                && index < model.records.len()
            {
                model.selected = index;
            }
            drop(model);
            refresh_public(app);
        }
        Action::Preview => {
            let model = state.model.lock().unwrap_or_else(|e| e.into_inner());
            if let (Some(record), Some(identity)) = (model.selected(), model.identity) {
                if current_identity(app) == Some(identity) {
                    let intent = Navigation::Snapshot {
                        id: record.id,
                        timestamp_ms: record.timestamp_ms,
                    };
                    drop(model);
                    navigate(app, intent, Some(identity));
                }
            }
        }
        Action::Search(query) => {
            let query = query.trim().chars().take(4096).collect::<String>();
            if !query.is_empty() {
                navigate(app, Navigation::Search { query }, current_identity(app));
            }
        }
        Action::OpenTimeline => navigate(app, Navigation::Timeline, current_identity(app)),
        Action::Lock => {
            app.state::<Arc<crate::CredentialManagerState>>()
                .invalidate_session();
            crate::settings_window::broadcast_auth_state(app);
        }
        Action::Quit => crate::quit_from_tray(app),
        action => {
            if matches!(
                action,
                Action::Start | Action::Resume | Action::Pause(_) | Action::Restart
            ) {
                // Recheck live power policy before accepting a queued click.
                refresh_public(app);
                if !state
                    .model
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .capture_controls_enabled()
                {
                    return;
                }
            }
            {
                let mut model = state.model.lock().unwrap_or_else(|e| e.into_inner());
                if model.busy {
                    return;
                }
                model.busy = true;
                model.authenticating = matches!(action, Action::Unlock);
                model.message = None;
            }
            notify(&state);
            let app = app.clone();
            let state = state.inner().clone();
            tauri::async_runtime::spawn(async move {
                let unlocking = matches!(action, Action::Unlock);
                let result = perform(&app, action).await;
                {
                    let mut model = state.model.lock().unwrap_or_else(|e| e.into_inner());
                    model.busy = false;
                    model.authenticating = false;
                    if let Err(error) = result {
                        // Power can change after admission. Refresh its pause
                        // reason below instead of reporting a retryable failure.
                        if error != crate::power::POWER_SAVING_ACTIVE {
                            tracing::warn!("Native tray action failed: {error}");
                            model.message = Some(
                                model
                                    .labels
                                    .get(if unlocking {
                                        "unlockFailed"
                                    } else {
                                        "actionFailed"
                                    })
                                    .to_string(),
                            );
                        }
                    }
                }
                refresh_public(&app);
                #[cfg(windows)]
                if unlocking {
                    native::focus_after_unlock(state.hwnd.load(Ordering::Acquire));
                }
            });
        }
    }
}

async fn perform(app: &tauri::AppHandle, action: Action) -> Result<(), String> {
    match action {
        Action::Pause(0) => crate::monitor::control(app, crate::monitor::MonitorAction::Pause)
            .await
            .map(|_| ()),
        Action::Start => crate::monitor::control(app, crate::monitor::MonitorAction::Start)
            .await
            .map(|_| ()),
        Action::Pause(minutes) => {
            crate::monitor::control(app, crate::monitor::MonitorAction::PauseFor(minutes))
                .await
                .map(|_| ())
        }
        Action::Resume => crate::monitor::control(app, crate::monitor::MonitorAction::Resume)
            .await
            .map(|_| ()),
        Action::Restart => crate::monitor::control(app, crate::monitor::MonitorAction::Restart)
            .await
            .map(|_| ()),
        Action::Unlock => {
            crate::storage_startup::ensure_initialized(app.clone()).await?;
            let state = app.state::<Arc<TrayPanelState>>();
            let verified = crate::commands::credential::verify_user_for_owner(
                app.clone(),
                state.hwnd.load(Ordering::Acquire),
            )
            .await?;
            verified
                .then_some(())
                .ok_or_else(|| "Verification was cancelled".to_string())
        }
        Action::Settings => {
            begin_handoff(app);
            hide(app);
            let result = crate::settings_window::open_from_native(app).await;
            finish_handoff(app);
            crate::settings_window::refresh_ui_foreground(app);
            result
        }
        Action::ToggleLightweight => {
            let lightweight = *app
                .state::<Arc<crate::LightweightModeState>>()
                .is_lightweight
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if lightweight {
                begin_handoff(app);
                hide(app);
                crate::open_main_window_from_tray(app);
                Ok(())
            } else {
                // Bring a possible unsaved-settings confirmation into view.
                if let Some(settings) = app.get_webview_window(crate::settings_window::LABEL) {
                    settings.unminimize().map_err(|e| e.to_string())?;
                    settings.show().map_err(|e| e.to_string())?;
                    settings.set_focus().map_err(|e| e.to_string())?;
                    begin_handoff(app);
                    hide(app);
                }
                crate::commands::utility::switch_to_lightweight_mode(
                    app.clone(),
                    app.state::<Arc<crate::LightweightModeState>>(),
                )
                .await
            }
        }
        Action::ToggleExcluded => {
            let state = app.state::<Arc<TrayPanelState>>();
            let (identity, process) = {
                let model = state.model.lock().unwrap_or_else(|e| e.into_inner());
                (
                    model.identity,
                    model.selected().map(|record| record.process.clone()),
                )
            };
            let identity = identity.ok_or("AUTH_REQUIRED")?;
            let process = process
                .filter(|process| !process.is_empty())
                .ok_or("Missing process")?;
            let app = app.clone();
            tokio::task::spawn_blocking(move || {
                if current_identity(&app) != Some(identity) {
                    return Err("AUTH_REQUIRED".into());
                }
                let capture = app.state::<Arc<crate::CaptureState>>();
                let storage = app.state::<Arc<crate::StorageState>>();
                let data_dir = storage
                    .data_dir
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone();
                if current_identity(&app) != Some(identity) {
                    return Err("AUTH_REQUIRED".into());
                }
                capture.toggle_process_exclusion_guarded(&data_dir, &process, || {
                    current_identity(&app) == Some(identity)
                })?;
                let _ = app.emit("capture-filters-changed", ());
                Ok(())
            })
            .await
            .map_err(|e| e.to_string())?
        }
        _ => Ok(()),
    }
}
