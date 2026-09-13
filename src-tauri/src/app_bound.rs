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

const OFFER_SEEN: &str = "app_bound_offer_seen";
static REPAIR_NEEDED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static REPAIR_OFFER_DISMISSED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);
static PROTECTED_ENVIRONMENT_READY: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

#[derive(serde::Serialize)]
pub struct AppBoundStatus {
    #[serde(flatten)]
    pub processing: ProcessingStatus,
    pub offer_enable: bool,
    pub package_available: bool,
}

fn executable_directory() -> Result<PathBuf, String> {
    let executable = std::env::current_exe().map_err(|e| e.to_string())?;
    executable
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| "Application directory is unavailable".into())
}

pub(crate) fn supported_build() -> bool {
    !cfg!(debug_assertions) || cfg!(feature = "app-bound-dev")
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
/// copies remain launchers for the registered protected runtime.
pub fn delegate_startup(args: &[String]) -> Result<bool, String> {
    #[cfg(feature = "app-bound-dev")]
    {
        if !args
            .iter()
            .any(|arg| matches!(arg.as_str(), "--cng-unlock" | "--silent-install-python"))
        {
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
        return Ok(false);
    }
    #[cfg(not(feature = "app-bound-dev"))]
    match try_delegate_startup(args) {
        Ok(result) => Ok(result),
        Err(_) => {
            // Keep archive capture and the repair UI usable. The unprotected
            // copy cannot pass the service's process identity verification.
            REPAIR_NEEDED.store(true, std::sync::atomic::Ordering::Release);
            Ok(false)
        }
    }
}

#[cfg(not(feature = "app-bound-dev"))]
fn try_delegate_startup(args: &[String]) -> Result<bool, String> {
    if cfg!(debug_assertions)
        || args
            .iter()
            .any(|arg| matches!(arg.as_str(), "--cng-unlock" | "--silent-install-python"))
    {
        return Ok(false);
    }
    if args.iter().any(|arg| arg == "--repair-protected-runtime") {
        REPAIR_NEEDED.store(true, std::sync::atomic::Ordering::Release);
        return Ok(false);
    }
    let Some(active) = identity::active_runtime().map_err(|e| e.to_string())? else {
        return Ok(false);
    };
    if identity::path_eq(&active, &executable_directory()?) {
        sanitize_runtime_environment()?;
        PROTECTED_ENVIRONMENT_READY.store(true, std::sync::atomic::Ordering::Release);
        return Ok(false);
    }
    let (release, _) = manifest::read_manifest(&active).map_err(|e| e.to_string())?;
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
fn sanitize_runtime_environment() -> Result<(), String> {
    for key in [
        "WEBVIEW2_BROWSER_EXECUTABLE_FOLDER",
        "WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS",
        "WEBVIEW2_USER_DATA_FOLDER",
        "CARBONPAPER_ORT_DYLIB_PATH",
        "ORT_DYLIB_PATH",
    ] {
        std::env::remove_var(key);
    }
    let executable = executable_directory()?;
    let system = identity::system_directory().map_err(|e| e.to_string())?;
    std::env::set_current_dir(&executable).map_err(|e| e.to_string())?;
    std::env::set_var(
        "PATH",
        std::env::join_paths([
            executable,
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
    crate::commands::check_main_window(&window)?;
    let storage = storage.inner().clone();
    tokio::task::spawn_blocking(move || {
        if storage.background_processing_enabled() {
            let _ = storage.processing_stage.refresh();
        } else {
            let _ = storage.processing_stage.disable_if_installed();
        }
        let mut processing = storage.processing_stage.status();
        let repair = REPAIR_NEEDED.load(std::sync::atomic::Ordering::Acquire);
        if repair {
            processing.installed = true;
            processing.available = false;
            processing.reason = Some("repair_required".into());
        }
        let package_available = supported_build()
            && installation_source()
                .ok()
                .is_some_and(|dir| manifest::read_manifest(&dir).is_ok());
        let offer_enable = package_available
            && ((repair && !REPAIR_OFFER_DISMISSED.load(std::sync::atomic::Ordering::Acquire))
                || (storage.background_processing_enabled()
                    && !processing.installed
                    && !crate::registry_config::get_bool(OFFER_SEEN).unwrap_or(false)));
        Ok(AppBoundStatus {
            processing,
            package_available,
            offer_enable,
        })
    })
    .await
    .map_err(|e| e.to_string())?
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
    crate::commands::check_main_window(&window)?;
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
    tokio::task::spawn_blocking(move || {
        storage.processing_stage.set_policy(enabled, limits)?;
        storage.processing_stage.reconcile(&storage)
    })
    .await
    .map_err(|e| e.to_string())??;
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
    use std::os::windows::process::CommandExt;
    std::process::Command::new(&helper)
        .arg("--launch-after")
        .arg(std::process::id().to_string())
        .current_dir(runtime)
        .creation_flags(0x08000000)
        .spawn()
        .map_err(|e| e.to_string())?;
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
    crate::commands::check_main_window(&window)?;
    crate::commands::check_auth_required(&credentials)?;
    if enable && !credentials.background_processing_enabled() {
        return Err("BACKGROUND_PROCESSING_DISABLED".into());
    }
    install_runtime(installation_source()?, enable).await?;
    crate::registry_config::set_bool(OFFER_SEEN, true)?;
    schedule_restart(&app)
}

#[tauri::command]
pub async fn app_bound_uninstall(
    window: tauri::Window,
    credentials: tauri::State<'_, Arc<CredentialManagerState>>,
    storage: tauri::State<'_, Arc<StorageState>>,
) -> Result<(), String> {
    crate::commands::check_main_window(&window)?;
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
        storage.processing_stage.discard_revoked_inputs()?;
        let _ = storage.processing_stage.refresh();
        Ok(())
    })
    .await
    .map_err(|e| e.to_string())?
}
