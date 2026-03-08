//! Error Window Module
//!
//! Displays critical errors as an overlay in the main window by emitting
//! a `critical-error` event. The frontend renders a full-screen "xibao"
//! (celebration-style) or normal error overlay.
//!
//! A global panic hook is installed via [`install_panic_hook`] so that
//! panics on **any** thread are captured and forwarded to the overlay.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;
use tauri::{Emitter, Manager};

/// Global flag indicating a critical error has occurred.
/// When true, closing the main window will exit the process instead of hiding.
pub static HAS_CRITICAL_ERROR: AtomicBool = AtomicBool::new(false);

/// Holds the AppHandle so the panic hook can emit events.
static APP_HANDLE: OnceLock<tauri::AppHandle> = OnceLock::new();

/// Store the AppHandle for use by the global panic hook.
/// Must be called once during app setup.
pub fn set_app_handle(app: tauri::AppHandle) {
    let _ = APP_HANDLE.set(app);
}

/// Install a global panic hook that forwards panic messages to the error overlay.
///
/// This captures panics from all threads — Tauri commands, spawned tasks,
/// background threads, etc. Should be called once during app setup.
///
/// The hook does NOT call the previous hook or park the thread. After showing
/// the overlay it returns normally, allowing `catch_unwind` (used internally
/// by Tokio's task/spawn_blocking executor) to catch the panic so the
/// thread and process survive.
pub fn install_panic_hook() {
    // Discard the previous hook — it may call process::exit or abort.
    let _ = std::panic::take_hook();
    std::panic::set_hook(Box::new(|panic_info| {
        // Extract a human-readable message from the panic payload
        let message = if let Some(msg) = panic_info.payload().downcast_ref::<String>() {
            msg.clone()
        } else if let Some(msg) = panic_info.payload().downcast_ref::<&str>() {
            msg.to_string()
        } else {
            "Unknown panic".to_string()
        };

        // Include location if available
        let full_message = if let Some(loc) = panic_info.location() {
            format!("{}\n  at {}:{}:{}", message, loc.file(), loc.line(), loc.column())
        } else {
            message
        };

        // Log to tracing (goes to log file) and stderr
        tracing::error!("PANIC captured: {}", full_message);
        eprintln!("PANIC: {}", full_message);

        // Try to show overlay via the stored AppHandle
        if let Some(app) = APP_HANDLE.get() {
            show_error_window(app, &full_message);
        }

        // Return normally — let catch_unwind (Tokio / our own) catch the
        // panic so the thread survives. Do NOT park the thread (blocks
        // Tokio's thread pool) or call the previous hook (may exit).
    }));
}

/// Show a critical error in the main window overlay.
/// If called multiple times, each error is appended via repeated events.
pub fn show_error_window(app: &tauri::AppHandle, message: &str) {
    tracing::error!("Critical error, showing error overlay: {}", message);

    HAS_CRITICAL_ERROR.store(true, Ordering::SeqCst);

    // Emit event to the main window (frontend will render overlay)
    let _ = app.emit(
        "critical-error",
        serde_json::json!({ "message": message }),
    );

    // Ensure main window is visible and focused
    if let Some(main_win) = app.get_webview_window("main") {
        let _ = main_win.show();
        let _ = main_win.set_focus();
    }
}
