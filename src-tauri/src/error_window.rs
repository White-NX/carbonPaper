//! Error Window Module
//!
//! Displays critical errors as an overlay in the main window by emitting
//! a `critical-error` event. The frontend renders a full-screen "xibao"
//! (celebration-style) or normal error overlay.
//!
//! A global panic hook is installed via [`install_panic_hook`] so that
//! panics on **any** thread are captured and forwarded to the overlay.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use tauri::{Emitter, Manager};

/// Global flag indicating a critical error has occurred.
/// When true, closing the main window will exit the process instead of hiding.
pub static HAS_CRITICAL_ERROR: AtomicBool = AtomicBool::new(false);

/// Holds the AppHandle so the panic hook can emit events.
static APP_HANDLE: OnceLock<tauri::AppHandle> = OnceLock::new();

/// The most recent critical errors, oldest first.
///
/// The overlay lives in the main window, which lightweight mode destroys. An
/// error reported while it was gone reaches no listener, so a recreated window
/// reads it from here; otherwise it would look healthy while
/// `HAS_CRITICAL_ERROR` stays set.
static CRITICAL_ERRORS: Mutex<VecDeque<CriticalError>> = Mutex::new(VecDeque::new());
/// Lets a window that both reads the record and hears the event show each
/// error once.
static NEXT_CRITICAL_ERROR_ID: AtomicU64 = AtomicU64::new(1);
/// A panic that recurs must not grow the record without bound.
const MAX_RETAINED_CRITICAL_ERRORS: usize = 20;

/// One critical error, as the `critical-error` event carries it.
#[derive(Clone, serde::Serialize)]
pub struct CriticalError {
    id: u64,
    message: String,
}

/// The retained critical errors, oldest first.
pub fn critical_errors() -> Vec<CriticalError> {
    CRITICAL_ERRORS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .cloned()
        .collect()
}

fn retain_critical_error(message: &str) -> CriticalError {
    let error = CriticalError {
        id: NEXT_CRITICAL_ERROR_ID.fetch_add(1, Ordering::SeqCst),
        message: message.to_string(),
    };
    let mut retained = CRITICAL_ERRORS.lock().unwrap_or_else(|e| e.into_inner());
    if retained.len() == MAX_RETAINED_CRITICAL_ERRORS {
        retained.pop_front();
    }
    retained.push_back(error.clone());
    error
}

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
            format!(
                "{}\n  at {}:{}:{}",
                message,
                loc.file(),
                loc.line(),
                loc.column()
            )
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
    let error = retain_critical_error(message);

    // Emit event to the main window (frontend will render overlay)
    let _ = app.emit("critical-error", &error);

    // Ensure main window is visible and focused
    if let Some(main_win) = app.get_webview_window("main") {
        let _ = main_win.show();
        let _ = main_win.set_focus();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_record_keeps_the_most_recent_errors_oldest_first() {
        let ids: Vec<u64> = (0..MAX_RETAINED_CRITICAL_ERRORS + 5)
            .map(|index| retain_critical_error(&format!("panic {index}")).id)
            .collect();

        let retained = critical_errors();
        assert_eq!(retained.len(), MAX_RETAINED_CRITICAL_ERRORS);
        assert_eq!(retained.first().map(|error| error.id), Some(ids[5]));
        let newest = format!("panic {}", MAX_RETAINED_CRITICAL_ERRORS + 4);
        assert_eq!(
            retained.last().map(|error| error.message.as_str()),
            Some(newest.as_str())
        );
        assert!(retained.windows(2).all(|pair| pair[0].id < pair[1].id));
    }
}
