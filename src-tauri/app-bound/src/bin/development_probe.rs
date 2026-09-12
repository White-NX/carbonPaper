use carbonpaper_app_bound::{
    crypto, development, manifest,
    protocol::*,
    windows::{self, identity},
};

fn main() {
    if std::env::args().any(|arg| arg == "--expect-rejection") {
        if windows::call(Request::Status {}).is_ok() {
            eprintln!("An unregistered executable path was accepted");
            std::process::exit(1);
        }
        return;
    }
    if let Err(error) = run() {
        eprintln!("Development broker smoke test failed: {error}");
        std::process::exit(1);
    }
}

fn run() -> std::result::Result<(), String> {
    // SAFETY: this only queries the caller's token. The smoke client must prove
    // the standard-user side of the SYSTEM boundary; helpers elevate separately.
    if unsafe { ::windows::Win32::UI::Shell::IsUserAnAdmin() }.as_bool() {
        return Err("Run the development smoke check from a normal, unelevated terminal".into());
    }
    // Use the application's version so repeated probe runs also exercise the
    // same manifest and installer version checks as the interactive instance.
    let config: serde_json::Value = serde_json::from_str(include_str!("../../../tauri.conf.json"))
        .map_err(|e| e.to_string())?;
    let version = config["version"]
        .as_str()
        .ok_or("Application version is missing")?;
    let package = development::prepare_package(version)?;
    println!("[app-bound dev] Installing a separate LocalSystem test instance...");
    development::install_package(&package, false)?;
    let result = exercise(&package);
    let cleanup = uninstall(&package);
    result?;
    cleanup?;
    println!(
        "[app-bound dev] PASS: real service identity, pipe, policy, DPAPI, repair and removal"
    );
    Ok(())
}

fn call(request: Request) -> std::result::Result<Response, String> {
    windows::call(request).map_err(|error| error.to_string())
}

fn exercise(package: &std::path::Path) -> std::result::Result<(), String> {
    match std::fs::File::open(
        identity::state_root()
            .map_err(|e| e.to_string())?
            .join("keys.db"),
    ) {
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {}
        Err(error) => return Err(format!("Expected SYSTEM ledger access denial: {error}")),
        Ok(_) => return Err("The ordinary client could read the SYSTEM key database".into()),
    }
    let Response::Status(initial) = call(Request::Status {})? else {
        return Err("Expected status".into());
    };
    if let Some(dataset_id) = initial.dataset_id {
        call(Request::RetireDataset { dataset_id })?;
    }
    call(Request::SetPolicy {
        enabled: false,
        limits: Limits::default(),
    })?;
    let Response::Status(enabled) = call(Request::SetPolicy {
        enabled: true,
        limits: Limits::default(),
    })?
    else {
        return Err("Expected enabled status".into());
    };
    if !enabled.enabled {
        return Err("Policy was not enabled".into());
    }
    let dataset_id = random_id();
    call(Request::AttachDataset {
        dataset_id: dataset_id.clone(),
    })?;
    let plaintext = b"isolated development smoke input";
    let Response::Prepared(prepared) = call(Request::PrepareTask {
        dataset_id,
        screenshot_id: 1,
        consumers: 7,
        payload_bytes: plaintext.len() as u64 + 28,
    })?
    else {
        return Err("Expected prepared key".into());
    };
    let ciphertext = crypto::encrypt_input(&prepared.key, &prepared.task, plaintext)
        .map_err(|e| e.to_string())?;
    call(Request::ActivateTask {
        task_id: prepared.task.task_id.clone(),
        ciphertext_digest: crypto::digest(&ciphertext),
    })?;
    println!("[app-bound dev] Repairing the running service and recovering its persisted task...");
    development::install_package(package, false)?;
    for consumer in [Consumer::Classification, Consumer::MiniLm, Consumer::Clip] {
        let Response::Lease(lease) = call(Request::AcquireTask {
            task_id: prepared.task.task_id.clone(),
            consumer,
            ciphertext_digest: crypto::digest(&ciphertext),
        })?
        else {
            return Err("Expected recovered lease".into());
        };
        let decoded = crypto::decrypt_input(&lease.key, &lease.task, &ciphertext)
            .map_err(|e| e.to_string())?;
        if decoded.as_slice() != plaintext {
            return Err("Recovered input differs".into());
        }
        call(Request::FinishConsumer {
            task_id: lease.task.task_id,
            consumer,
            lease_id: lease.lease_id,
        })?;
    }
    let Response::TaskState(completed) = call(Request::InspectTask {
        task_id: prepared.task.task_id,
    })?
    else {
        return Err("Expected completion state".into());
    };
    if !completed.retired || completed.finished_consumers != 7 {
        return Err("Task did not complete".into());
    }
    let unregistered = development::user_root()
        .map_err(|e| e.to_string())?
        .join("unregistered-probe");
    std::fs::create_dir_all(&unregistered).map_err(|e| e.to_string())?;
    let copy = unregistered.join("carbonpaper.exe");
    std::fs::copy(std::env::current_exe().map_err(|e| e.to_string())?, &copy)
        .map_err(|e| e.to_string())?;
    let denied = std::process::Command::new(&copy)
        .arg("--expect-rejection")
        .status()
        .map_err(|e| e.to_string())?;
    if !denied.success() {
        return Err("Executable-path isolation failed".into());
    }
    // A stopped or broken service must not make the rejection check pass.
    call(Request::Status {})?;
    Ok(())
}

fn uninstall(package: &std::path::Path) -> std::result::Result<(), String> {
    let (release, _) = manifest::read_manifest(package).map_err(|e| e.to_string())?;
    let helper = package.join("carbonpaper-protected-setup.exe");
    let mut locked = identity::locked_file(&helper).map_err(|e| e.to_string())?;
    identity::verify_file(
        &mut locked,
        &release.files["carbonpaper-protected-setup.exe"],
    )
    .map_err(|e| e.to_string())?;
    let status = runas::Command::new(&helper)
        .arg("--uninstall")
        .arg("--caller-pid")
        .arg(std::process::id().to_string())
        .gui(true)
        .show(false)
        .status()
        .map_err(|e| e.to_string())?;
    if !status.success() {
        return Err("Could not remove the development test service".into());
    }
    if identity::active_runtime()
        .map_err(|e| e.to_string())?
        .is_some()
    {
        return Err("Test activation remained after removal".into());
    }
    Ok(())
}
