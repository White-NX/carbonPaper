// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::env;
fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() > 1 && args[1] == "--silent-install-python" {
        carbonpaper_lib::run_silent_install();
        return;
    }
    carbonpaper_lib::run();
}
