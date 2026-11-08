//! Startup regressions use synthetic directories and isolated child processes.
//! No application activation, service, credentials, or screenshot store is changed.

use super::*;
use std::os::windows::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};
use windows::{
    core::PCWSTR,
    Win32::{Foundation::FreeLibrary, System::LibraryLoader::LoadLibraryW},
};

const CHILD_TEST: &str = "app_bound::tests::native_startup_child";
const OVERRIDE_VALUE: &str = "synthetic-inherited-override";
const OVERRIDES: [&str; 5] = [
    "WEBVIEW2_BROWSER_EXECUTABLE_FOLDER",
    "WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS",
    "WEBVIEW2_USER_DATA_FOLDER",
    "CARBONPAPER_ORT_DYLIB_PATH",
    "ORT_DYLIB_PATH",
];

fn run_child(mode: &str) {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", CHILD_TEST, "--ignored", "--nocapture"])
        .env("CARBONPAPER_STARTUP_TEST_MODE", mode)
        .env("PATH", OVERRIDE_VALUE)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(0x0800_0000);
    for key in OVERRIDES {
        command.env(key, OVERRIDE_VALUE);
    }
    let mut child = command.spawn().unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    while child.try_wait().unwrap().is_none() {
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("isolated startup child timed out: {mode}");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "startup child {mode} failed: {} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

struct RestoreDirectory(PathBuf);
impl Drop for RestoreDirectory {
    fn drop(&mut self) {
        let _ = std::env::set_current_dir(&self.0);
    }
}

fn assert_sanitized(directory: &Path) {
    for key in OVERRIDES {
        assert!(std::env::var_os(key).is_none(), "override survived: {key}");
    }
    assert!(identity::path_eq(
        &std::env::current_dir().unwrap(),
        directory
    ));
    let system = identity::system_directory().unwrap();
    let actual: Vec<_> = std::env::split_paths(&std::env::var_os("PATH").unwrap()).collect();
    assert_eq!(
        actual,
        [
            directory.to_path_buf(),
            system.clone(),
            system.join("WindowsPowerShell/v1.0"),
            system.join("Wbem"),
        ]
    );
}

// Invoked only by the owned subprocesses above: DLL-search policy, environment,
// cwd and startup flags must never leak into the parallel Rust test runner.
#[test]
#[ignore]
fn native_startup_child() {
    let Ok(mode) = std::env::var("CARBONPAPER_STARTUP_TEST_MODE") else {
        return;
    };
    let temporary = tempfile::tempdir().unwrap();
    let _restore = RestoreDirectory(std::env::current_dir().unwrap());
    let root = temporary.path().join("Protected");
    let runtime = root.join("Runtime").join("synthetic-release");
    let inherited = temporary.path().join("Inherited");
    let portable = temporary.path().join("Portable");
    for directory in [&runtime, &inherited, &portable] {
        std::fs::create_dir_all(directory).unwrap();
    }
    std::env::set_current_dir(&inherited).unwrap();
    assert!(!protected_environment_ready());
    assert!(!REPAIR_NEEDED.load(Ordering::Acquire));

    if mode == "sanitize_error" {
        let missing = root.join("Runtime").join("missing-release");
        let result = delegate_startup_in_directory(&missing, &root, || {
            panic!("failed sanitization must not run startup or recovery")
        });
        assert!(result.is_err());
        assert!(!REPAIR_NEEDED.load(Ordering::Acquire));
        assert!(!protected_environment_ready());
        return;
    }

    // A harmless system DLL under a synthetic basename proves that the test's
    // inherited cwd was searchable before startup changes its loader policy.
    let dll_name = format!("carbonpaper-startup-test-{}.dll", std::process::id());
    let dll_wide = identity::wide(std::ffi::OsStr::new(&dll_name));
    if mode == "repair" {
        std::fs::copy(
            identity::system_directory().unwrap().join("version.dll"),
            inherited.join(&dll_name),
        )
        .unwrap();
        // SAFETY: the name is NUL-terminated and the test-owned file is a copy
        // of a system DLL. Release the returned module before sanitizing.
        unsafe {
            let module = LoadLibraryW(PCWSTR(dll_wide.as_ptr())).unwrap();
            FreeLibrary(module).unwrap();
        }
    }

    let arguments: Vec<String> = match mode.as_str() {
        "repair" | "portable" => vec!["carbonpaper.exe", "--repair-protected-runtime"],
        "cng" => vec!["carbonpaper.exe", "--cng-unlock", "synthetic-key-file"],
        "incomplete_cng" => vec!["carbonpaper.exe", "--cng-unlock"],
        "misplaced_cng" => vec!["carbonpaper.exe", "--hidden", "--cng-unlock"],
        "delegate_error" => vec!["carbonpaper.exe"],
        _ => panic!("unknown startup test mode: {mode}"),
    }
    .into_iter()
    .map(String::from)
    .collect();
    let directory = if mode == "portable" {
        &portable
    } else {
        &runtime
    };
    let result = delegate_startup_in_directory(directory, &root, || {
        if mode == "delegate_error" {
            assert_sanitized(directory);
            return Err("synthetic activation or delegation failure".into());
        }
        try_delegate_startup(&arguments, directory)
    });
    assert_eq!(result, Ok(false));
    assert!(!protected_environment_ready());
    assert_eq!(
        REPAIR_NEEDED.load(Ordering::Acquire),
        matches!(mode.as_str(), "repair" | "portable" | "delegate_error")
    );
    if mode == "portable" {
        for key in OVERRIDES {
            assert_eq!(std::env::var(key).unwrap(), OVERRIDE_VALUE);
        }
        assert_eq!(std::env::var("PATH").unwrap(), OVERRIDE_VALUE);
        assert!(identity::path_eq(
            &std::env::current_dir().unwrap(),
            &inherited
        ));
    } else {
        assert_sanitized(directory);
    }
    if mode == "repair" {
        // Reintroduce an untrusted cwd to check the DLL policy independently
        // of the cwd and PATH cleanup. It must no longer find the synthetic DLL.
        std::env::set_current_dir(&inherited).unwrap();
        // SAFETY: the name remains NUL-terminated. Free a module if a policy
        // regression unexpectedly allows loading the test-owned system DLL.
        unsafe {
            if let Ok(module) = LoadLibraryW(PCWSTR(dll_wide.as_ptr())) {
                FreeLibrary(module).unwrap();
                panic!("protected repair startup still searches the inherited cwd for DLLs");
            }
        }
    }
}

#[test]
fn protected_repair_sanitizes_environment_and_keeps_broker_disabled() {
    run_child("repair");
}

#[test]
fn protected_cng_modes_cannot_skip_environment_sanitization() {
    for mode in ["cng", "incomplete_cng", "misplaced_cng"] {
        run_child(mode);
    }
}

#[test]
fn delegation_failure_keeps_a_sanitized_repair_startup() {
    run_child("delegate_error");
}

#[test]
fn sanitization_failure_cannot_fall_back_to_the_ui() {
    run_child("sanitize_error");
}

#[test]
fn portable_repair_preserves_its_runtime_environment() {
    run_child("portable");
}

#[test]
fn protected_runtime_location_matches_the_service_directory_layout() {
    let root = Path::new(r"C:\Program Files\CarbonPaper\Protected");
    for directory in [
        r"C:\Program Files\CarbonPaper\Protected\Runtime\0.9.1",
        r"c:\program files\carbonpaper\protected\runtime\0.9.1",
        r"\\?\C:\Program Files\CarbonPaper\Protected\Runtime\0.9.1",
    ] {
        assert!(protected_runtime_directory(Path::new(directory), root));
    }
    for directory in [
        r"C:\Program Files\CarbonPaper\Protected\Runtime",
        r"C:\Program Files\CarbonPaper\Protected\Runtime\0.9.1\nested",
        r"C:\Program Files\CarbonPaper\Protected\Runtime-other\0.9.1",
        r"C:\Program Files\CarbonPaper\Portable",
    ] {
        assert!(!protected_runtime_directory(Path::new(directory), root));
    }
}
