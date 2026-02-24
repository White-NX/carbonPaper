//! Storage policy save/load operations.

use serde_json::Value as JsonValue;

use super::StorageState;

impl StorageState {
    /// Save storage policy to storage_policy.json in the app config directory.
    pub fn save_policy(&self, policy: &JsonValue) -> Result<(), String> {
        let mut cfg_dir = self.data_dir.lock().unwrap().clone();
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
        let mut cfg_dir = self.data_dir.lock().unwrap().clone();
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
}
