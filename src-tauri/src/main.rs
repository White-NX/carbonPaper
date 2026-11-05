//! CarbonPaper desktop executable entry point.

// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::env;
fn main() {
    let args: Vec<String> = env::args().collect();
    if args
        .get(1)
        .is_some_and(|arg| arg == "--verify-update-manifest")
    {
        if args.len() != 3 {
            std::process::exit(2);
        }
        if let Err(error) =
            carbonpaper_lib::verify_update_manifest_file(std::path::Path::new(&args[2]))
        {
            eprintln!("{error}");
            std::process::exit(1);
        }
        return;
    }
    match carbonpaper_lib::delegate_protected_runtime(&args) {
        Ok(true) => return,
        Ok(false) => {}
        Err(error) => {
            eprintln!("Cannot open protected CarbonPaper: {error}");
            return;
        }
    }
    if args.len() > 2 && args[1] == "--cng-unlock" {
        let owner_hwnd = args.get(3).and_then(|value| value.parse::<isize>().ok());
        carbonpaper_lib::run_cng_unlock(&args[2], owner_hwnd);
        return;
    }

    if args.contains(&"--hidden".to_string()) {
        std::env::set_var("CARBONPAPER_START_HIDDEN", "1");
    }

    carbonpaper_lib::run();
}
