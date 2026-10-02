//! First-run wizard and "what's new" state.
//!
//! One integer in the registry, `onboarding_version`, records the newest
//! wizard the user has finished. It replaces the per-prompt flags the separate
//! extension and smart-cluster wizards used to keep.
//!
//! A user is shown one of three things:
//!
//! - the full wizard, on a genuinely new installation;
//! - a short "what's new" page, when an older build was already in use or an
//!   earlier wizard version was finished;
//! - nothing, once the current version is recorded.
//!
//! Capture does not start on its own while the full wizard is pending, so a new
//! user decides what is recorded before anything is. Frontend:
//! `src/components/onboarding/`.

use crate::registry_config;
use std::path::Path;

/// Bump when the wizard gains a step or a "what's new" entry that existing
/// users should see once.
pub const ONBOARDING_VERSION: u32 = 1;

const VERSION_KEY: &str = "onboarding_version";

/// Flags written by builds before the unified wizard. Any of them means the
/// app has been used before.
const LEGACY_FLAGS: [&str; 3] = [
    "extension_setup_done",
    "smart_cluster_setup_done",
    "smart_cluster_setup_dismissed",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OnboardingMode {
    Full,
    WhatsNew,
    None,
}

#[derive(Debug, serde::Serialize)]
pub struct OnboardingState {
    pub mode: OnboardingMode,
    pub current_version: u32,
    pub completed_version: u32,
}

fn decide_mode(completed_version: u32, existing_user: bool) -> OnboardingMode {
    if completed_version >= ONBOARDING_VERSION {
        OnboardingMode::None
    } else if completed_version == 0 && !existing_user {
        OnboardingMode::Full
    } else {
        OnboardingMode::WhatsNew
    }
}

/// True when the directory holds at least one file anywhere below it. Stops at
/// the first file, so a large history costs one directory read per level.
fn contains_any_file(dir: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    for entry in entries.flatten() {
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_file() || (kind.is_dir() && contains_any_file(&entry.path())) {
            return true;
        }
    }
    false
}

/// Whether this installation was used before the unified wizard existed.
/// Screenshots on disk are checked instead of the database because the
/// database cannot be read before the user unlocks it.
fn is_existing_user() -> bool {
    LEGACY_FLAGS
        .iter()
        .any(|flag| registry_config::get_bool(flag).unwrap_or(false))
        || contains_any_file(&crate::get_data_dir().join("screenshots"))
}

pub fn current_state() -> OnboardingState {
    let completed_version = registry_config::get_u32(VERSION_KEY).unwrap_or(0);
    let mode = if completed_version >= ONBOARDING_VERSION {
        OnboardingMode::None
    } else {
        decide_mode(completed_version, is_existing_user())
    };
    OnboardingState {
        mode,
        current_version: ONBOARDING_VERSION,
        completed_version,
    }
}

/// Capture must wait while a new user has not finished the full wizard.
pub fn capture_held() -> bool {
    current_state().mode == OnboardingMode::Full
}

/// Returns which wizard, if any, the main window should show.
///
/// Authentication: not required. Returns `{ mode, current_version,
/// completed_version }` where `mode` is `"full"`, `"whats_new"` or `"none"`.
#[tauri::command]
pub fn get_onboarding_state() -> Result<OnboardingState, String> {
    Ok(current_state())
}

/// Records that the wizard for `version` was finished. Versions above the
/// current one are clamped, and a lower version never overwrites a higher one.
///
/// Authentication: not required. Main window only. Returns JSON `null`.
#[tauri::command]
pub fn complete_onboarding(window: tauri::Window, version: u32) -> Result<(), String> {
    crate::commands::check_main_window(&window)?;
    let version = version.min(ONBOARDING_VERSION);
    let previous = registry_config::get_u32(VERSION_KEY).unwrap_or(0);
    if version > previous {
        registry_config::set_u32(VERSION_KEY, version)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_installation_gets_the_full_wizard() {
        assert_eq!(decide_mode(0, false), OnboardingMode::Full);
    }

    #[test]
    fn earlier_users_get_whats_new_once() {
        assert_eq!(decide_mode(0, true), OnboardingMode::WhatsNew);
        assert_eq!(
            decide_mode(ONBOARDING_VERSION - 1, true),
            OnboardingMode::WhatsNew
        );
        assert_eq!(decide_mode(ONBOARDING_VERSION, true), OnboardingMode::None);
    }

    #[test]
    fn finishing_an_older_wizard_counts_as_an_existing_user() {
        // Someone who completed version N and meets version N + 1 has used the
        // app; they get the short page even without legacy flags.
        if ONBOARDING_VERSION > 1 {
            assert_eq!(decide_mode(1, false), OnboardingMode::WhatsNew);
        }
        assert_eq!(
            decide_mode(ONBOARDING_VERSION + 3, false),
            OnboardingMode::None
        );
    }

    #[test]
    fn finds_files_in_nested_directories_only() {
        let root = std::env::temp_dir().join(format!("cp-onboarding-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("2026/09/thumbs")).unwrap();
        assert!(!contains_any_file(&root));
        assert!(!contains_any_file(&root.join("missing")));
        std::fs::write(root.join("2026/09/shot.bin"), b"x").unwrap();
        assert!(contains_any_file(&root));
        let _ = std::fs::remove_dir_all(&root);
    }
}
