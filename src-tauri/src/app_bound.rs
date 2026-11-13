//! User-facing controls and protected runtime handoff. Keys and task inputs
//! deliberately have no Tauri invoke interface.
use crate::{
    credential_manager::CredentialManagerState, processing_stage::ProcessingStatus,
    storage::StorageState,
};
use carbonpaper_app_bound::{manifest, protocol::Limits, windows::identity};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};
use tauri::{AppHandle, Manager};

#[cfg(not(feature = "app-bound-dev"))]
mod repair;

const OFFER_SEEN: &str = "app_bound_offer_seen";
const PACKAGE_SOURCE: &str = "app_bound_package_source";
static REPAIR_NEEDED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static UPGRADE_NEEDED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static REPAIR_OFFER_DISMISSED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);
static PROTECTED_ENVIRONMENT_READY: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

#[derive(serde::Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum RepairOutcome {
    Repaired {
        restart_required: bool,
    },
    #[cfg(not(feature = "app-bound-dev"))]
    DownloadRequired {
        size_bytes: Option<u64>,
    },
}

#[derive(serde::Serialize)]
pub struct AppBoundStatus {
    #[serde(flatten)]
    pub processing: ProcessingStatus,
    pub offer_enable: bool,
    pub package_available: bool,
    pub repair_available: bool,
}

fn executable_directory() -> Result<PathBuf, String> {
    // Match the service's kernel image path rather than the loader's spelling
    // of current_exe, which can retain caller-supplied path aliases.
    let executable = identity::inspect_process(std::process::id())
        .map_err(|e| e.to_string())?
        .image;
    executable
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| "Application directory is unavailable".into())
}

pub(crate) fn supported_build() -> bool {
    !cfg!(debug_assertions) || cfg!(feature = "app-bound-dev")
}

/// Package uninstall runs before the desktop or delegation starts. The helper
/// validates this signed caller again after elevation and removes only its SID.
pub fn uninstall_for_package() -> Result<(), String> {
    if !supported_build() {
        return Ok(());
    }
    let root = identity::protected_root().map_err(|e| e.to_string())?;
    if !root.exists() {
        return Ok(());
    }
    let sid = identity::current_sid().map_err(|e| e.to_string())?;
    let needs_removal = [
        root.join("Activations").join(format!("{sid}.json")),
        root.join("Detached").join(format!("{sid}.json")),
        root.join("System/activation.pending.json"),
    ]
    .into_iter()
    .map(|path| path.try_exists().map_err(|error| error.to_string()))
    .collect::<Result<Vec<_>, _>>()?
    .into_iter()
    .any(|exists| exists);
    if !needs_removal {
        return Ok(());
    }
    let directory = executable_directory()?;
    let (release, _) = manifest::read_manifest(&directory).map_err(|e| e.to_string())?;
    let helper = directory.join("carbonpaper-protected-setup.exe");
    let mut locked = identity::locked_file(&helper).map_err(|e| e.to_string())?;
    identity::verify_file(
        &mut locked,
        release
            .files
            .get("carbonpaper-protected-setup.exe")
            .ok_or("Installer is missing")?,
    )
    .map_err(|e| e.to_string())?;
    carbonpaper_app_bound::privileged::verify_loader_policy(&mut locked)
        .map_err(|e| e.to_string())?;
    let status = runas::Command::new(&helper)
        .arg("--uninstall")
        .arg("--remove-runtime")
        .arg("--caller-pid")
        .arg(std::process::id().to_string())
        .gui(true)
        .show(false)
        .status()
        .map_err(|e| e.to_string())?;
    if !status.success() {
        return Err("APP_BOUND_UNINSTALL_FAILED".into());
    }
    Ok(())
}

fn installation_source() -> Result<PathBuf, String> {
    #[cfg(feature = "app-bound-dev")]
    return carbonpaper_app_bound::development::package_directory().map_err(|e| e.to_string());
    #[cfg(not(feature = "app-bound-dev"))]
    executable_directory()
}

pub(crate) fn is_protected_runtime() -> bool {
    // The development client remains in Cargo's target directory. Its signed
    // registration is checked by the separate service, while dev workers keep
    // their ordinary discovery and Vite keeps control of the desktop process.
    if cfg!(feature = "app-bound-dev") {
        return false;
    }
    if protected_environment_ready() {
        return true;
    }
    let Ok(Some(active)) = identity::active_runtime() else {
        return false;
    };
    executable_directory().is_ok_and(|current| identity::path_eq(&active, &current))
}

pub(crate) fn protected_environment_ready() -> bool {
    PROTECTED_ENVIRONMENT_READY.load(std::sync::atomic::Ordering::Acquire)
}

pub(crate) fn uses_protected_installation() -> bool {
    if cfg!(feature = "app-bound-dev") {
        return false;
    }
    is_protected_runtime()
        || REPAIR_NEEDED.load(std::sync::atomic::Ordering::Acquire)
        || identity::active_runtime().ok().flatten().is_some()
}

/// Protected processes never fall back to a developer tree, a Python DLL, or
/// a per-user runtime override when a signed native component is unavailable.
pub(crate) fn protected_resource(relative: &str) -> Result<Option<PathBuf>, String> {
    if !is_protected_runtime() {
        return Ok(None);
    }
    let directory = executable_directory()?;
    let path = manifest::safe_relative_path(relative).map_err(|e| e.to_string())?;
    let (release, _) = manifest::read_manifest(&directory).map_err(|e| e.to_string())?;
    let expected = release
        .files
        .get(relative)
        .ok_or("Protected resource is missing from the release")?;
    let mut target = directory;
    for component in path.components() {
        target.push(component);
        identity::assert_protected(&target).map_err(|e| e.to_string())?;
    }
    identity::verify_file(
        &mut identity::locked_file(&target).map_err(|e| e.to_string())?,
        expected,
    )
    .map_err(|e| e.to_string())?;
    Ok(Some(target))
}

/// Called before the single-instance plugin: movable portable and installer
/// copies launch the registered runtime unless this package is newer.
pub fn delegate_startup(args: &[String]) -> Result<bool, String> {
    #[cfg(feature = "app-bound-dev")]
    {
        if !args.iter().any(|arg| arg == "--cng-unlock") {
            match crate::app_bound_dev::initialize() {
                Ok(()) => {
                    PROTECTED_ENVIRONMENT_READY.store(true, std::sync::atomic::Ordering::Release)
                }
                Err(error) => {
                    eprintln!("[app-bound dev] {error}");
                    REPAIR_NEEDED.store(true, std::sync::atomic::Ordering::Release);
                }
            }
        }
        Ok(false)
    }
    #[cfg(not(feature = "app-bound-dev"))]
    {
        if cfg!(debug_assertions) {
            return Ok(false);
        }
        let directory = executable_directory()?;
        let root = identity::protected_root().map_err(|e| e.to_string())?;
        delegate_startup_in_directory(&directory, &root, || try_delegate_startup(args, &directory))
    }
}

#[cfg(not(feature = "app-bound-dev"))]
fn protected_runtime_directory(directory: &Path, root: &Path) -> bool {
    directory
        .parent()
        .is_some_and(|parent| identity::path_eq(parent, &root.join("Runtime")))
}

#[cfg(not(feature = "app-bound-dev"))]
fn delegate_startup_in_directory(
    directory: &Path,
    root: &Path,
    delegate: impl FnOnce() -> Result<bool, String>,
) -> Result<bool, String> {
    // Classify the image location without relying on an activation or manifest
    // that may itself need repair. Every protected startup, including the CNG
    // helper, must be sanitized before any argument or recovery branch runs.
    if protected_runtime_directory(directory, root) {
        sanitize_runtime_environment(directory)?;
    }
    match delegate() {
        Ok(result) => Ok(result),
        Err(_) => {
            // Keep archive capture and repair usable after delegation failures.
            // A protected copy reaches this only after successful sanitization;
            // failed delegation never enables its broker access.
            REPAIR_NEEDED.store(true, std::sync::atomic::Ordering::Release);
            Ok(false)
        }
    }
}

#[cfg(not(feature = "app-bound-dev"))]
fn try_delegate_startup(args: &[String], directory: &Path) -> Result<bool, String> {
    if args.iter().any(|arg| arg == "--cng-unlock") {
        return Ok(false);
    }
    if args.iter().any(|arg| arg == "--repair-protected-runtime") {
        REPAIR_NEEDED.store(true, std::sync::atomic::Ordering::Release);
        return Ok(false);
    }
    let Some(active) = identity::active_runtime().map_err(|e| e.to_string())? else {
        return Ok(false);
    };
    if identity::path_eq(&active, directory) {
        PROTECTED_ENVIRONMENT_READY.store(true, std::sync::atomic::Ordering::Release);
        return Ok(false);
    }
    let (release, _) = manifest::read_manifest(&active).map_err(|e| e.to_string())?;
    if package_is_newer(env!("CARGO_PKG_VERSION"), &release.version)? {
        // A manual package update must run the new UI rather than silently
        // delegating to an older protected copy. Activation remains an explicit
        // authenticated action; an ordinary login never starts a UAC prompt.
        UPGRADE_NEEDED.store(true, std::sync::atomic::Ordering::Release);
        if existing_desktop_instance() {
            // The single-instance plugin would otherwise raise the old UI and
            // discard this new process. Ask for an ordinary exit before it can
            // touch the shared archive; background launches remain silent.
            if !args
                .iter()
                .any(|arg| matches!(arg.as_str(), "--hidden" | "--autostart"))
            {
                show_old_version_running();
            }
            return Ok(true);
        }
        return Ok(false);
    }
    let executable = active.join("carbonpaper.exe");
    let mut locked = identity::locked_file(&executable).map_err(|e| e.to_string())?;
    identity::verify_file(
        &mut locked,
        release
            .files
            .get("carbonpaper.exe")
            .ok_or("Protected application is missing")?,
    )
    .map_err(|e| e.to_string())?;
    let mut command = std::process::Command::new(&executable);
    command.current_dir(&active);
    for argument in args.iter().skip(1) {
        if matches!(argument.as_str(), "--hidden" | "--autostart") {
            command.arg(argument);
        }
    }
    let mut child = command
        .spawn()
        .map_err(|e| format!("Failed to open protected application: {e}"))?;
    // Detect immediate loader failures so the original package can show repair.
    // A successful single-instance handoff may also exit immediately with zero.
    std::thread::sleep(std::time::Duration::from_millis(250));
    if child
        .try_wait()
        .map_err(|e| e.to_string())?
        .is_some_and(|status| !status.success())
    {
        return Err("Protected application could not start".into());
    }
    Ok(true)
}

#[cfg(not(feature = "app-bound-dev"))]
fn package_is_newer(candidate: &str, active: &str) -> Result<bool, String> {
    manifest::compare_versions(candidate, active)
        .map(|order| order.is_gt())
        .map_err(|e| e.to_string())
}

#[cfg(not(feature = "app-bound-dev"))]
fn existing_desktop_instance() -> bool {
    use windows::{
        core::PCWSTR,
        Win32::UI::WindowsAndMessaging::{FindWindowW, GetWindowThreadProcessId},
    };
    let class = identity::wide(std::ffi::OsStr::new("com.eyling.carbonpaper-sic"));
    let name = identity::wide(std::ffi::OsStr::new("com.eyling.carbonpaper-siw"));
    // SAFETY: this queries the plugin's fixed local window. Its owner is
    // checked against the OS token; it is never used as an IPC authority.
    unsafe {
        let Ok(window) = FindWindowW(PCWSTR(class.as_ptr()), PCWSTR(name.as_ptr())) else {
            return false;
        };
        let mut pid = 0;
        GetWindowThreadProcessId(window, Some(&mut pid));
        if pid == 0 || pid == std::process::id() {
            return false;
        }
        let Ok(process) = identity::inspect_process(pid) else {
            return true;
        };
        if identity::current_sid().is_ok_and(|sid| sid != process.principal.sid) {
            return false;
        }
        // Another copy of this newer UI may already be offering activation.
        // Its normal single-instance handoff is safe and needs no exit prompt.
        if process
            .image
            .parent()
            .and_then(|dir| manifest::read_manifest(dir).ok())
            .is_some_and(|(release, _)| {
                package_is_newer(env!("CARGO_PKG_VERSION"), &release.version) == Ok(false)
            })
        {
            return false;
        }
        true
    }
}

#[cfg(not(feature = "app-bound-dev"))]
fn show_old_version_running() {
    use windows::{
        core::PCWSTR,
        Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONINFORMATION, MB_OK},
    };
    let locale = crate::registry_config::get_string("language").unwrap_or_default();
    let text = identity::wide(std::ffi::OsStr::new(&crate::i18n::t(
        &locale,
        "appBound.closeOldVersion",
    )));
    let title = identity::wide(std::ffi::OsStr::new("CarbonPaper"));
    // SAFETY: this informational dialog has no privileged action or shared
    // application state and both strings remain valid until it closes.
    unsafe {
        MessageBoxW(
            None,
            PCWSTR(text.as_ptr()),
            PCWSTR(title.as_ptr()),
            MB_OK | MB_ICONINFORMATION,
        );
    }
}

#[cfg(not(feature = "app-bound-dev"))]
fn sanitize_runtime_environment(executable: &Path) -> Result<(), String> {
    for key in [
        "WEBVIEW2_BROWSER_EXECUTABLE_FOLDER",
        "WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS",
        "WEBVIEW2_USER_DATA_FOLDER",
        "CARBONPAPER_ORT_DYLIB_PATH",
        "ORT_DYLIB_PATH",
    ] {
        std::env::remove_var(key);
    }
    let system = identity::system_directory().map_err(|e| e.to_string())?;
    std::env::set_current_dir(executable).map_err(|e| e.to_string())?;
    std::env::set_var(
        "PATH",
        std::env::join_paths([
            executable.to_path_buf(),
            system.clone(),
            system.join("WindowsPowerShell/v1.0"),
            system.join("Wbem"),
        ])
        .map_err(|e| e.to_string())?,
    );
    // SAFETY: this changes only the current process DLL-search policy.
    unsafe {
        use windows::Win32::System::LibraryLoader::*;
        SetDefaultDllDirectories(
            LOAD_LIBRARY_SEARCH_APPLICATION_DIR | LOAD_LIBRARY_SEARCH_SYSTEM32,
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
pub async fn app_bound_status(
    window: tauri::Window,
    storage: tauri::State<'_, Arc<StorageState>>,
) -> Result<AppBoundStatus, String> {
    crate::settings_window::check_settings_ui(&window)?;
    let storage = storage.inner().clone();
    tokio::task::spawn_blocking(move || {
        let upgrade = UPGRADE_NEEDED.load(std::sync::atomic::Ordering::Acquire);
        let launch_repair = REPAIR_NEEDED.load(std::sync::atomic::Ordering::Acquire);
        if !upgrade && !launch_repair && storage.background_processing_enabled() {
            let _ = storage.processing_stage.refresh();
        } else if !upgrade && !launch_repair {
            let _ = storage.processing_stage.disable_if_installed();
        }
        let mut processing = storage.processing_stage.status();
        if processing.installed && !processing.available {
            processing.reason = status_reason(processing.reason.as_deref()).map(str::to_string);
        }
        let repair = launch_repair || processing.reason.as_deref() == Some("repair_required");
        if repair || upgrade {
            processing.installed = true;
            processing.available = false;
            processing.reason = Some(
                if upgrade {
                    "upgrade_required"
                } else {
                    "repair_required"
                }
                .into(),
            );
        }
        let package_available = supported_build()
            && installation_source()
                .ok()
                .is_some_and(|dir| manifest::read_manifest(&dir).is_ok());
        let repair_available = supported_build()
            && (processing.installed
                || launch_repair
                || identity::current_sid()
                    .ok()
                    .and_then(|sid| {
                        identity::protected_root().ok().map(|root| {
                            root.join("Activations")
                                .join(format!("{sid}.json"))
                                .exists()
                        })
                    })
                    .unwrap_or(false));
        let offer_enable = ((repair && repair_available)
            || (upgrade && package_available)
            || (package_available
                && !processing.installed
                && storage.background_processing_enabled()))
            && (((repair || upgrade)
                && !REPAIR_OFFER_DISMISSED.load(std::sync::atomic::Ordering::Acquire))
                || (storage.background_processing_enabled()
                    && !processing.installed
                    && !crate::registry_config::get_bool(OFFER_SEEN).unwrap_or(false)));
        Ok(AppBoundStatus {
            processing,
            package_available,
            repair_available,
            offer_enable,
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

fn status_reason(reason: Option<&str>) -> Option<&str> {
    match reason {
        Some(
            "app_bound_unavailable"
            | "app_bound_task_busy"
            | "app_bound_storage_failure"
            | "app_bound_protection_failure",
        ) => Some("service_unavailable"),
        Some(
            "app_bound_access_denied"
            | "app_bound_integrity_failure"
            | "app_bound_dataset_mismatch"
            | "app_bound_version_mismatch",
        ) => Some("repair_required"),
        other => other,
    }
}

#[tauri::command]
pub fn app_bound_acknowledge_offer(window: tauri::Window) -> Result<(), String> {
    crate::commands::check_main_window(&window)?;
    REPAIR_OFFER_DISMISSED.store(true, std::sync::atomic::Ordering::Release);
    crate::registry_config::set_bool(OFFER_SEEN, true)
}

#[tauri::command]
pub async fn app_bound_set_policy(
    window: tauri::Window,
    app: AppHandle,
    credentials: tauri::State<'_, Arc<CredentialManagerState>>,
    storage: tauri::State<'_, Arc<StorageState>>,
    enabled: bool,
    retention_days: u32,
    capacity_mib: u32,
) -> Result<(), String> {
    crate::settings_window::check_settings_ui(&window)?;
    crate::commands::check_auth_required(&credentials)?;
    if enabled && !credentials.background_processing_enabled() {
        return Err("BACKGROUND_PROCESSING_DISABLED".into());
    }
    let limits = Limits {
        retention_days,
        capacity_bytes: u64::from(capacity_mib) * 1024 * 1024,
    };
    limits.validate().map_err(|e| e.to_string())?;
    let storage = storage.inner().clone();
    let recap_app = app.clone();
    tokio::task::spawn_blocking(move || {
        let was_enabled = storage.processing_stage.status().enabled;
        storage.processing_stage.set_policy(enabled, limits)?;
        storage.clear_protected_read_cache();
        if enabled {
            if !was_enabled {
                storage.rotate_protected_write_keys()?;
            }
            storage.request_protected_key_publication();
            crate::ai::recap::protected::sync_after_unlock(&recap_app)?;
        }
        storage.processing_stage.reconcile(&storage)
    })
    .await
    .map_err(|e| e.to_string())??;
    if let Some(ann) = app.try_state::<crate::clip_ann::ClipAnnState>() {
        ann.disarm();
    }
    if let Some(scheduler) =
        app.try_state::<Arc<crate::background_scheduler::BackgroundSchedulerState>>()
    {
        scheduler.wake();
    }
    Ok(())
}

/// Sources come only from the current package or the verified updater staging
/// directory; no arbitrary-source installation command is exposed to the UI.
pub(crate) async fn install_runtime(source: PathBuf, enable: bool) -> Result<(), String> {
    if !supported_build() {
        return Err("APP_BOUND_PACKAGE_UNAVAILABLE".into());
    }
    #[cfg(feature = "app-bound-dev")]
    let action =
        move || carbonpaper_app_bound::development::install_package(&source, enable).map(|_| ());
    #[cfg(not(feature = "app-bound-dev"))]
    let action = move || {
        let (release, _) =
            manifest::read_manifest(&source).map_err(|_| "APP_BOUND_PACKAGE_UNAVAILABLE")?;
        let setup = source.join("carbonpaper-protected-setup.exe");
        // Keep the executable locked across UAC and execution to prevent its
        // replacement between validation and the elevated process opening it.
        let mut executable = identity::locked_file(&setup).map_err(|e| e.to_string())?;
        identity::verify_file(
            &mut executable,
            release
                .files
                .get("carbonpaper-protected-setup.exe")
                .ok_or("Installer is missing")?,
        )
        .map_err(|e| e.to_string())?;
        carbonpaper_app_bound::privileged::verify_loader_policy(&mut executable)
            .map_err(|e| e.to_string())?;
        let mut command = runas::Command::new(&setup);
        command
            .arg("--source")
            .arg(&source)
            .arg("--caller-pid")
            .arg(std::process::id().to_string())
            .gui(true)
            .show(false);
        if enable {
            command.arg("--enable");
        }
        let status = command.status().map_err(|error| -> String {
            if error.raw_os_error() == Some(1223) {
                "APP_BOUND_CANCELLED".into()
            } else {
                "APP_BOUND_INSTALL_FAILED".into()
            }
        })?;
        if !status.success() {
            return Err("APP_BOUND_INSTALL_FAILED".into());
        }
        Ok(())
    };
    tokio::task::spawn_blocking(action)
        .await
        .map_err(|e| e.to_string())?
}

pub(crate) fn schedule_restart(app: &AppHandle) -> Result<(), String> {
    if cfg!(feature = "app-bound-dev") {
        PROTECTED_ENVIRONMENT_READY.store(true, std::sync::atomic::Ordering::Release);
        REPAIR_NEEDED.store(false, std::sync::atomic::Ordering::Release);
        return Ok(());
    }
    let runtime = identity::active_runtime()
        .map_err(|e| e.to_string())?
        .ok_or("Protected runtime is missing")?;
    let helper = runtime.join("carbonpaper-protected-setup.exe");
    let (manifest, _) = manifest::read_manifest(&runtime).map_err(|e| e.to_string())?;
    let mut locked = identity::locked_file(&helper).map_err(|e| e.to_string())?;
    identity::verify_file(
        &mut locked,
        manifest
            .files
            .get("carbonpaper-protected-setup.exe")
            .ok_or("Restart helper is missing")?,
    )
    .map_err(|e| e.to_string())?;
    let context = carbonpaper_app_bound::windows::update::CoordinatorContext::new()
        .map_err(|e| e.to_string())?;
    let mut child = context
        .spawn_restart(
            &helper,
            &[
                "--launch-after".into(),
                std::process::id().to_string().into(),
            ],
            &runtime,
        )
        .map_err(|e| e.to_string())?;
    child.detach();
    crate::IS_UPDATING.store(true, std::sync::atomic::Ordering::Relaxed);
    app.exit(0);
    Ok(())
}

#[tauri::command]
pub async fn app_bound_install(
    window: tauri::Window,
    app: AppHandle,
    credentials: tauri::State<'_, Arc<CredentialManagerState>>,
    enable: bool,
) -> Result<(), String> {
    crate::settings_window::check_settings_ui(&window)?;
    crate::commands::check_auth_required(&credentials)?;
    if enable && !credentials.background_processing_enabled() {
        return Err("BACKGROUND_PROCESSING_DISABLED".into());
    }
    let source = installation_source()?;
    install_runtime(source.clone(), enable).await?;
    if !source.starts_with(identity::protected_root().map_err(|e| e.to_string())?) {
        if let Err(error) =
            crate::registry_config::set_string(PACKAGE_SOURCE, &source.to_string_lossy())
        {
            tracing::warn!("Failed to remember the local component repair source: {error}");
        }
    }
    crate::registry_config::set_bool(OFFER_SEEN, true)?;
    schedule_restart(&app)
}

#[cfg(not(feature = "app-bound-dev"))]
fn repaired_client_restart_required(
    staging: &crate::processing_stage::ProcessingStaging,
    runtime_restart: bool,
) -> Result<bool, String> {
    // A component repair does not reauthorize a quarantined client. Resume with
    // a fresh process, retaining the integrity latch until that process exits.
    if runtime_restart || staging.broker_quarantined() {
        return Ok(true);
    }
    staging
        .refresh()
        .map_err(|_| "APP_BOUND_REPAIR_SERVICE_UNAVAILABLE".to_string())?;
    Ok(false)
}

/// Repair the registered protected runtime from verified local or release sources.
/// Requires a main/settings window and an authenticated session. `package_path`
/// selects an optional ZIP; `allow_full_download` opts into a complete package.
/// Returns a status-tagged outcome with `restart_required` or `size_bytes`.
/// Frontend: `lib/app_bound_api.js` (`repairAppBound`).
#[tauri::command]
pub async fn app_bound_repair(
    window: tauri::Window,
    app: AppHandle,
    credentials: tauri::State<'_, Arc<CredentialManagerState>>,
    storage: tauri::State<'_, Arc<StorageState>>,
    package_path: Option<String>,
    allow_full_download: Option<bool>,
) -> Result<RepairOutcome, String> {
    crate::settings_window::check_settings_ui(&window)?;
    crate::commands::check_auth_required(&credentials)?;
    if !supported_build() {
        return Err("APP_BOUND_PACKAGE_UNAVAILABLE".into());
    }
    #[cfg(feature = "app-bound-dev")]
    {
        let _ = (package_path, allow_full_download);
        install_runtime(installation_source()?, false).await?;
        if storage.processing_stage.broker_quarantined() {
            crate::IS_UPDATING.store(true, std::sync::atomic::Ordering::Relaxed);
            app.request_restart();
        } else {
            schedule_restart(&app)?;
        }
        Ok(RepairOutcome::Repaired {
            restart_required: true,
        })
    }
    #[cfg(not(feature = "app-bound-dev"))]
    {
        let mut outcome = tokio::task::spawn_blocking(move || {
            tauri::async_runtime::block_on(repair::run(
                &window,
                package_path.map(PathBuf::from),
                allow_full_download.unwrap_or(false),
            ))
        })
        .await
        .map_err(|e| e.to_string())??;
        if let RepairOutcome::Repaired { restart_required } = &mut outcome {
            let storage = storage.inner().clone();
            let runtime_restart = *restart_required;
            *restart_required = tokio::task::spawn_blocking(move || {
                let restart =
                    repaired_client_restart_required(&storage.processing_stage, runtime_restart)?;
                if !restart {
                    storage.request_protected_key_publication();
                }
                Ok::<_, String>(restart)
            })
            .await
            .map_err(|e| e.to_string())??;
            if *restart_required {
                schedule_restart(&app)?;
            } else {
                if let Some(scheduler) =
                    app.try_state::<Arc<crate::background_scheduler::BackgroundSchedulerState>>()
                {
                    scheduler.wake();
                }
            }
            REPAIR_NEEDED.store(false, std::sync::atomic::Ordering::Release);
            REPAIR_OFFER_DISMISSED.store(true, std::sync::atomic::Ordering::Release);
        }
        Ok(outcome)
    }
}

#[tauri::command]
pub async fn app_bound_uninstall(
    window: tauri::Window,
    credentials: tauri::State<'_, Arc<CredentialManagerState>>,
    storage: tauri::State<'_, Arc<StorageState>>,
) -> Result<(), String> {
    crate::settings_window::check_settings_ui(&window)?;
    crate::commands::check_auth_required(&credentials)?;
    let storage = storage.inner().clone();
    tokio::task::spawn_blocking(move || {
        let runtime = identity::active_runtime()
            .map_err(|e| e.to_string())?
            .ok_or("Protected runtime is missing")?;
        let helper = runtime.join("carbonpaper-protected-setup.exe");
        let (manifest, _) = manifest::read_manifest(&runtime).map_err(|e| e.to_string())?;
        let mut locked = identity::locked_file(&helper).map_err(|e| e.to_string())?;
        identity::verify_file(
            &mut locked,
            manifest
                .files
                .get("carbonpaper-protected-setup.exe")
                .ok_or("Installer is missing")?,
        )
        .map_err(|e| e.to_string())?;
        carbonpaper_app_bound::privileged::verify_loader_policy(&mut locked)
            .map_err(|e| e.to_string())?;
        let status = runas::Command::new(&helper)
            .arg("--uninstall")
            .arg("--caller-pid")
            .arg(std::process::id().to_string())
            .gui(true)
            .show(false)
            .status()
            .map_err(|error| {
                if error.raw_os_error() == Some(1223) {
                    "APP_BOUND_CANCELLED"
                } else {
                    "APP_BOUND_INSTALL_FAILED"
                }
            })?;
        if !status.success() {
            return Err("APP_BOUND_INSTALL_FAILED".into());
        }
        if let Err(error) = storage.processing_stage.discard_revoked_inputs() {
            tracing::warn!(
                "[APP_BOUND] Local staging cleanup deferred after component removal: {error}"
            );
        }
        let _ = storage.processing_stage.refresh();
        Ok(())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[cfg(all(test, windows, not(feature = "app-bound-dev")))]
#[path = "app_bound_tests.rs"]
mod tests;
