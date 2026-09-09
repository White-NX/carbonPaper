#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
fn main() {
    #[cfg(windows)]
    if carbonpaper_app_bound::windows::service::run().is_err() {
        std::process::exit(1);
    }
    #[cfg(not(windows))]
    std::process::exit(1);
}
