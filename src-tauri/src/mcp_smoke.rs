//! Loopback-only MCP smoke test used by the settings page.

use crate::mcp_contract;
use reqwest::StatusCode;
use serde::Serialize;
use serde_json::{json, Value};
use std::time::{Duration, Instant};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(25);
const STAGE_IDS: &[&str] = &["initialize", "ping", "tools_list", "metadata_query"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SmokeStageStatus {
    Passed,
    Failed,
    Skipped,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct McpSmokeStage {
    pub id: String,
    pub status: SmokeStageStatus,
    pub duration_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct McpSmokeReport {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure_kind: Option<String>,
    pub tool_schema_version: u64,
    pub expected_tool_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub advertised_tool_count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime_generation: Option<u64>,
    pub stages: Vec<McpSmokeStage>,
}

impl McpSmokeReport {
    pub fn preflight_failure(failure_kind: &'static str) -> Self {
        Self {
            ok: false,
            failure_kind: Some(failure_kind.to_string()),
            tool_schema_version: mcp_contract::TOOL_SCHEMA_VERSION,
            expected_tool_count: mcp_contract::TOOL_NAMES.len(),
            advertised_tool_count: None,
            runtime_generation: None,
            stages: STAGE_IDS
                .iter()
                .map(|id| McpSmokeStage {
                    id: (*id).to_string(),
                    status: SmokeStageStatus::Skipped,
                    duration_ms: 0,
                    error_code: None,
                })
                .collect(),
        }
    }

    pub fn with_runtime_generation(mut self, runtime_generation: u64) -> Self {
        self.runtime_generation = Some(runtime_generation);
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SmokeFailure {
    PortUnreachable,
    RequestTimeout,
    AuthFailed,
    HttpError,
    ProtocolError,
    ProtocolMismatch,
    ToolCatalogMismatch,
    SessionLocked,
    PrivacyFilterError,
    MaintenanceInProgress,
    MetadataQueryFailed,
}

impl SmokeFailure {
    fn code(self) -> &'static str {
        match self {
            Self::PortUnreachable => "port_unreachable",
            Self::RequestTimeout => "request_timeout",
            Self::AuthFailed => "auth_failed",
            Self::HttpError => "http_error",
            Self::ProtocolError => "protocol_error",
            Self::ProtocolMismatch => "protocol_mismatch",
            Self::ToolCatalogMismatch => "tool_catalog_mismatch",
            Self::SessionLocked => "session_locked",
            Self::PrivacyFilterError => "privacy_filter_error",
            Self::MaintenanceInProgress => "maintenance_in_progress",
            Self::MetadataQueryFailed => "metadata_query_failed",
        }
    }
}

struct SmokeRunner {
    client: reqwest::Client,
    endpoint: String,
    token: String,
    report: McpSmokeReport,
}

impl SmokeRunner {
    fn new(port: u16, token: &str) -> Result<Self, SmokeFailure> {
        let client = reqwest::Client::builder()
            .no_proxy()
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|_| SmokeFailure::HttpError)?;

        Ok(Self {
            client,
            endpoint: format!("http://127.0.0.1:{port}/mcp"),
            token: token.to_string(),
            report: McpSmokeReport {
                ok: false,
                failure_kind: None,
                tool_schema_version: mcp_contract::TOOL_SCHEMA_VERSION,
                expected_tool_count: mcp_contract::TOOL_NAMES.len(),
                advertised_tool_count: None,
                runtime_generation: None,
                stages: Vec::with_capacity(STAGE_IDS.len()),
            },
        })
    }

    async fn rpc(
        &self,
        id: u64,
        method: &str,
        params: Option<Value>,
    ) -> Result<Value, SmokeFailure> {
        let response = self
            .client
            .post(&self.endpoint)
            .bearer_auth(&self.token)
            .json(&json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": method,
                "params": params
            }))
            .send()
            .await
            .map_err(|error| {
                if error.is_timeout() {
                    SmokeFailure::RequestTimeout
                } else if error.is_connect() {
                    SmokeFailure::PortUnreachable
                } else {
                    SmokeFailure::HttpError
                }
            })?;

        if matches!(
            response.status(),
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN
        ) {
            return Err(SmokeFailure::AuthFailed);
        }
        if !response.status().is_success() {
            return Err(SmokeFailure::HttpError);
        }

        response
            .json::<Value>()
            .await
            .map_err(|_| SmokeFailure::ProtocolError)
    }

    fn push_passed(&mut self, id: &'static str, started: Instant) {
        self.report.stages.push(McpSmokeStage {
            id: id.to_string(),
            status: SmokeStageStatus::Passed,
            duration_ms: elapsed_ms(started),
            error_code: None,
        });
    }

    fn finish_failed(
        mut self,
        id: &'static str,
        started: Instant,
        failure: SmokeFailure,
    ) -> McpSmokeReport {
        self.report.failure_kind = Some(failure.code().to_string());
        self.report.stages.push(McpSmokeStage {
            id: id.to_string(),
            status: SmokeStageStatus::Failed,
            duration_ms: elapsed_ms(started),
            error_code: Some(failure.code().to_string()),
        });
        for remaining in STAGE_IDS.iter().skip(self.report.stages.len()) {
            self.report.stages.push(McpSmokeStage {
                id: (*remaining).to_string(),
                status: SmokeStageStatus::Skipped,
                duration_ms: 0,
                error_code: None,
            });
        }
        self.report
    }

    fn finish_success(mut self) -> McpSmokeReport {
        self.report.ok = true;
        self.report
    }
}

pub async fn run(port: u16, token: &str) -> McpSmokeReport {
    let mut runner = match SmokeRunner::new(port, token) {
        Ok(runner) => runner,
        Err(failure) => return McpSmokeReport::preflight_failure(failure.code()),
    };

    let started = Instant::now();
    let initialize = match runner
        .rpc(
            1,
            "initialize",
            Some(json!({
                "protocolVersion": mcp_contract::MCP_PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": { "name": "CarbonPaper settings smoke test", "version": env!("CARGO_PKG_VERSION") }
            })),
        )
        .await
    {
        Ok(response) => response,
        Err(failure) => return runner.finish_failed("initialize", started, failure),
    };
    let initialize_result = match rpc_result(&initialize) {
        Ok(result) => result,
        Err(failure) => return runner.finish_failed("initialize", started, failure),
    };
    if initialize_result
        .get("protocolVersion")
        .and_then(Value::as_str)
        != Some(mcp_contract::MCP_PROTOCOL_VERSION)
    {
        return runner.finish_failed("initialize", started, SmokeFailure::ProtocolMismatch);
    }
    runner.push_passed("initialize", started);

    let started = Instant::now();
    let ping = match runner.rpc(2, "ping", None).await {
        Ok(response) => response,
        Err(failure) => return runner.finish_failed("ping", started, failure),
    };
    if let Err(failure) = rpc_result(&ping) {
        return runner.finish_failed("ping", started, failure);
    }
    runner.push_passed("ping", started);

    let started = Instant::now();
    let tools_response = match runner.rpc(3, "tools/list", None).await {
        Ok(response) => response,
        Err(failure) => return runner.finish_failed("tools_list", started, failure),
    };
    let tools_result = match rpc_result(&tools_response) {
        Ok(result) => result,
        Err(failure) => return runner.finish_failed("tools_list", started, failure),
    };
    let Some(tools) = tools_result.get("tools").and_then(Value::as_array) else {
        return runner.finish_failed("tools_list", started, SmokeFailure::ProtocolError);
    };
    runner.report.advertised_tool_count = Some(tools.len());
    if Value::Array(tools.clone()) != mcp_contract::tool_definitions() {
        return runner.finish_failed("tools_list", started, SmokeFailure::ToolCatalogMismatch);
    }
    runner.push_passed("tools_list", started);

    let started = Instant::now();
    let metadata_response = match runner
        .rpc(
            4,
            "tools/call",
            Some(json!({
                "name": "get_snapshots_by_time_range",
                "arguments": {
                    "start_time": 0,
                    "end_time": chrono::Utc::now().timestamp_millis(),
                    "max_records": 1
                }
            })),
        )
        .await
    {
        Ok(response) => response,
        Err(failure) => return runner.finish_failed("metadata_query", started, failure),
    };
    let metadata_result = match rpc_result(&metadata_response) {
        Ok(result) => result,
        Err(failure) => {
            let failure = classify_metadata_failure(&metadata_response).unwrap_or(failure);
            return runner.finish_failed("metadata_query", started, failure);
        }
    };
    if !metadata_result_is_array(metadata_result) {
        return runner.finish_failed("metadata_query", started, SmokeFailure::ProtocolError);
    }
    runner.push_passed("metadata_query", started);

    runner.finish_success()
}

fn rpc_result(response: &Value) -> Result<&Value, SmokeFailure> {
    if response.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Err(SmokeFailure::ProtocolError);
    }
    if let Some(error) = response.get("error") {
        let code = error.get("code").and_then(Value::as_i64);
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if code == Some(-32001) || message.contains("AUTH_REQUIRED") {
            return Err(SmokeFailure::SessionLocked);
        }
        if message.contains("MAINTENANCE_IN_PROGRESS") {
            return Err(SmokeFailure::MaintenanceInProgress);
        }
        return Err(SmokeFailure::ProtocolError);
    }
    response.get("result").ok_or(SmokeFailure::ProtocolError)
}

fn classify_metadata_failure(response: &Value) -> Option<SmokeFailure> {
    let message = response
        .get("error")?
        .get("message")?
        .as_str()?
        .to_ascii_lowercase();
    if message.contains("privacy") || message.contains("sensitive") || message.contains("filter") {
        Some(SmokeFailure::PrivacyFilterError)
    } else if message.contains("auth_required") {
        Some(SmokeFailure::SessionLocked)
    } else if message.contains("maintenance_in_progress") {
        Some(SmokeFailure::MaintenanceInProgress)
    } else {
        Some(SmokeFailure::MetadataQueryFailed)
    }
}

fn metadata_result_is_array(result: &Value) -> bool {
    result
        .get("content")
        .and_then(Value::as_array)
        .and_then(|content| content.first())
        .and_then(|entry| entry.get("text"))
        .and_then(Value::as_str)
        .and_then(|text| serde_json::from_str::<Value>(text).ok())
        .is_some_and(|value| value.is_array())
}

fn elapsed_ms(started: Instant) -> u64 {
    started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preflight_failure_has_stable_skipped_stages() {
        let report =
            McpSmokeReport::preflight_failure("server_not_running").with_runtime_generation(17);
        assert!(!report.ok);
        assert_eq!(report.failure_kind.as_deref(), Some("server_not_running"));
        assert_eq!(report.runtime_generation, Some(17));
        assert_eq!(report.stages.len(), STAGE_IDS.len());
        assert!(report
            .stages
            .iter()
            .all(|stage| stage.status == SmokeStageStatus::Skipped));
    }

    #[test]
    fn metadata_failure_is_classified_without_returning_server_text() {
        let response = json!({
            "jsonrpc": "2.0",
            "id": 4,
            "error": { "code": -32000, "message": "Rejected by user's privacy settings" }
        });
        assert_eq!(
            classify_metadata_failure(&response),
            Some(SmokeFailure::PrivacyFilterError)
        );
    }

    #[test]
    fn metadata_result_requires_a_json_array_payload() {
        let good = json!({ "content": [{ "type": "text", "text": "[]" }] });
        let bad = json!({ "content": [{ "type": "text", "text": "{\"id\":1}" }] });
        assert!(metadata_result_is_array(&good));
        assert!(!metadata_result_is_array(&bad));
    }
}
