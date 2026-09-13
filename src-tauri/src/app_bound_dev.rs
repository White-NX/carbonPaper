//! Real app-bound service startup for the ordinary `npm run debug` workflow.
use carbonpaper_app_bound::{
    development, manifest,
    protocol::Request,
    windows::{self, identity},
};

pub fn initialize() -> Result<(), String> {
    // Tauri restarts the desktop after a Rust edit. Refresh the small native
    // helpers here too, so service changes are not left on the previous build.
    use std::os::windows::process::CommandExt;
    let status = std::process::Command::new("node")
        .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/../scripts/debug.mjs"))
        .arg("--prepare-native")
        .creation_flags(0x08000000)
        .status()
        .map_err(|e| format!("Cannot prepare development components: {e}"))?;
    if !status.success() {
        return Err("Development component build failed; see the debug terminal".into());
    }
    let package = development::prepare_package(env!("CARGO_PKG_VERSION"))?;
    let (_, package_id) = manifest::read_manifest(&package).map_err(|e| e.to_string())?;
    let registration_matches = identity::active_runtime()
        .ok()
        .flatten()
        .is_some_and(|path| path.file_name().is_some_and(|id| id == package_id.as_str()));
    let caller_matches = identity::inspect_process(std::process::id())
        .and_then(identity::verify_main)
        .is_ok();
    if !registration_matches || !caller_matches || windows::call(Request::Status {}).is_err() {
        eprintln!(
            "[app-bound dev] Registering this debug build with the isolated Windows service..."
        );
        development::install_package(&package, false)?;
    }
    windows::call(Request::Status {})
        .map_err(|e| format!("Development service handshake failed: {e}"))?;
    let data_dir = crate::get_data_dir();
    let ledger = identity::state_root()
        .map_err(|e| format!("Cannot resolve development ledger directory: {e}"))?
        .join("keys.db");
    eprintln!(
        "[app-bound dev] Ready: service={} instance={} data_dir={} ledger={}",
        carbonpaper_app_bound::protocol::SERVICE_NAME,
        development::INSTANCE_ID,
        data_dir.display(),
        ledger.display()
    );
    Ok(())
}
