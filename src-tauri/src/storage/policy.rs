//! Storage policy save/load operations.

use chrono::{Duration, Utc};
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

/// Resolve `retention_period` to a UTC cutoff datetime string
/// (`%Y-%m-%d %H:%M:%S`, matching the `created_at` column). Snapshots created
/// strictly before this value are considered expired. Returns `None` for
/// `permanent`, empty, or unrecognized values (retention disabled).
fn parse_retention_cutoff(policy: &JsonValue) -> Option<String> {
    let key = match policy.get("retention_period")? {
        JsonValue::String(v) => v.trim().to_ascii_lowercase(),
        _ => return None,
    };

    // Fixed-length day approximations; a retention policy does not need
    // calendar-exact month boundaries.
    let days = match key.as_str() {
        "1month" => 30,
        "6months" => 180,
        "1year" => 365,
        "2years" => 730,
        _ => return None,
    };

    let cutoff = Utc::now() - Duration::days(days);
    Some(cutoff.format("%Y-%m-%d %H:%M:%S").to_string())
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
        let mut cfg_dir = self
            .data_dir
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if let Some(parent) = cfg_dir.parent() {
            cfg_dir = parent.to_path_buf();
        }
        let policy_path = cfg_dir.join("storage_policy.json");

        let s =
            serde_json::to_string_pretty(policy).map_err(|e| format!("serde json error: {}", e))?;
        std::fs::write(&policy_path, s).map_err(|e| format!("failed to write policy file: {}", e))
    }

    /// Load storage policy from storage_policy.json. Returns empty object if file doesn't exist.
    pub fn load_policy(&self) -> Result<JsonValue, String> {
        let mut cfg_dir = self
            .data_dir
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
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
    /// 1) Retention (`retention_period`): prune snapshots older than the
    ///    configured age (1 month / 6 months / 1 year / 2 years).
    /// 2) User snapshot cap (`storage_limit` in GB): prune oldest snapshots beyond cap.
    /// 3) Disk pressure fallback: if free space gets too low, prune to a safe free-space value.
    ///
    /// Returns a summary string when pruning is enqueued, otherwise `None`.
    pub fn enforce_snapshot_storage_policy_once(&self) -> Result<Option<String>, String> {
        let policy = self.load_policy()?;

        let screenshot_dir = self
            .screenshot_dir
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let data_dir = self
            .data_dir
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();

        let mut reasons: Vec<String> = Vec::new();
        let mut total_marked = 0i64;

        // 1) Age-based retention. Snapshots older than the cutoff are pruned
        //    regardless of how much space they occupy.
        let mut retention_freed_bytes = 0u64;
        if let Some(cutoff_dt) = parse_retention_cutoff(&policy) {
            let (candidate_ids, freed_bytes) = self
                .select_screenshots_created_before(&cutoff_dt, MAX_POLICY_DELETE_CANDIDATES_PER_RUN)?;
            if !candidate_ids.is_empty() {
                let result = self.soft_delete_screenshots(&candidate_ids)?;
                if result.screenshots_marked > 0 {
                    total_marked += result.screenshots_marked;
                    retention_freed_bytes = freed_bytes;
                    reasons.push(format!(
                        "retention_expired(cutoff={}, queued={}, freed~={} bytes)",
                        cutoff_dt, result.screenshots_marked, freed_bytes
                    ));
                }
            }
        }

        // 2) + 3) Size-based reclaim (user cap and disk-pressure fallback).
        let current_images_bytes = if screenshot_dir.exists() {
            directory_size(&screenshot_dir)
        } else {
            0
        };
        // Retention deletions above are queued but not yet unlinked, so their
        // bytes still count in `directory_size`. Discount them so the cap pass
        // does not double-count space that is already being reclaimed.
        let effective_images_bytes = current_images_bytes.saturating_sub(retention_freed_bytes);

        let mut required_reclaim_bytes = 0u64;

        if let Some(limit_bytes) = parse_storage_limit_bytes(&policy) {
            if effective_images_bytes > limit_bytes {
                let exceed = effective_images_bytes.saturating_sub(limit_bytes);
                required_reclaim_bytes = required_reclaim_bytes.max(exceed);
                reasons.push(format!(
                    "policy_cap_exceeded(current={}, cap={}, exceed={})",
                    effective_images_bytes, limit_bytes, exceed
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

        if required_reclaim_bytes > 0 {
            let (candidate_ids, estimated_reclaim_bytes) = self
                .select_oldest_screenshots_for_reclaim(
                    required_reclaim_bytes,
                    MAX_POLICY_DELETE_CANDIDATES_PER_RUN,
                )?;
            if !candidate_ids.is_empty() {
                let result = self.soft_delete_screenshots(&candidate_ids)?;
                if result.screenshots_marked > 0 {
                    total_marked += result.screenshots_marked;
                    reasons.push(format!(
                        "cap_reclaim(queued={}, estimated_reclaim={} bytes, target_reclaim={} bytes)",
                        result.screenshots_marked, estimated_reclaim_bytes, required_reclaim_bytes
                    ));
                }
            }
        }

        if total_marked == 0 {
            return Ok(None);
        }

        Ok(Some(format!(
            "queued {} screenshots ({})",
            total_marked,
            reasons.join("; ")
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::parse_retention_cutoff;
    use chrono::{Duration, NaiveDateTime, Utc};
    use serde_json::json;

    const FORMAT: &str = "%Y-%m-%d %H:%M:%S";

    fn cutoff_for(period: &str) -> String {
        parse_retention_cutoff(&json!({ "retention_period": period }))
            .unwrap_or_else(|| panic!("expected a cutoff for {period}"))
    }

    #[test]
    fn permanent_and_unknown_disable_retention() {
        assert!(parse_retention_cutoff(&json!({ "retention_period": "permanent" })).is_none());
        assert!(parse_retention_cutoff(&json!({ "retention_period": "" })).is_none());
        assert!(parse_retention_cutoff(&json!({ "retention_period": "forever" })).is_none());
        // Non-string values and a missing key both mean "no retention".
        assert!(parse_retention_cutoff(&json!({ "retention_period": 30 })).is_none());
        assert!(parse_retention_cutoff(&json!({})).is_none());
    }

    #[test]
    fn known_periods_produce_parseable_cutoffs() {
        for period in ["1month", "6months", "1year", "2years"] {
            let cutoff = cutoff_for(period);
            assert_eq!(cutoff.len(), 19, "unexpected format for {period}: {cutoff}");
            NaiveDateTime::parse_from_str(&cutoff, FORMAT)
                .unwrap_or_else(|e| panic!("cutoff for {period} not in `{FORMAT}`: {e}"));
        }
    }

    #[test]
    fn period_key_is_trimmed_and_case_insensitive() {
        assert!(parse_retention_cutoff(&json!({ "retention_period": "1Month" })).is_some());
        assert!(parse_retention_cutoff(&json!({ "retention_period": " 1YEAR " })).is_some());
    }

    #[test]
    fn longer_retention_yields_earlier_cutoff() {
        // The datetime string sorts lexicographically, so a longer retention
        // window must produce a smaller (earlier) cutoff string.
        let one_month = cutoff_for("1month");
        let six_months = cutoff_for("6months");
        let one_year = cutoff_for("1year");
        let two_years = cutoff_for("2years");
        assert!(two_years < one_year);
        assert!(one_year < six_months);
        assert!(six_months < one_month);
    }

    #[test]
    fn cutoff_matches_expected_day_offset() {
        for (period, days) in [("1month", 30), ("6months", 180), ("1year", 365), ("2years", 730)] {
            let cutoff = NaiveDateTime::parse_from_str(&cutoff_for(period), FORMAT).unwrap();
            let expected = (Utc::now() - Duration::days(days)).naive_utc();
            let skew = (expected - cutoff).num_seconds().abs();
            assert!(skew <= 5, "{period}: cutoff off by {skew}s");
        }
    }
}
