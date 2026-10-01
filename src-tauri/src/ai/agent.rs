//! The loop that lets a model search the user's history with contract tools.
//!
//! Each step sends the conversation to the model. When the model asks for
//! tools they run, their results join the conversation, and the next step
//! begins. The loop ends when the model answers without calling a tool, and
//! is bounded by a step limit and a per-request context budget. The last allowed step
//! disables tool calls while retaining their schemas for prompt-cache reuse.
//!
//! The loop reports progress through a callback rather than a window, so a
//! scheduled task can reuse it and record events instead of displaying them.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::config::{ResolvedProvider, ToolCalling};
use super::context::{self, TurnContext};
use super::provider::{
    self, Cancellation, ChatRequest, ChatResponse, Message, ProviderError, StreamEvent, ToolCall,
    ToolSpec, Usage,
};
use super::tools::{self, SnapshotRef, ToolScope};

#[derive(Debug, Clone, Copy)]
pub struct AgentLimits {
    pub max_steps: u32,
    /// Tool calls honoured per step; extra calls are answered with an error.
    pub max_calls_per_step: usize,
    pub max_answer_tokens: u32,
    pub tool_scope: ToolScope,
}

impl Default for AgentLimits {
    fn default() -> Self {
        Self {
            max_steps: 20,
            max_calls_per_step: 10,
            max_answer_tokens: 8_192,
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
    ReasoningDelta {
        text: String,
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
    pub time_context: TurnContext,
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

pub fn system_prompt() -> String {
    "You are the search assistant inside CarbonPaper, an app that keeps a private, \
text-searchable history of the user's screen. Answer the user's question by looking \
through that history with the tools provided.\n\
\n\
Each question includes its own local time, UTC offset and Unix milliseconds in \
turn_time. All tool timestamps are Unix milliseconds. Resolve relative dates such \
as \"yesterday\" from that question's time. Historical searched_time_ranges record \
the absolute bounds actually used by successful searches, including searches with \
no matches; they are not evidence that an event occurred. Reuse those bounds when \
a follow-up refers to the same period instead of shifting them to the new date.\n\
\n\
How to search:\n\
- When tools are available, before the first search output a brief user-facing plan in ordinary text. Then call the tools in the same response. This is commentary, not private reasoning.\n\
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
- Use previous conversation turns to understand follow-up questions. Recheck screenshot details with tools before making new factual claims.\n\
- Earlier conversation or tool results may be omitted to fit the context. Search again for missing evidence; never assume what omitted results contained.\n\
- Base every claim on tool results. When nothing relevant turns up, say so plainly \
and suggest what the user could try instead. Never invent content.\n\
- Cite the screenshots you rely on with their id in the form [#123], placed right \
after the statement they support. Use the screenshot id, not an OCR row id.\n\
- Text shown as [censored] or similar was withheld for privacy. Do not guess at it.\n\
- Be concise. Lead with the answer, then the supporting details.".into()
}

/// Searches run up front for a model that cannot call tools. The question is
/// used as-is, which suits visual search well and literal text search less so.
fn prefetch_calls(question: &str) -> Vec<ToolCall> {
    vec![
        ToolCall {
            id: "prefetch_ocr".into(),
            name: "search_ocr_text".into(),
            arguments: serde_json::json!({ "query": question, "limit": 25, "fuzzy": true })
                .to_string(),
        },
        ToolCall {
            id: "prefetch_nl".into(),
            name: "search_nl".into(),
            arguments: serde_json::json!({ "query": question, "limit": 10 }).to_string(),
        },
    ]
}

fn prefetched_prompt(results: &[(String, String)]) -> String {
    let mut prompt = context::PREFETCH_PREFIX.to_string();
    for (name, content) in results {
        prompt.push_str(&format!(
            "\n<result tool=\"{name}\">\n{content}\n</result>\n"
        ));
    }
    prompt
}

const FORCE_ANSWER_PROMPT: &str =
    "Your tool call budget is exhausted. Stop searching now. Answer the original question \
using only what the tool results above show, cite screenshots as [#id], and say what \
remains unknown.";

#[derive(Debug, Clone, Deserialize)]
pub struct ConversationTurn {
    pub question: String,
    pub answer: String,
    #[serde(default)]
    pub time_context: Option<TurnContext>,
}

fn conversation_messages(history: &[ConversationTurn]) -> Vec<Message> {
    let mut messages = vec![Message::System(system_prompt())];
    // Evict four turns at a time, keeping a stable prefix between evictions.
    // Enforce the bound even if a caller bypasses the frontend.
    let start = history.len().saturating_sub(12).div_ceil(4) * 4;
    for turn in history.iter().skip(start) {
        let question: String = turn.question.chars().take(4_000).collect();
        messages.push(Message::User(match &turn.time_context {
            Some(context) => context.question(&question),
            None => question,
        }));
        let mut answer: String = turn.answer.chars().take(16_000).collect();
        if let Some(context) = &turn.time_context {
            answer.push_str(&context.range_note());
        }
        messages.push(Message::Assistant {
            text: answer,
            tool_calls: Vec::new(),
            reasoning: Value::Null,
        });
    }
    messages
}

pub async fn run(
    app_handle: &tauri::AppHandle,
    provider_config: &ResolvedProvider,
    question: &str,
    history: &[ConversationTurn],
    limits: AgentLimits,
    sink: &mut (dyn FnMut(AgentEvent) + Send),
    cancel: &Cancellation,
) -> Result<AgentOutcome, ProviderError> {
    let client = provider::http_client().map_err(ProviderError::Network)?;
    let specs = tools::specs(limits.tool_scope);
    let mut outcome = AgentOutcome {
        answer: String::new(),
        time_context: TurnContext::new(chrono::Local::now().fixed_offset()),
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
    let mut messages = conversation_messages(history);
    let mut current_question = messages.len();
    messages.push(Message::User(outcome.time_context.question(question)));
    if !tools_supported {
        let mut results = Vec::new();
        for call in prefetch_calls(question) {
            let content = run_tool(app_handle, &call, &specs, sink, cancel, &mut outcome).await?;
            results.push((call.name, content));
        }
        messages.push(Message::User(prefetched_prompt(&results)));
    }

    loop {
        let last_step = !tools_supported || outcome.steps + 1 >= limits.max_steps;
        if last_step && tools_supported {
            messages.push(Message::User(FORCE_ANSWER_PROMPT.into()));
        }
        let mut request = ChatRequest {
            messages: messages.clone(),
            tools: if tools_supported {
                specs.clone()
            } else {
                Vec::new()
            },
            max_tokens: Some(
                limits
                    .max_answer_tokens
                    .min(provider_config.context_tokens / 4),
            ),
            temperature: Some(0.2),
            disable_tools: last_step && tools_supported,
        };
        context::fit_request(
            &mut request,
            &mut current_question,
            provider_config.kind,
            provider_config.context_tokens,
        )?;
        // Persist pruning so the same old material is not reintroduced at the
        // next tool step. The frontend still keeps its complete display history.
        messages = request.messages.clone();

        let response = complete_step(
            &client,
            provider_config,
            request,
            last_step,
            sink,
            cancel,
            &mut outcome,
        )
        .await?;

        if response.tool_calls.is_empty() || last_step {
            outcome.answer = response.text;
            outcome.truncated = response.truncated;
            outcome.stopped_early = tools_supported && last_step && outcome.tool_calls > 0;
            return Ok(outcome);
        }

        messages.push(Message::Assistant {
            reasoning: response.reasoning,
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

/// The final step may need one extra HTTP request for hosts that reject or
/// ignore tool_choice. It never executes more tools. Report each request as its
/// own step so streamed text from a rejected answer cannot mix with its retry.
async fn complete_step(
    client: &reqwest::Client,
    config: &ResolvedProvider,
    mut request: ChatRequest,
    last_step: bool,
    sink: &mut (dyn FnMut(AgentEvent) + Send),
    cancel: &Cancellation,
    outcome: &mut AgentOutcome,
) -> Result<ChatResponse, ProviderError> {
    loop {
        if cancel.is_cancelled() {
            return Err(ProviderError::Cancelled);
        }
        outcome.steps += 1;
        sink(AgentEvent::StepStarted {
            step: outcome.steps,
        });
        let result = {
            let mut forward = |event| match event {
                StreamEvent::ReasoningDelta(text) => sink(AgentEvent::ReasoningDelta { text }),
                StreamEvent::TextDelta(text) => sink(AgentEvent::TextDelta { text }),
            };
            provider::complete(client, config, &request, &mut forward, cancel).await
        };
        if let Ok(response) = &result {
            if let Some(usage) = response.usage {
                outcome.usage.input_tokens += usage.input_tokens;
                outcome.usage.output_tokens += usage.output_tokens;
            }
        }
        if cancel.is_cancelled() {
            return Err(ProviderError::Cancelled);
        }
        if last_step
            && request.disable_tools
            && !request.tools.is_empty()
            && (matches!(&result, Err(ProviderError::BadRequest(_)))
                || matches!(&result, Ok(response) if !response.tool_calls.is_empty()))
        {
            // Clearing both fields makes this fallback available only once.
            // Keep the already fitted, paired tool history and answer prompt.
            request.tools.clear();
            request.disable_tools = false;
            continue;
        }
        let response = result?;
        if last_step && !response.tool_calls.is_empty() {
            return Err(ProviderError::InvalidResponse(
                "Model requested tools when a final answer was required.".into(),
            ));
        }
        if response.tool_calls.is_empty() && response.text.trim().is_empty() {
            return Err(ProviderError::InvalidResponse(
                "Model returned an empty answer.".into(),
            ));
        }
        return Ok(response);
    }
}

#[cfg(test)]
#[path = "agent/request_tests.rs"]
mod request_tests;

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
    if result.error.is_none() {
        outcome.time_context.record_search(call);
    }
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
    #[test]
    fn history_preserves_its_time_and_ranges_without_changing_system_rules() {
        let mut context = TurnContext::new("2026-09-27T23:59:00+08:00".parse().unwrap());
        context.record_search(&ToolCall {
            id: "c".into(),
            name: "search_ocr_text".into(),
            arguments: r#"{"query":"invoice","start_time":1790467200000,"end_time":1790553600000}"#
                .into(),
        });
        let original_question = context.question("today's invoice");
        let messages = conversation_messages(&[ConversationTurn {
            question: "today's invoice".into(),
            answer: "Found [#42]".into(),
            time_context: Some(context),
        }]);
        assert!(
            matches!(&messages[0], Message::System(text) if text == &system_prompt() && !text.contains("2026-09-27"))
        );
        assert!(
            matches!(&messages[1], Message::User(text) if text == &original_question && text.contains("+08:00"))
        );
        assert!(
            matches!(&messages[2], Message::Assistant { text, tool_calls, reasoning }
            if text.starts_with("Found [#42]") && text.contains("1790467200000") && tool_calls.is_empty() && reasoning.is_null())
        );
        let next = TurnContext::new("2026-09-28T00:01:00+08:00".parse().unwrap());
        assert!(next.question("an hour earlier?").contains("2026-09-28"));
        assert!(matches!(&messages[1], Message::User(text) if text.contains("2026-09-27")));
    }

    #[test]
    fn follow_up_context_is_bounded_and_keeps_citations_in_assistant_turns() {
        let history: Vec<_> = (0..15)
            .map(|i| ConversationTurn {
                question: format!("question {i}"),
                answer: "Found [#42]".into(),
                time_context: None,
            })
            .collect();
        let messages = conversation_messages(&history);
        assert_eq!(messages.len(), 23);
        assert!(matches!(&messages[1], Message::User(text) if text == "question 4"));
        assert!(
            matches!(&messages[2], Message::Assistant { text, tool_calls, reasoning } if text == "Found [#42]" && tool_calls.is_empty() && reasoning.is_null())
        );
    }

    #[test]
    fn bounded_history_keeps_its_prefix_between_chunk_evictions() {
        let history: Vec<_> = (0..17)
            .map(|i| ConversationTurn {
                question: format!("question {i}"),
                answer: "answer".into(),
                time_context: None,
            })
            .collect();
        for length in 13..=16 {
            let messages = conversation_messages(&history[..length]);
            assert!(matches!(&messages[1], Message::User(text) if text == "question 4"));
            assert!(messages.len() <= 25);
        }
        assert!(
            matches!(&conversation_messages(&history)[1], Message::User(text) if text == "question 8")
        );
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

        let prompt = prefetched_prompt(&[("search_nl".into(), "[1]".into())]);
        assert!(prompt.starts_with(context::PREFETCH_PREFIX));
        assert!(prompt.contains("<result tool=\"search_nl\">\n[1]\n</result>"));
    }

    #[test]
    fn minimum_context_budget_can_fit_rules_and_the_real_tool_catalog() {
        let budget = super::super::config::MIN_CONTEXT_TOKENS;
        let mut request = ChatRequest {
            messages: vec![
                Message::System(system_prompt()),
                Message::User("Find an invoice".into()),
            ],
            tools: tools::specs(ToolScope::ReadOnly),
            max_tokens: Some(AgentLimits::default().max_answer_tokens.min(budget / 4)),
            ..Default::default()
        };
        context::fit_request(
            &mut request,
            &mut 1,
            super::super::config::ProviderKind::Anthropic,
            budget,
        )
        .unwrap();
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
