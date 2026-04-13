//! Storage policy save/load operations.

use serde_json::Value as JsonValue;
use std::path::Path;
use sysinfo::Disks;
use walkdir::WalkDir;

use super::StorageState;

const GIB: u64 = 1024 * 1024 * 1024;
const DISK_PRESSURE_TRIGGER_FREE_BYTES: u64 = 2 * GIB;
const DISK_PRESSURE_SAFE_FREE_BYTES: u64 = 5 * GIB;
const MAX_POLICY_DELETE_CANDIDATES_PER_RUN: i64 = 2_000;

fn directory_size(path: &Path) -> u64 {
    WalkDir::new(path)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file())
        .filter_map(|entry| entry.metadata().ok().map(|meta| meta.len()))
        .sum()
}

fn parse_storage_limit_bytes(policy: &JsonValue) -> Option<u64> {
    let raw = policy.get("storage_limit")?;

    let gb = match raw {
        JsonValue::Number(v) => v.as_u64(),
        JsonValue::String(v) => {
            let text = v.trim().to_ascii_lowercase();
            if text.is_empty() || text == "unlimited" {
                None
            } else {
                text.parse::<u64>().ok()
            }
        }
        _ => None,
    }?;

    if gb == 0 {
        return None;
    }

    Some(gb.saturating_mul(GIB))
}

fn disk_totals_for_path(path: &Path) -> Option<(u64, u64)> {
    let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let disks = Disks::new_with_refreshed_list();

    let mut matched: Option<(usize, u64, u64)> = None;
    for disk in disks.list() {
        let mount = disk.mount_point();
        if !canonical.starts_with(mount) {
            continue;
        }

        let mount_len = mount.to_string_lossy().len();
        let total = disk.total_space();
        let available = disk.available_space();

        let replace = matched
            .as_ref()
            .map(|(best_len, _, _)| mount_len > *best_len)
            .unwrap_or(true);

        if replace {
            matched = Some((mount_len, total, available));
        }
    }

    matched.map(|(_, total, available)| (total, available))
}

impl StorageState {
    /// Save storage policy to storage_policy.json in the app config directory.
    pub fn save_policy(&self, policy: &JsonValue) -> Result<(), String> {
        let mut cfg_dir = self.data_dir.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if let Some(parent) = cfg_dir.parent() {
            cfg_dir = parent.to_path_buf();
        }
        let policy_path = cfg_dir.join("storage_policy.json");

        let s = serde_json::to_string_pretty(policy)
            .map_err(|e| format!("serde json error: {}", e))?;
        std::fs::write(&policy_path, s)
            .map_err(|e| format!("failed to write policy file: {}", e))
    }

    /// Load storage policy from storage_policy.json. Returns empty object if file doesn't exist.
    pub fn load_policy(&self) -> Result<JsonValue, String> {
        let mut cfg_dir = self.data_dir.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if let Some(parent) = cfg_dir.parent() {
            cfg_dir = parent.to_path_buf();
        }
        let policy_path = cfg_dir.join("storage_policy.json");

        if !policy_path.exists() {
            return Ok(serde_json::json!({}));
        }

        let content = std::fs::read_to_string(&policy_path)
            .map_err(|e| format!("failed to read policy file: {}", e))?;
        let v: JsonValue = serde_json::from_str(&content)
            .map_err(|e| format!("failed to parse policy json: {}", e))?;
        Ok(v)
    }

    /// Enforce snapshot storage policy once.
    ///
    /// Policy includes:
    /// 1) User snapshot cap (`storage_limit` in GB): prune oldest snapshots beyond cap.
    /// 2) Disk pressure fallback: if free space gets too low, prune to a safe free-space value.
    ///
    /// Returns a summary string when pruning is enqueued, otherwise `None`.
    pub fn enforce_snapshot_storage_policy_once(&self) -> Result<Option<String>, String> {
        let policy = self.load_policy()?;

        let screenshot_dir = self
            .screenshot_dir
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let data_dir = self.data_dir.lock().unwrap_or_else(|e| e.into_inner()).clone();

        let current_images_bytes = if screenshot_dir.exists() {
            directory_size(&screenshot_dir)
        } else {
            0
        };

        let mut required_reclaim_bytes = 0u64;
        let mut reasons: Vec<String> = Vec::new();

        if let Some(limit_bytes) = parse_storage_limit_bytes(&policy) {
            if current_images_bytes > limit_bytes {
                let exceed = current_images_bytes.saturating_sub(limit_bytes);
                required_reclaim_bytes = required_reclaim_bytes.max(exceed);
                reasons.push(format!(
                    "policy_cap_exceeded(current={}, cap={}, exceed={})",
                    current_images_bytes, limit_bytes, exceed
                ));
            }
        }

        if let Some((total_bytes, available_bytes)) = disk_totals_for_path(&data_dir) {
            if total_bytes > 0 {
                // Trigger when free space <= max(2 GiB, 1% disk).
                let trigger_threshold = DISK_PRESSURE_TRIGGER_FREE_BYTES.max(total_bytes / 100);
                if available_bytes <= trigger_threshold {
                    // Safe target is max(5 GiB, 3% disk).
                    let safe_threshold =
                        DISK_PRESSURE_SAFE_FREE_BYTES.max(total_bytes.saturating_mul(3) / 100);
                    let disk_reclaim = safe_threshold.saturating_sub(available_bytes);
                    if disk_reclaim > 0 {
                        required_reclaim_bytes = required_reclaim_bytes.max(disk_reclaim);
                        reasons.push(format!(
                            "disk_pressure(total={}, free={}, trigger={}, safe={}, reclaim={})",
                            total_bytes,
                            available_bytes,
                            trigger_threshold,
                            safe_threshold,
                            disk_reclaim
                        ));
                    }
                }
            }
        }

        if required_reclaim_bytes == 0 {
            return Ok(None);
        }

        let (candidate_ids, estimated_reclaim_bytes) = self.select_oldest_screenshots_for_reclaim(
            required_reclaim_bytes,
            MAX_POLICY_DELETE_CANDIDATES_PER_RUN,
        )?;

        if candidate_ids.is_empty() {
            return Ok(None);
        }

        let result = self.soft_delete_screenshots(&candidate_ids)?;
        if result.screenshots_marked <= 0 {
            return Ok(None);
        }

        Ok(Some(format!(
            "queued {} screenshots (estimated_reclaim={} bytes, target_reclaim={} bytes, reasons={})",
            result.screenshots_marked,
            estimated_reclaim_bytes,
            required_reclaim_bytes,
            reasons.join("; ")
        )))
    }
}
