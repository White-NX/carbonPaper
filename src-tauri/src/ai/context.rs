//! Stable per-turn time anchors and request-size management.

use chrono::{DateTime, FixedOffset};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::config::ProviderKind;
use super::provider::{self, ChatRequest, Message, ProviderError, ToolCall};

const MAX_SEARCH_RANGES: usize = 48;
pub(super) const PREFETCH_PREFIX: &str = "You cannot search yourself this time. These results were found for the question above; answer from them alone and cite screenshots as [#id].";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SearchTimeRange {
    pub tool: String,
    pub start_time: Option<i64>,
    pub end_time: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnContext {
    pub started_at: DateTime<FixedOffset>,
    #[serde(default)]
    pub searched_ranges: Vec<SearchTimeRange>,
}

fn is_time_search(name: &str) -> bool {
    matches!(
        name,
        "search_ocr_text" | "search_nl" | "get_snapshots_by_time_range"
    )
}

impl TurnContext {
    pub fn new(now: DateTime<FixedOffset>) -> Self {
        Self {
            started_at: now,
            searched_ranges: Vec::new(),
        }
    }

    /// Only call after an executed search succeeded. Do not infer ranges from
    /// the wording of the answer or record skipped/failed calls.
    pub fn record_search(&mut self, call: &ToolCall) {
        if !is_time_search(&call.name) || self.searched_ranges.len() >= MAX_SEARCH_RANGES {
            return;
        }
        let Ok(args) = serde_json::from_str::<Value>(&call.arguments) else {
            return;
        };
        let timestamp = |key: &str| {
            args.get(key)
                .and_then(Value::as_f64)
                .map(|n| crate::mcp_contract::timestamp_seconds(n) * 1000.0)
                .filter(|n| n.is_finite() && *n >= 0.0 && *n < i64::MAX as f64)
                .map(|n| n as i64)
        };
        let range = SearchTimeRange {
            tool: call.name.clone(),
            start_time: timestamp("start_time"),
            end_time: timestamp("end_time"),
        };
        if range.valid() && !self.searched_ranges.contains(&range) {
            self.searched_ranges.push(range);
        }
    }

    pub fn question(&self, question: &str) -> String {
        format!(
            "<turn_time>\nLocal time when this question was asked: {} (UTC offset {}); Unix milliseconds: {}.\n</turn_time>\n\n{}",
            self.started_at.format("%Y-%m-%d %H:%M:%S (%A)"),
            self.started_at.format("%:z"), self.started_at.timestamp_millis(), question,
        )
    }

    pub fn range_note(&self) -> String {
        let ranges: Vec<_> = self
            .searched_ranges
            .iter()
            .filter(|r| r.valid())
            .take(MAX_SEARCH_RANGES)
            .collect();
        if ranges.is_empty() {
            return String::new();
        }
        format!(
            "\n\n<searched_time_ranges unit=\"unix_ms\">\n{}\n</searched_time_ranges>",
            serde_json::to_string(&ranges).expect("time ranges serialize")
        )
    }
}

impl SearchTimeRange {
    fn valid(&self) -> bool {
        is_time_search(&self.tool)
            && (self.start_time.is_some() || self.end_time.is_some())
            && self.start_time.is_none_or(|t| t >= 0)
            && self.end_time.is_none_or(|t| t >= 0)
            && !matches!((self.start_time, self.end_time), (Some(a), Some(b)) if a > b)
    }
}

/// Size the actual provider payload with the bundled DeepSeek vocabulary and
/// a fixed safety multiplier. Context allowance separately leaves 10% headroom.
pub(super) fn estimated_input_tokens(kind: ProviderKind, request: &ChatRequest) -> u64 {
    let body = provider::request_body_for_estimate(kind, request).to_string();
    super::tokenizer::estimate(&body)
}

pub(super) fn input_allowance(request: &ChatRequest, context_tokens: u32) -> u64 {
    (u64::from(context_tokens) * 9 / 10).saturating_sub(u64::from(request.max_tokens.unwrap_or(0)))
}

/// Keep system rules and the current question intact. Remove complete old turns
/// (including their tool exchanges) first, then older current-turn exchanges. Never orphan a
/// tool result or edit the retained provider-native reasoning blocks.
pub(super) fn fit_request(
    request: &mut ChatRequest,
    current_question: &mut usize,
    kind: ProviderKind,
    context_tokens: u32,
) -> Result<(), ProviderError> {
    let allowance = input_allowance(request, context_tokens);
    if estimated_input_tokens(kind, request) <= allowance {
        return Ok(());
    }
    // Once compaction is necessary, make room for several more tool steps.
    // Pruning only just enough would rewrite the distant prefix on every step.
    let target = allowance * 4 / 5;
    while estimated_input_tokens(kind, request) > target {
        if *current_question > 1 {
            // A completed turn ends with an assistant answer without tool calls.
            // Intermediate user messages may contain prefetch results or the
            // forced-answer instruction, so they are not turn boundaries.
            let end = request.messages[1..*current_question]
                .iter()
                .position(|message| {
                    matches!(message, Message::Assistant { tool_calls, .. } if tool_calls.is_empty())
                })
                .map(|index| index + 2)
                .unwrap_or(*current_question);
            request.messages.drain(1..end);
            *current_question -= end - 1;
            continue;
        }
        let exchanges: Vec<_> = request
            .messages
            .iter()
            .enumerate()
            .skip(*current_question + 1)
            .filter_map(|(i, m)| {
                matches!(m, Message::Assistant { tool_calls, .. } if !tool_calls.is_empty())
                    .then_some(i)
            })
            .collect();
        if exchanges.len() > 1 {
            request.messages.drain(exchanges[0]..exchanges[1]);
            continue;
        }
        // A single search can itself be larger than the budget. Keep its call
        // and result IDs, retaining a labelled excerpt that can be re-read.
        let mut reduced = false;
        for (index, message) in request.messages.iter_mut().enumerate() {
            let content = match message {
                Message::ToolResult { content, .. } => content,
                Message::User(text)
                    if index > *current_question && text.starts_with(PREFETCH_PREFIX) =>
                {
                    text
                }
                _ => continue,
            };
            let count = content.chars().count();
            if count > 512 {
                let keep = count / 2;
                let excerpt: String = content.chars().take(keep).collect();
                *content = format!(
                    "{excerpt}\n[Result truncated to fit context; omitted content is unknown.]"
                );
                reduced = true;
                break;
            }
        }
        if !reduced {
            if estimated_input_tokens(kind, request) <= allowance {
                return Ok(());
            }
            return Err(ProviderError::ContextLimit);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::provider::ToolSpec;
    use super::*;
    use serde_json::json;

    fn assistant(text: &str) -> Message {
        Message::Assistant {
            text: text.into(),
            tool_calls: Vec::new(),
            reasoning: Value::Null,
        }
    }

    fn request() -> ChatRequest {
        ChatRequest {
            messages: vec![
                Message::System("stable rules".into()),
                Message::User("current question".into()),
            ],
            max_tokens: Some(4096),
            ..Default::default()
        }
    }

    fn add_exchange(request: &mut ChatRequest, id: &str, result: &str, reasoning: Value) {
        request.messages.push(Message::Assistant {
            text: "Searching".into(),
            reasoning,
            tool_calls: vec![ToolCall {
                id: id.into(),
                name: "search_ocr_text".into(),
                arguments: "{}".into(),
            }],
        });
        request.messages.push(Message::ToolResult {
            call_id: id.into(),
            content: result.into(),
        });
    }

    #[test]
    fn budget_preserves_small_requests_and_prunes_oldest_whole_turns_first() {
        for kind in [ProviderKind::OpenaiCompatible, ProviderKind::Anthropic] {
            let mut req = request();
            req.messages.splice(
                1..1,
                [
                    Message::User("old question".into()),
                    assistant(&"旧内容".repeat(2000)),
                    Message::User("recent question".into()),
                    assistant("recent answer [#42]"),
                ],
            );
            let mut current = 5;
            let before = provider::request_body_for_estimate(kind, &req);
            fit_request(&mut req, &mut current, kind, 128_000).unwrap();
            assert_eq!(before, provider::request_body_for_estimate(kind, &req));
            fit_request(&mut req, &mut current, kind, 8192).unwrap();
            assert_eq!(current, 3);
            assert_eq!(req.messages.len(), 4);
            assert!(matches!(&req.messages[1], Message::User(s) if s == "recent question"));
            assert!(matches!(&req.messages[3], Message::User(s) if s == "current question"));
            assert!(estimated_input_tokens(kind, &req) <= input_allowance(&req, 8192));
        }
    }

    #[test]
    fn historical_tool_turns_are_evicted_whole_including_prefetch_and_forced_answers() {
        for kind in [ProviderKind::OpenaiCompatible, ProviderKind::Anthropic] {
            let mut req = request();
            req.messages[1] = Message::User("old question".into());
            req.messages.push(Message::User(format!(
                "{PREFETCH_PREFIX}\n{}",
                "旧结果".repeat(2000)
            )));
            add_exchange(&mut req, "old", "old result", Value::Null);
            req.messages.push(Message::User("Answer now".into()));
            req.messages.push(assistant("old answer"));
            req.messages.push(Message::User("recent question".into()));
            add_exchange(&mut req, "recent", "evidence [#42]", Value::Null);
            req.messages.push(assistant("recent answer [#42]"));
            let recent = req.messages[7..].to_vec();
            let mut current = req.messages.len();
            req.messages.push(Message::User("current question".into()));
            fit_request(&mut req, &mut current, kind, 8192).unwrap();
            assert_eq!(current, 5);
            assert_eq!(&req.messages[1..current], recent);
            assert!(matches!(&req.messages[current], Message::User(s) if s == "current question"));
            assert!(!provider::request_body_for_estimate(kind, &req)
                .to_string()
                .contains("old"));
        }
    }

    #[test]
    fn tool_exchanges_stay_paired_in_both_protocols_including_the_final_step() {
        for kind in [ProviderKind::OpenaiCompatible, ProviderKind::Anthropic] {
            let mut req = request();
            add_exchange(&mut req, "old", &"历史资料".repeat(2000), Value::Null);
            let reasoning = if kind == ProviderKind::Anthropic {
                json!([{"type":"thinking","thinking":"keep this","signature":"unchanged"}])
            } else {
                json!("keep this")
            };
            add_exchange(&mut req, "new", "evidence [#42]", reasoning.clone());
            req.messages.push(Message::User("Answer now".into()));
            fit_request(&mut req, &mut 1, kind, 8192).unwrap();
            assert_eq!(req.messages.len(), 5);
            assert!(
                matches!(&req.messages[2], Message::Assistant { reasoning: r, tool_calls, .. }
                if r == &reasoning && tool_calls[0].id == "new")
            );
            assert!(
                matches!(&req.messages[3], Message::ToolResult { call_id, content }
                if call_id == "new" && content == "evidence [#42]")
            );
            assert!(matches!(&req.messages[4], Message::User(s) if s == "Answer now"));
            let payload = provider::request_body_for_estimate(kind, &req).to_string();
            assert!(!payload.contains("\"old\""));
            assert!(payload.contains("keep this"));
        }
    }

    #[test]
    fn oversized_latest_results_are_marked_and_never_orphaned() {
        for kind in [ProviderKind::OpenaiCompatible, ProviderKind::Anthropic] {
            let mut req = request();
            add_exchange(&mut req, "c", &"中文证据🙂".repeat(3000), Value::Null);
            fit_request(&mut req, &mut 1, kind, 8192).unwrap();
            assert_eq!(req.messages.len(), 4);
            assert!(
                matches!(&req.messages[3], Message::ToolResult { call_id, content }
                if call_id == "c" && content.contains("truncated") && content.starts_with("中文证据🙂"))
            );
            assert!(estimated_input_tokens(kind, &req) <= input_allowance(&req, 8192));
        }
    }

    #[test]
    fn prefetch_can_shrink_without_cutting_the_current_question() {
        let mut req = request();
        req.messages.push(Message::User(format!(
            "{PREFETCH_PREFIX}\n{}",
            "结果".repeat(10_000)
        )));
        fit_request(&mut req, &mut 1, ProviderKind::OpenaiCompatible, 8192).unwrap();
        assert!(matches!(&req.messages[1], Message::User(s) if s == "current question"));
        assert!(
            matches!(&req.messages[2], Message::User(s) if s.starts_with(PREFETCH_PREFIX) && s.contains("truncated"))
        );
    }

    #[test]
    fn irreducible_question_and_tool_schema_fail_before_a_request_is_sent() {
        let mut req = request();
        req.messages[1] = Message::User("很长的问题".repeat(3000));
        assert_eq!(
            fit_request(&mut req, &mut 1, ProviderKind::Anthropic, 8192),
            Err(ProviderError::ContextLimit)
        );
        let mut req = request();
        req.tools.push(ToolSpec {
            name: "large".into(),
            description: "long".repeat(5000),
            parameters: json!({}),
        });
        assert_eq!(
            fit_request(&mut req, &mut 1, ProviderKind::OpenaiCompatible, 8192),
            Err(ProviderError::ContextLimit)
        );
    }

    #[test]
    fn time_context_records_valid_executed_bounds_and_survives_round_trip() {
        let now: DateTime<FixedOffset> = "2026-09-27T23:59:00+08:00".parse().unwrap();
        let mut context = TurnContext::new(now);
        let call = |name: &str, args: Value| ToolCall {
            id: "c".into(),
            name: name.into(),
            arguments: args.to_string(),
        };
        let valid = call("search_ocr_text", json!({"start_time":1790467200000i64}));
        context.record_search(&valid);
        context.record_search(&valid);
        context.record_search(&call("search_nl", json!({"end_time":1790553600})));
        context.record_search(&call("search_nl", json!({"query":"no time"})));
        context.record_search(&call("search_nl", json!({"start_time":20,"end_time":10})));
        context.record_search(&call("get_snapshot_details", json!({"start_time":10})));
        assert_eq!(context.searched_ranges.len(), 2);
        assert_eq!(context.searched_ranges[1].end_time, Some(1790553600000));
        let restored: TurnContext =
            serde_json::from_value(serde_json::to_value(&context).unwrap()).unwrap();
        assert_eq!(restored.question("q"), context.question("q"));
        assert_eq!(restored.range_note(), context.range_note());
        let question = restored.question("q");
        assert!(
            question.contains("+08:00") && question.contains(&now.timestamp_millis().to_string())
        );
    }
}
