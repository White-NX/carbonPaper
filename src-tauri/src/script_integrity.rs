//! Monitor script integrity verification.
//!
//! ## Threat model — what this actually mitigates
//!
//! This is an *integrity* check, **not** anti-tamper protection against a
//! privileged attacker. The expected SHA-256 of `monitor.pyz` is injected at
//! compile time by `build.rs` via `cargo:rustc-env=MONITOR_PYZ_SHA256=...` and
//! ends up as a `&'static str` constant in the Rust binary's `.rodata`. That
//! raises the bar — an attacker now has to coherently patch **both** the
//! `.exe` (rewriting the constant) and the `.pyz`, which breaks any
//! Authenticode signature on the NSIS installer — but anyone with write
//! access to the `.pyz` typically has comparable access to the `.exe`, so
//! this is not proof against a determined adversary. For real anti-tamper,
//! sign the installer and rely on Windows code-signing / SmartScreen.
//!
//! What the check *does* reliably catch:
//!   - accidental corruption (disk error, half-written update)
//!   - drive-by replacement of only the `.pyz` (e.g. malware that drops a
//!     malicious monitor script next to an untouched executable)
//!   - shipping a release binary against a stale or wrong-version `.pyz`
//!
//! ## Operational pieces
//!
//! - `verify_monitor_pyz()` reads the disk `.pyz`, computes SHA-256, and
//!   compares with the expected value; any mismatch / read failure returns
//!   `Err`.
//! - `log_security_event()` appends the event to
//!   `<LocalAppData>/CarbonPaper/security.log` when verification fails,
//!   independent of the tracing logging system, for forensic purposes.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;

use chrono::Utc;
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Emitter};

use crate::resource_utils::file_in_local_appdata;

/// Compile-time injected expected monitor.pyz hash value (lowercase hexadecimal).
pub const EXPECTED_PYZ_SHA256: &str = env!("MONITOR_PYZ_SHA256");

/// Compute SHA-256 of a byte slice, return lowercase hexadecimal string.
fn compute_sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    digest.iter().map(|b| format!("{:02x}", b)).collect()
}

/// Verify if the given hash matches the expected value. Extracted mainly for unit test convenience
/// (`EXPECTED_PYZ_SHA256` at test time depends on the value injected by build.rs, cannot directly assert a known value).
fn verify_hash(actual: &str, expected: &str) -> Result<(), String> {
    if actual.eq_ignore_ascii_case(expected) {
        Ok(())
    } else {
        Err(format!(
            "SHA-256 mismatch: expected={}, got={}",
            expected, actual
        ))
    }
}

/// Verify if the monitor.pyz on disk matches the expected hash embedded at compile time.
///
/// On success returns `Ok(())`; any failure (cannot read, hash mismatch) returns `Err(reason)`.
pub fn verify_monitor_pyz(pyz_path: &Path) -> Result<(), String> {
    let bytes = fs::read(pyz_path)
        .map_err(|e| format!("Cannot read monitor.pyz at {}: {}", pyz_path.display(), e))?;
    let actual = compute_sha256_hex(&bytes);
    verify_hash(&actual, EXPECTED_PYZ_SHA256)
}

/// Append a security event to `<LocalAppData>/CarbonPaper/security.log`.
///
/// Even if writing fails, only log a warning through tracing, do not propagate errors to the caller:
/// Verification failure itself will be communicated to the user through other channels (emit event + Result).
pub fn log_security_event(_app: &AppHandle, event: &str, detail: &str) {
    let Some(base_dir) = file_in_local_appdata() else {
        tracing::warn!(
            "log_security_event: LOCALAPPDATA unavailable, dropping event={} detail={}",
            event,
            detail
        );
        return;
    };

    if let Err(e) = fs::create_dir_all(&base_dir) {
        tracing::warn!(
            "log_security_event: failed to create {}: {}",
            base_dir.display(),
            e
        );
        return;
    }

    let log_path = base_dir.join("security.log");
    let timestamp = Utc::now().to_rfc3339();
    // 过滤掉换行避免行格式被打乱
    let safe_event = event.replace(['\n', '\r'], " ");
    let safe_detail = detail.replace(['\n', '\r'], " ");
    let line = format!(
        "[{}] event={} detail={}\n",
        timestamp, safe_event, safe_detail
    );

    match OpenOptions::new().create(true).append(true).open(&log_path) {
        Ok(mut f) => {
            if let Err(e) = f.write_all(line.as_bytes()) {
                tracing::warn!(
                    "log_security_event: write {} failed: {}",
                    log_path.display(),
                    e
                );
            }
        }
        Err(e) => {
            tracing::warn!(
                "log_security_event: open {} failed: {}",
                log_path.display(),
                e
            );
        }
    }
}

/// Debug Tauri command: allows frontend buttons to manually trigger a `security-alert` event once,
/// to verify the Rust→frontend alert path.
/// Also writes a `event=debug_trigger` to security.log to verify the log path is correct.
/// This command itself has no side effects, the security alert payload is simulated.
#[tauri::command]
pub fn debug_trigger_security_alert(app: AppHandle) -> Result<(), String> {
    log_security_event(&app, "debug_trigger", "manually triggered from settings");
    app.emit(
        "security-alert",
        serde_json::json!({
            "code": "DEBUG_MANUAL_TRIGGER",
            "message": "[Debug] Security alert triggered manually from settings.",
            "detail": "This is a test alert. No real integrity issue was detected.",
        }),
    )
    .map_err(|e| format!("Failed to emit security-alert: {}", e))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Known SHA-256 test vectors
    const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
    const HELLO_SHA256: &str = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";

    #[test]
    fn test_compute_sha256_empty() {
        assert_eq!(compute_sha256_hex(b""), EMPTY_SHA256);
    }

    #[test]
    fn test_compute_sha256_hello() {
        assert_eq!(compute_sha256_hex(b"hello"), HELLO_SHA256);
    }

    #[test]
    fn test_verify_hash_matches() {
        assert!(verify_hash(EMPTY_SHA256, EMPTY_SHA256).is_ok());
    }

    #[test]
    fn test_verify_hash_case_insensitive() {
        let upper = EMPTY_SHA256.to_uppercase();
        assert!(verify_hash(EMPTY_SHA256, &upper).is_ok());
    }

    #[test]
    fn test_verify_hash_mismatch() {
        let result = verify_hash(EMPTY_SHA256, HELLO_SHA256);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.contains("mismatch"));
        assert!(err.contains(EMPTY_SHA256));
        assert!(err.contains(HELLO_SHA256));
    }

    #[test]
    fn test_verify_monitor_pyz_missing_file() {
        let result = verify_monitor_pyz(Path::new("nonexistent_test_pyz_4f8a3c1d.pyz"));
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Cannot read"));
    }

    #[test]
    fn test_verify_monitor_pyz_tampered() {
        // Create a temporary file with content that clearly won't match the expected SHA-256
        let tmp = tempfile::NamedTempFile::new().expect("create tempfile");
        std::fs::write(tmp.path(), b"this is definitely not a real monitor.pyz")
            .expect("write tempfile");
        let result = verify_monitor_pyz(tmp.path());
        // EXPECTED_PYZ_SHA256 comes from the real .pyz hash injected by build.rs, so it will definitely not match the temporary file content
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("mismatch"));
    }
}
