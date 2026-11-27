//! Presentation of the main window while its WebView is starting.
//!
//! A foreground window stays hidden until the static startup screen can paint.
//! Show intent belongs to the application, so a late page-ready callback cannot
//! undo hiding the window or present a previously destroyed WebView.

use std::sync::Mutex;
use tauri::{webview::PageLoadEvent, Manager};

pub struct MainWindowPresentationState {
    creation: Mutex<()>,
    presentation: Mutex<Presentation>,
}

struct Presentation {
    generation: u64,
    ready: bool,
    show_requested: bool,
}

impl MainWindowPresentationState {
    pub fn new(show_requested: bool) -> Self {
        Self {
            creation: Mutex::new(()),
            presentation: Mutex::new(Presentation {
                generation: 0,
                ready: false,
                show_requested,
            }),
        }
    }

    /// Keep lookup and construction together so overlapping opens reuse the
    /// same WebView without advancing its startup generation.
    pub fn create_if_missing<E>(
        &self,
        exists: impl FnOnce() -> bool,
        create: impl FnOnce(u64) -> Result<(), E>,
    ) -> Result<bool, E> {
        // Page-ready callbacks use only the presentation lock and must remain
        // free to run while the WebView builder waits for the event loop.
        let _creation = self.creation.lock().unwrap_or_else(|e| e.into_inner());
        if exists() {
            return Ok(false);
        }
        create(self.begin_creation())?;
        Ok(true)
    }

    fn begin_creation(&self) -> u64 {
        let mut presentation = self.presentation.lock().unwrap_or_else(|e| e.into_inner());
        presentation.generation = presentation.generation.wrapping_add(1);
        presentation.ready = false;
        presentation.show_requested = true;
        presentation.generation
    }

    pub fn destroyed(&self) {
        let mut presentation = self.presentation.lock().unwrap_or_else(|e| e.into_inner());
        presentation.ready = false;
        presentation.show_requested = false;
    }
}

impl Presentation {
    fn acknowledge(&mut self, generation: u64) -> bool {
        if self.generation != generation || self.ready {
            return false;
        }
        self.ready = true;
        self.show_requested
    }
}

pub fn request_show(app: &tauri::AppHandle) -> Result<(), String> {
    let state = app.state::<MainWindowPresentationState>();
    let mut presentation = state.presentation.lock().unwrap_or_else(|e| e.into_inner());
    presentation.show_requested = true;
    if presentation.ready {
        if let Some(window) = app.get_webview_window("main") {
            window.unminimize().map_err(|e| e.to_string())?;
            window.show().map_err(|e| e.to_string())?;
            window.set_focus().map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

pub fn hide(window: &tauri::Window) -> Result<(), String> {
    let state = window.app_handle().state::<MainWindowPresentationState>();
    let mut presentation = state.presentation.lock().unwrap_or_else(|e| e.into_inner());
    presentation.show_requested = false;
    window.hide().map_err(|e| e.to_string())
}

fn acknowledge(app: &tauri::AppHandle, generation: u64) -> Result<(), String> {
    let state = app.state::<MainWindowPresentationState>();
    let mut presentation = state.presentation.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(window) = app.get_webview_window("main") {
        if presentation.acknowledge(generation) {
            window.show().map_err(|e| e.to_string())?;
            if !window.is_minimized().unwrap_or(false) {
                window.set_focus().map_err(|e| e.to_string())?;
            }
        }
    }
    Ok(())
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StartupLabels {
    tagline: String,
    minimize: String,
    hide_to_tray: String,
}

/// Presents a loaded startup screen and returns its localized labels.
///
/// Authentication: main-window origin required; exposes no history or keys.
/// Frontend: `public/startup.js`, before the React entry point loads.
#[tauri::command]
pub fn main_window_ready(
    window: tauri::Window,
    generation: u64,
    language: String,
) -> Result<StartupLabels, String> {
    crate::commands::check_main_window(&window)?;
    acknowledge(window.app_handle(), generation)?;
    Ok(StartupLabels {
        tagline: crate::i18n::t(&language, "startupScreen.tagline"),
        minimize: crate::i18n::t(&language, "startupScreen.minimize"),
        hide_to_tray: crate::i18n::t(&language, "topbar.hideToTray"),
    })
}

/// Full page load is a fallback if the early script could not notify Rust.
/// Duplicate callbacks preserve a user's subsequent minimize or hide action.
pub fn on_page_load(webview: &tauri::Webview, payload: &tauri::webview::PageLoadPayload<'_>) {
    if webview.label() != "main" || payload.event() != PageLoadEvent::Finished {
        return;
    }
    let generation = payload
        .url()
        .query_pairs()
        .find(|(key, _)| key == "startup")
        .and_then(|(_, value)| value.parse().ok())
        .unwrap_or(0);
    if let Err(error) = acknowledge(webview.app_handle(), generation) {
        tracing::warn!("Failed to present the loaded main window: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hiding_before_page_ready_is_preserved_and_later_open_is_possible() {
        let state = MainWindowPresentationState::new(true);
        let mut presentation = state.presentation.lock().unwrap();
        assert!(!presentation.ready);
        presentation.show_requested = false;
        assert!(!presentation.acknowledge(0));
        assert!(presentation.ready);
        presentation.show_requested = true;
        assert!(presentation.ready && presentation.show_requested);
    }

    #[test]
    fn duplicate_page_load_does_not_present_a_minimized_or_hidden_window_again() {
        let state = MainWindowPresentationState::new(true);
        let mut presentation = state.presentation.lock().unwrap();
        assert!(presentation.acknowledge(0));
        assert!(!presentation.acknowledge(0));
        presentation.show_requested = false;
        assert!(!presentation.acknowledge(0));
    }

    #[test]
    fn recreated_window_ignores_ready_callbacks_from_the_previous_webview() {
        let state = MainWindowPresentationState::new(true);
        assert!(state.presentation.lock().unwrap().acknowledge(0));
        state.destroyed();
        let generation = state.begin_creation();
        let mut presentation = state.presentation.lock().unwrap();
        assert!(!presentation.acknowledge(0));
        assert!(!presentation.ready);
        assert!(presentation.acknowledge(generation));
    }

    #[test]
    fn concurrent_opens_share_one_window_and_startup_generation() {
        use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
        use std::sync::Barrier;

        let state = MainWindowPresentationState::new(false);
        let window_generation = AtomicU64::new(0);
        let creations = AtomicUsize::new(0);
        let start = Barrier::new(8);
        let results = std::thread::scope(|scope| {
            let requests: Vec<_> = (0..8)
                .map(|_| {
                    scope.spawn(|| {
                        start.wait();
                        state
                            .create_if_missing(
                                || window_generation.load(Ordering::Acquire) != 0,
                                |generation| {
                                    creations.fetch_add(1, Ordering::Relaxed);
                                    // Keep the window absent while other opens arrive.
                                    std::thread::sleep(std::time::Duration::from_millis(20));
                                    window_generation.store(generation, Ordering::Release);
                                    Ok::<(), ()>(())
                                },
                            )
                            .unwrap()
                    })
                })
                .collect();
            requests
                .into_iter()
                .map(|request| request.join().unwrap())
                .collect::<Vec<_>>()
        });

        assert_eq!(creations.load(Ordering::Relaxed), 1);
        assert_eq!(results.into_iter().filter(|created| *created).count(), 1);
        assert_eq!(window_generation.load(Ordering::Acquire), 1);
        assert!(state.presentation.lock().unwrap().acknowledge(1));
    }

    #[test]
    fn failed_creation_allows_retry_with_a_new_startup_generation() {
        let state = MainWindowPresentationState::new(false);
        let result = state.create_if_missing(|| false, |_| Err("WebView failed"));
        assert_eq!(result, Err("WebView failed"));

        assert!(state
            .create_if_missing(
                || false,
                |generation| {
                    assert_eq!(generation, 2);
                    Ok::<(), &str>(())
                }
            )
            .unwrap());
        let mut presentation = state.presentation.lock().unwrap();
        assert!(!presentation.acknowledge(1));
        assert!(presentation.acknowledge(2));
    }
}
