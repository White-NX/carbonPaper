//! Unprivileged Python host. Deliberately has a different image identity from
//! the desktop executable: it must never qualify as an app-bound key client.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[path = "../python_launcher.rs"]
mod python_launcher;

fn main() {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    std::process::exit(python_launcher::run_python_launcher(&args));
}
