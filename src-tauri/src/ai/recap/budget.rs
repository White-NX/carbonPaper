//! Shared generation limits, safe diagnostics and durable retry decisions.
use super::types::{digest, RecapSettings, RecapUsage};
use crate::ai::config::ResolvedProvider;
use serde_json::json;

pub(crate) const HARD_REQUEST_LIMIT: u64 = 32;
pub(crate) const MAX_EVIDENCE_ROUNDS: u32 = 24;

#[derive(Clone, Copy, Debug)]
pub(crate) struct Limit {
    pub code: &'static str,
    pub limit: u64,
    pub used: u64,
}

impl Limit {
    pub(crate) fn available(self) -> u64 {
        self.limit.saturating_sub(self.used)
    }

    pub(crate) fn error(self, requested: u64) -> String {
        format!(
            "{}: limit={}, used={}, requested={}, available={}",
            self.code,
            self.limit,
            self.used,
            requested,
            self.available()
        )
    }

    pub(crate) fn check(self, requested: u64) -> Result<(), String> {
        if requested > self.available() {
            Err(self.error(requested))
        } else {
            Ok(())
        }
    }

    pub(crate) fn tighter(self, other: Self) -> Self {
        if other.available() < self.available() {
            other
        } else {
            self
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Allowance {
    pub input: Limit,
    pub output: Limit,
    pub requests: Limit,
}

pub(crate) fn is_mainline(stage: &str) -> bool {
    matches!(stage, "mainlines" | "mainline_evidence" | "mainline_repair")
}

pub(crate) fn allowance(
    settings: &RecapSettings,
    daily: &RecapUsage,
    run: &RecapUsage,
    request_limit: u64,
    mainline: bool,
) -> Allowance {
    let input = Limit {
        code: "RECAP_DAILY_INPUT_EXHAUSTED",
        limit: settings.daily_input_tokens,
        used: daily.input_tokens,
    }
    .tighter(Limit {
        code: "RECAP_RUN_INPUT_EXHAUSTED",
        limit: settings.batch_input_tokens,
        used: run.input_tokens,
    });
    let output = Limit {
        code: "RECAP_DAILY_OUTPUT_EXHAUSTED",
        limit: settings.daily_output_tokens,
        used: daily.output_tokens,
    }
    .tighter(Limit {
        code: "RECAP_RUN_OUTPUT_EXHAUSTED",
        limit: settings.batch_output_tokens,
        used: run.output_tokens,
    });
    // The final portion of the shared daily allowance is available to mainlines
    // and their repairs. Setting zero makes the entire allowance shared.
    let reserve = u64::from(settings.mainline_reserve_percent.min(50));
    Allowance {
        input: if mainline || reserve == 0 {
            input
        } else {
            input.tighter(Limit {
                code: "RECAP_MAINLINE_HEADROOM",
                limit: settings.daily_input_tokens * (100 - reserve) / 100,
                used: daily.input_tokens,
            })
        },
        output: if mainline || reserve == 0 {
            output
        } else {
            output.tighter(Limit {
                code: "RECAP_MAINLINE_HEADROOM",
                limit: settings.daily_output_tokens * (100 - reserve) / 100,
                used: daily.output_tokens,
            })
        },
        requests: Limit {
            code: "RECAP_REQUEST_LIMIT",
            limit: request_limit.min(HARD_REQUEST_LIMIT),
            used: run.requests,
        },
    }
}

pub(crate) fn fingerprint(settings: &RecapSettings, provider: &ResolvedProvider) -> String {
    digest(
        &json!([
            1,
            settings.batch_input_tokens,
            settings.batch_output_tokens,
            settings.request_output_tokens,
            settings.answer_tokens,
            settings.daily_input_tokens,
            settings.daily_output_tokens,
            settings.screening_input_tokens,
            settings.evidence_rounds,
            settings.mainline_reserve_percent,
            provider.id,
            provider.kind,
            provider.model,
            provider.context_tokens,
            provider.tool_calling,
        ])
        .to_string(),
    )
}

pub(crate) fn code(error: &str) -> &str {
    error.split(':').next().unwrap_or_default()
}

pub(crate) fn numeric_detail(error: &str) -> Option<&str> {
    let detail = error.split_once(':')?.1.trim();
    (detail.len() <= 256
        && !detail.is_empty()
        && detail.split(", ").all(|field| {
            field.split_once('=').is_some_and(|(key, value)| {
                matches!(key, "limit" | "used" | "requested" | "available")
                    && !value.is_empty()
                    && value.bytes().all(|byte| byte.is_ascii_digit())
            })
        }))
    .then_some(detail)
}

pub(crate) fn is_limit(error: &str) -> bool {
    matches!(
        code(error),
        "RECAP_BUDGET_EXHAUSTED"
            | "RECAP_DAILY_INPUT_EXHAUSTED"
            | "RECAP_DAILY_OUTPUT_EXHAUSTED"
            | "RECAP_SCREENING_INPUT_EXHAUSTED"
            | "RECAP_RUN_INPUT_EXHAUSTED"
            | "RECAP_RUN_OUTPUT_EXHAUSTED"
            | "RECAP_REQUEST_LIMIT"
            | "RECAP_MAINLINE_HEADROOM"
            | "AI_CONTEXT_LIMIT"
    )
}

pub(crate) fn retry_due(
    error: Option<&str>,
    saved_hash: &str,
    hash: &str,
    updated_at: i64,
    now: i64,
) -> bool {
    let Some(error) = error.filter(|error| is_limit(error)) else {
        return false;
    };
    if saved_hash != hash {
        return true;
    }
    // A new day restores daily allowance, not the same run's request/context limit.
    matches!(
        code(error),
        "RECAP_BUDGET_EXHAUSTED"
            | "RECAP_DAILY_INPUT_EXHAUSTED"
            | "RECAP_DAILY_OUTPUT_EXHAUSTED"
            | "RECAP_SCREENING_INPUT_EXHAUSTED"
            | "RECAP_MAINLINE_HEADROOM"
    ) && chrono::DateTime::from_timestamp_millis(updated_at)
        .zip(chrono::DateTime::from_timestamp_millis(now))
        .is_some_and(|(a, b)| {
            a.with_timezone(&chrono::Local).date_naive()
                != b.with_timezone(&chrono::Local).date_naive()
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limits_distinguish_daily_run_requests_and_reserved_mainline_allowance() {
        let settings = RecapSettings {
            batch_input_tokens: 1000,
            batch_output_tokens: 1000,
            daily_input_tokens: 2000,
            daily_output_tokens: 2000,
            mainline_reserve_percent: 10,
            ..Default::default()
        };
        let daily = RecapUsage {
            input_tokens: 1800,
            output_tokens: 1800,
            ..Default::default()
        };
        let run = RecapUsage::default();
        let regular = allowance(&settings, &daily, &run, 16, false);
        assert_eq!(regular.input.available(), 0);
        assert_eq!(code(&regular.input.error(100)), "RECAP_MAINLINE_HEADROOM");
        let mainline = allowance(&settings, &daily, &run, 16, true);
        assert!(mainline.input.check(200).is_ok());
        assert_eq!(
            code(&mainline.input.error(201)),
            "RECAP_DAILY_INPUT_EXHAUSTED"
        );
        let run = RecapUsage {
            input_tokens: 1000,
            output_tokens: 1000,
            requests: 16,
            ..Default::default()
        };
        let limits = allowance(&settings, &RecapUsage::default(), &run, 16, true);
        assert_eq!(limits.input.code, "RECAP_RUN_INPUT_EXHAUSTED");
        assert_eq!(limits.output.code, "RECAP_RUN_OUTPUT_EXHAUSTED");
        assert_eq!(limits.requests.code, "RECAP_REQUEST_LIMIT");
        assert!(limits.requests.check(1).is_err());
        let no_reserve = RecapSettings {
            mainline_reserve_percent: 0,
            ..settings
        };
        assert_eq!(
            allowance(&no_reserve, &daily, &RecapUsage::default(), 16, false)
                .input
                .available(),
            200
        );
    }

    #[test]
    fn config_changes_retry_blocked_work_without_expiring_success_or_repeating_runs_each_day() {
        let settings = RecapSettings::default();
        let provider = ResolvedProvider::default();
        let hash = fingerprint(&settings, &provider);
        let changed = fingerprint(
            &RecapSettings {
                daily_input_tokens: settings.daily_input_tokens + 1000,
                ..settings.clone()
            },
            &provider,
        );
        assert!(!retry_due(
            Some("RECAP_DAILY_INPUT_EXHAUSTED: limit=1"),
            &hash,
            &hash,
            0,
            1
        ));
        assert!(retry_due(
            Some("RECAP_DAILY_INPUT_EXHAUSTED"),
            &hash,
            &changed,
            0,
            1
        ));
        assert!(retry_due(
            Some("RECAP_REQUEST_LIMIT"),
            &hash,
            &changed,
            0,
            1
        ));
        assert!(!retry_due(None, &hash, &changed, 0, 1));
        assert!(!retry_due(Some("AI_TIMEOUT"), &hash, &changed, 0, 1));
        assert!(retry_due(
            Some("RECAP_DAILY_OUTPUT_EXHAUSTED"),
            &hash,
            &hash,
            0,
            86_400_000
        ));
        assert!(!retry_due(
            Some("RECAP_REQUEST_LIMIT"),
            &hash,
            &hash,
            0,
            86_400_000
        ));
        assert!(!retry_due(
            Some("AI_CONTEXT_LIMIT"),
            &hash,
            &hash,
            0,
            86_400_000
        ));
        let deeper = RecapSettings {
            evidence_rounds: 24,
            ..settings
        };
        assert_eq!(deeper.request_limit(), 28);
        assert_eq!(deeper.tool_call_limit(), 96);
        assert_ne!(fingerprint(&deeper, &provider), hash);
        assert_eq!(
            numeric_detail("RECAP_REQUEST_LIMIT: limit=16, used=16, requested=1, available=0"),
            Some("limit=16, used=16, requested=1, available=0")
        );
        assert_eq!(
            numeric_detail("RECAP_REQUEST_LIMIT: private screen text"),
            None
        );
    }
}
