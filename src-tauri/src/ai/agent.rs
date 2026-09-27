//! The loop that lets a model search the user's history with contract tools.
//!
//! Each step sends the conversation to the model. When the model asks for
//! tools they run, their results join the conversation, and the next step
//! begins. The loop ends when the model answers without calling a tool, and
//! is bounded by a step limit and a token budget. The last allowed step
//! offers no tools, which forces an answer from what was found so far.
//!
//! The loop reports progress through a callback rather than a window, so a
//! scheduled task can reuse it and record events instead of displaying them.

use serde::Serialize;
use serde_json::Value;

use super::config::{ResolvedProvider, ToolCalling};
use super::provider::{
    self, Cancellation, ChatRequest, Message, ProviderError, StreamEvent, ToolCall, ToolSpec, Usage,
};
use super::tools::{self, SnapshotRef, ToolScope};

#[derive(Debug, Clone, Copy)]
pub struct AgentLimits {
    pub max_steps: u32,
    /// Tool calls honoured per step; extra calls are answered with an error.
    pub max_calls_per_step: usize,
    /// Once cumulative input tokens pass this, the next step must answer.
    pub input_token_budget: u64,
    pub max_answer_tokens: u32,
    pub tool_scope: ToolScope,
}

impl Default for AgentLimits {
    fn default() -> Self {
        Self {
            max_steps: 8,
            max_calls_per_step: 6,
            input_token_budget: 200_000,
            max_answer_tokens: 4_096,
            tool_scope: ToolScope::ReadOnly,
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentEvent {
    /// A new model call began; text streamed after this belongs to it.
    StepStarted {
        step: u32,
    },
    TextDelta {
        text: String,
    },
    ToolStarted {
        call_id: String,
        name: String,
        arguments: Value,
    },
    ToolFinished {
        call_id: String,
        name: String,
        ok: bool,
        item_count: Option<usize>,
        snapshots: Vec<SnapshotRef>,
    },
}

#[derive(Debug, Clone, Serialize)]
pub struct AgentOutcome {
    pub answer: String,
    /// Every screenshot a tool result mentioned, in first-seen order.
    pub snapshots: Vec<SnapshotRef>,
    pub steps: u32,
    pub tool_calls: u32,
    pub usage: Usage,
    /// The answer was forced by a limit rather than chosen by the model.
    pub stopped_early: bool,
    /// The model hit its output limit mid-answer.
    pub truncated: bool,
}

pub fn system_prompt(now: chrono::DateTime<chrono::Local>) -> String {
    format!(
        "You are the search assistant inside CarbonPaper, an app that keeps a private, \
text-searchable history of the user's screen. Answer the user's question by looking \
through that history with the tools provided.\n\
\n\
Current local time: {local} (UTC offset {offset}); as Unix milliseconds: {millis}. \
All tool timestamps are Unix milliseconds. Convert relative dates such as \"yesterday\" \
or \"last Tuesday\" into millisecond ranges from this time.\n\
\n\
How to search:\n\
- search_ocr_text matches words that were visible on screen. Try the distinctive \
words the user would have seen, and alternative spellings or languages when the first \
attempt finds nothing.\n\
- search_nl finds screenshots by what they look like, such as \"a spreadsheet with \
a bar chart\".\n\
- get_snapshots_by_time_range lists what was on screen during a period, which suits \
questions about when something happened or what the user was doing.\n\
- get_snapshot_details reads the full text of one screenshot. Use it before quoting \
details you have not seen in full.\n\
- Prefer several narrow searches over one broad one, and stop searching once you can \
answer.\n\
\n\
How to answer:\n\
- Reply in the language of the user's question.\n\
- Base every claim on tool results. When nothing relevant turns up, say so plainly \
and suggest what the user could try instead. Never invent content.\n\
- Cite the screenshots you rely on with their id in the form [#123], placed right \
after the statement they support. Use the screenshot id, not an OCR row id.\n\
- Text shown as [censored] or similar was withheld for privacy. Do not guess at it.\n\
- Be concise. Lead with the answer, then the supporting details.",
        local = now.format("%Y-%m-%d %H:%M:%S (%A)"),
        offset = now.format("%:z"),
        millis = now.timestamp_millis(),
    )
}

/// Searches run up front for a model that cannot call tools. The question is
/// used as-is, which suits visual search well and literal text search less so.
fn prefetch_calls(question: &str) -> Vec<ToolCall> {
    vec![
        ToolCall {
            id: "prefetch_ocr".into(),
            name: "search_ocr_text".into(),
            arguments: serde_json::json!({ "query": question, "limit": 15, "fuzzy": true })
                .to_string(),
        },
        ToolCall {
            id: "prefetch_nl".into(),
            name: "search_nl".into(),
            arguments: serde_json::json!({ "query": question, "limit": 10 }).to_string(),
        },
    ]
}

fn prefetched_prompt(question: &str, results: &[(String, String)]) -> String {
    let mut prompt = format!(
        "{question}\n\nYou cannot search yourself this time. These results were found \
for the question above; answer from them alone and cite screenshots as [#id].\n"
    );
    for (name, content) in results {
        prompt.push_str(&format!(
            "\n<result tool=\"{name}\">\n{content}\n</result>\n"
        ));
    }
    prompt
}

const FORCE_ANSWER_PROMPT: &str = "Stop searching now. Answer the original question \
using only what the tool results above show, cite screenshots as [#id], and say what \
remains unknown.";

pub async fn run(
    app_handle: &tauri::AppHandle,
    provider_config: &ResolvedProvider,
    question: &str,
    limits: AgentLimits,
    sink: &mut (dyn FnMut(AgentEvent) + Send),
    cancel: &Cancellation,
) -> Result<AgentOutcome, ProviderError> {
    let client = provider::http_client().map_err(ProviderError::Network)?;
    let specs = tools::specs(limits.tool_scope);
    let mut outcome = AgentOutcome {
        answer: String::new(),
        snapshots: Vec::new(),
        steps: 0,
        tool_calls: 0,
        usage: Usage::default(),
        stopped_early: false,
        truncated: false,
    };

    // A model that cannot call tools gets one round of searches done for it
    // and a single answering step.
    let tools_supported = provider_config.tool_calling != ToolCalling::Unsupported;
    let first_user_message = if tools_supported {
        question.to_string()
    } else {
        let mut results = Vec::new();
        for call in prefetch_calls(question) {
            let content = run_tool(app_handle, &call, &specs, sink, cancel, &mut outcome).await?;
            results.push((call.name, content));
        }
        prefetched_prompt(question, &results)
    };
    let mut messages = vec![
        Message::System(system_prompt(chrono::Local::now())),
        Message::User(first_user_message),
    ];

    loop {
        let over_budget = outcome.usage.input_tokens >= limits.input_token_budget;
        let last_step = !tools_supported || outcome.steps + 1 >= limits.max_steps || over_budget;
        if last_step && tools_supported && outcome.tool_calls > 0 {
            messages.push(Message::User(FORCE_ANSWER_PROMPT.into()));
        }
        let request = ChatRequest {
            messages: messages.clone(),
            tools: if last_step { Vec::new() } else { specs.clone() },
            max_tokens: Some(limits.max_answer_tokens),
            temperature: Some(0.2),
        };

        outcome.steps += 1;
        sink(AgentEvent::StepStarted {
            step: outcome.steps,
        });
        let response = {
            let mut forward = |event: StreamEvent| match event {
                StreamEvent::TextDelta(text) => sink(AgentEvent::TextDelta { text }),
            };
            provider::complete(&client, provider_config, &request, &mut forward, cancel).await?
        };
        if let Some(usage) = response.usage {
            outcome.usage.input_tokens += usage.input_tokens;
            outcome.usage.output_tokens += usage.output_tokens;
        }

        if response.tool_calls.is_empty() || last_step {
            outcome.answer = response.text;
            outcome.truncated = response.truncated;
            outcome.stopped_early = tools_supported && last_step && outcome.tool_calls > 0;
            return Ok(outcome);
        }

        messages.push(Message::Assistant {
            text: response.text,
            tool_calls: response.tool_calls.clone(),
        });
        for (index, call) in response.tool_calls.iter().enumerate() {
            if cancel.is_cancelled() {
                return Err(ProviderError::Cancelled);
            }
            let result = if index < limits.max_calls_per_step {
                run_tool(app_handle, call, &specs, sink, cancel, &mut outcome).await?
            } else {
                // Every call id needs a reply or the next request is rejected.
                serde_json::json!({ "error": "Too many tool calls in one step; this one was skipped." })
                    .to_string()
            };
            messages.push(Message::ToolResult {
                call_id: call.id.clone(),
                content: result,
            });
        }
    }
}

/// Runs one tool call, reporting it to `sink` and recording what it found.
async fn run_tool(
    app_handle: &tauri::AppHandle,
    call: &ToolCall,
    specs: &[ToolSpec],
    sink: &mut (dyn FnMut(AgentEvent) + Send),
    cancel: &Cancellation,
    outcome: &mut AgentOutcome,
) -> Result<String, ProviderError> {
    sink(AgentEvent::ToolStarted {
        call_id: call.id.clone(),
        name: call.name.clone(),
        arguments: serde_json::from_str(&call.arguments).unwrap_or(Value::Null),
    });
    outcome.tool_calls += 1;
    let result = tokio::select! {
        result = tools::execute(app_handle, call, specs) => result,
        _ = cancel.cancelled() => return Err(ProviderError::Cancelled),
    };
    sink(AgentEvent::ToolFinished {
        call_id: call.id.clone(),
        name: call.name.clone(),
        ok: result.error.is_none(),
        item_count: result.item_count,
        snapshots: result.snapshots.clone(),
    });
    tools::merge_snapshots(&mut outcome.snapshots, result.snapshots);
    Ok(result.content)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn system_prompt_states_the_time_in_every_form_tools_need() {
        let now = chrono::Local
            .with_ymd_and_hms(2026, 9, 27, 18, 30, 0)
            .unwrap();
        let prompt = system_prompt(now);
        assert!(prompt.contains("2026-09-27 18:30:00"));
        assert!(prompt.contains(&now.timestamp_millis().to_string()));
        assert!(prompt.contains("[#123]"));
    }

    #[test]
    fn prefetch_uses_read_only_search_tools_and_embeds_their_results() {
        let calls = prefetch_calls("月亮");
        let specs = tools::specs(ToolScope::ReadOnly);
        assert!(calls
            .iter()
            .all(|call| specs.iter().any(|spec| spec.name == call.name)));
        let args: Value = serde_json::from_str(&calls[0].arguments).unwrap();
        assert_eq!(args["query"], "月亮");

        let prompt = prefetched_prompt("q", &[("search_nl".into(), "[1]".into())]);
        assert!(prompt.starts_with("q\n"));
        assert!(prompt.contains("<result tool=\"search_nl\">\n[1]\n</result>"));
    }

    #[test]
    fn events_serialize_with_a_type_tag() {
        let event = AgentEvent::ToolFinished {
            call_id: "c".into(),
            name: "search_ocr_text".into(),
            ok: true,
            item_count: Some(2),
            snapshots: vec![SnapshotRef {
                id: 4,
                ..Default::default()
            }],
        };
        let json = serde_json::to_value(event).unwrap();
        assert_eq!(json["type"], "tool_finished");
        assert_eq!(json["snapshots"][0], serde_json::json!({ "id": 4 }));
    }
}
