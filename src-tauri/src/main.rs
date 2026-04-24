// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::env;
fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() > 1 && args[1] == "--silent-install-python" {
        carbonpaper_lib::run_silent_install();
        return;
    }
    if args.len() > 2 && args[1] == "--cng-unlock" {
        carbonpaper_lib::run_cng_unlock(&args[2]);
        return;
    }

    // 新增：隐藏启动（用于自动启动和轻量模式）
    if args.contains(&"--hidden".to_string()) {
        std::env::set_var("CARBONPAPER_START_HIDDEN", "1");
    }

    carbonpaper_lib::run();
}
