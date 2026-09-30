use std::time::{Duration, Instant};

use super::types::*;
use crate::ai::provider::{ChatResponse, StreamEvent};

const PREVIEW_CHARS: usize = 16_000;
const NOTIFY_INTERVAL: Duration = Duration::from_millis(250);

#[derive(Default)]
pub(super) struct ProgressState {
    pub snapshot: Option<RecapProgress>,
    pub generation: u64,
    pub revision: Option<i64>,
    pub privacy: String,
    last_notify: Option<Instant>,
}

impl ProgressState {
    pub fn start(&mut self, date: String, generation: u64) {
        *self = Self {
            generation,
            snapshot: Some(RecapProgress {
                run_id: hex::encode(rand::random::<[u8; 16]>()),
                date,
                started_at_ms: chrono::Utc::now().timestamp_millis(),
                stage: "preparing".into(),
                ..Default::default()
            }),
            ..Default::default()
        };
    }

    pub fn update(&mut self, force: bool, update: impl FnOnce(&mut RecapProgress)) -> bool {
        let Some(snapshot) = self.snapshot.as_mut() else {
            return false;
        };
        update(snapshot);
        snapshot.version += 1;
        snapshot.updated_at_ms = chrono::Utc::now().timestamp_millis();
        if force
            || self
                .last_notify
                .is_none_or(|t| t.elapsed() >= NOTIFY_INTERVAL)
        {
            self.last_notify = Some(Instant::now());
            true
        } else {
            false
        }
    }
}

impl RecapProgress {
    pub fn stream(&mut self, event: StreamEvent) {
        let Some(attempt) = self.attempts.last_mut() else {
            return;
        };
        if attempt.finished_at_ms.is_some() {
            return;
        }
        match event {
            StreamEvent::ReasoningDelta(text) => {
                append_preview(&mut attempt.reasoning, &mut attempt.reasoning_chars, &text);
                if attempt.text_chars == 0 {
                    self.stage = "thinking".into();
                }
            }
            StreamEvent::TextDelta(text) => {
                append_preview(&mut attempt.text, &mut attempt.text_chars, &text);
                self.stage = "writing".into();
            }
        }
    }

    pub fn finish_attempt(&mut self, result: &Result<ChatResponse, String>) {
        let Some(attempt) = self.attempts.last_mut() else {
            return;
        };
        attempt.finished_at_ms = Some(chrono::Utc::now().timestamp_millis());
        match result {
            Ok(response) => {
                attempt.truncated = response.truncated;
                attempt.input_tokens = response.usage.map(|u| u.input_tokens);
                attempt.output_tokens = response.usage.map(|u| u.output_tokens);
                attempt.reasoning_tokens = response.reasoning_tokens;
                attempt.status = if response.truncated {
                    "failed"
                } else {
                    "validating"
                }
                .into();
                attempt.error = response.truncated.then(|| "RECAP_OUTPUT_TRUNCATED".into());
                self.stage = "validating".into();
            }
            Err(error) => {
                attempt.status = "failed".into();
                attempt.error = Some(error.clone());
            }
        }
    }

    pub fn validation(&mut self, error: Option<&str>) {
        if let Some(attempt) = self.attempts.last_mut() {
            attempt.status = if error.is_some() { "failed" } else { "ready" }.into();
            attempt.error = error.map(str::to_string);
        }
    }
}

fn append_preview(preview: &mut String, total: &mut usize, text: &str) {
    let available = PREVIEW_CHARS.saturating_sub(*total);
    preview.extend(text.chars().take(available));
    *total = total.saturating_add(text.chars().count());
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn deepseek_style_reasoning_stream_keeps_usage_and_preview_when_truncated() {
        use crate::ai::{
            config::{ProviderKind, ResolvedProvider, ToolCalling},
            provider::{self, Cancellation, ChatRequest},
        };
        use axum::{routing::post, Json, Router};
        use serde_json::{json, Value};
        let body = format!(
            "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
            json!({"choices":[{"delta":{"reasoning_content":"思".repeat(20_000)}}]}),
            json!({"choices":[{"delta":{},"finish_reason":"length"}],"usage":{
                "prompt_tokens":500,"completion_tokens":32000,
                "completion_tokens_details":{"reasoning_tokens":32000}
            }})
        );
        let server = Router::new().route(
            "/v1/chat/completions",
            post(move |Json(request): Json<Value>| {
                let body = body.clone();
                async move {
                    assert_eq!(request["max_tokens"], 32000);
                    ([("content-type", "text/event-stream")], body)
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, server).await.unwrap();
        });
        let model = ResolvedProvider {
            kind: ProviderKind::OpenaiCompatible,
            model: "test-model".into(),
            base_url: format!("http://{address}/v1"),
            api_key: None,
            tool_calling: ToolCalling::Unknown,
            context_tokens: 128000,
        };
        let mut progress = RecapProgress {
            attempts: vec![RecapAttempt::default()],
            ..Default::default()
        };
        let response = provider::complete(
            &provider::http_client().unwrap(),
            &model,
            &ChatRequest {
                max_tokens: Some(32000),
                ..Default::default()
            },
            &mut |event| progress.stream(event),
            &Cancellation::default(),
        )
        .await
        .unwrap();
        assert_eq!(progress.stage, "thinking");
        progress.finish_attempt(&Ok(response));
        server.abort();
        let attempt = &progress.attempts[0];
        assert_eq!(attempt.reasoning_chars, 20000);
        assert_eq!(attempt.reasoning.chars().count(), PREVIEW_CHARS);
        assert_eq!(attempt.reasoning_tokens, Some(32000));
        assert_eq!(attempt.output_tokens, Some(32000)); // Thinking is already included.
        assert_eq!(attempt.text_chars, 0);
        assert_eq!(attempt.error.as_deref(), Some("RECAP_OUTPUT_TRUNCATED"));
    }

    #[test]
    fn thinking_and_answer_are_separate_bounded_and_preserved_on_failure() {
        let mut progress = RecapProgress {
            attempts: vec![RecapAttempt::default()],
            ..Default::default()
        };
        progress.stream(StreamEvent::ReasoningDelta("思".repeat(20_000)));
        assert_eq!(progress.stage, "thinking");
        assert_eq!(
            progress.attempts[0].reasoning.chars().count(),
            PREVIEW_CHARS
        );
        assert_eq!(progress.attempts[0].reasoning_chars, 20_000);
        progress.stream(StreamEvent::TextDelta("{\"activities\":".into()));
        assert_eq!(progress.stage, "writing");
        progress.finish_attempt(&Ok(ChatResponse {
            truncated: true,
            ..Default::default()
        }));
        assert_eq!(
            progress.attempts[0].error.as_deref(),
            Some("RECAP_OUTPUT_TRUNCATED")
        );
        assert!(!progress.attempts[0].text.is_empty());
        progress.stream(StreamEvent::TextDelta("late event".into()));
        assert!(!progress.attempts[0].text.contains("late event"));
    }
    #[test]
    fn snapshot_updates_survive_notification_throttling_and_a_new_run_resets_them() {
        let mut state = ProgressState::default();
        state.start("2026-09-30".into(), 1);
        assert!(state.update(true, |p| p.total_batches = 5));
        assert!(!state.update(false, |p| p.completed_batches = 1));
        assert_eq!(state.snapshot.as_ref().unwrap().completed_batches, 1);
        assert!(state.update(true, |p| p.stage = "ready".into()));
        state.start("2026-10-01".into(), 2);
        assert_eq!(state.snapshot.as_ref().unwrap().completed_batches, 0);
        assert_eq!(state.generation, 2);
    }
}
