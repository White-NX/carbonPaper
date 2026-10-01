use super::*;
use crate::ai::config::ProviderKind;
use axum::{http::StatusCode, routing::post, Json, Router};
use serde_json::json;
use std::sync::{Arc, Mutex};

async fn server(
    kind: ProviderKind,
    replies: Vec<(u16, String)>,
) -> (ResolvedProvider, Arc<Mutex<Vec<Value>>>) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let recorder = seen.clone();
    let route = if kind == ProviderKind::Anthropic {
        "/v1/messages"
    } else {
        "/v1/chat/completions"
    };
    let app = Router::new().route(
        route,
        post(move |Json(body): Json<Value>| {
            let mut seen = recorder.lock().unwrap();
            let reply = replies
                .get(seen.len())
                .cloned()
                .unwrap_or((500, "unexpected retry".into()));
            seen.push(body);
            async move {
                (
                    StatusCode::from_u16(reply.0).unwrap(),
                    [("content-type", "text/event-stream")],
                    reply.1,
                )
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (
        ResolvedProvider {
            kind,
            base_url: format!("http://{addr}/v1"),
            model: "test".into(),
            api_key: None,
            tool_calling: ToolCalling::Supported,
            context_tokens: 32_000,
        },
        seen,
    )
}

fn reply(kind: ProviderKind, text: &str, tool: bool) -> (u16, String) {
    let events = if kind == ProviderKind::OpenaiCompatible {
        let calls = if tool {
            json!([{"index":0,"id":"unexpected","type":"function",
            "function":{"name":"search","arguments":"{}"}}])
        } else {
            json!([])
        };
        vec![
            json!({"choices":[{"delta":{"content":text,"tool_calls":calls}}],
            "usage":{"prompt_tokens":3,"completion_tokens":2}}),
        ]
    } else {
        let mut events = vec![
            json!({"type":"message_start","message":{"usage":{"input_tokens":3}}}),
            json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":text}}),
        ];
        if tool {
            events.push(json!({"type":"content_block_start","index":1,
                "content_block":{"type":"tool_use","id":"unexpected","name":"search"}}));
        }
        events.push(json!({"type":"message_delta","usage":{"output_tokens":2}}));
        events.push(json!({"type":"message_stop"}));
        events
    };
    (
        200,
        events
            .into_iter()
            .map(|e| format!("data: {e}\n\n"))
            .collect(),
    )
}

fn request() -> ChatRequest {
    ChatRequest {
        messages: vec![
            Message::System("rules".into()),
            Message::User("question".into()),
            Message::Assistant {
                text: String::new(),
                reasoning: Value::Null,
                tool_calls: vec![ToolCall {
                    id: "old".into(),
                    name: "search".into(),
                    arguments: "{}".into(),
                }],
            },
            Message::ToolResult {
                call_id: "old".into(),
                content: "found evidence".into(),
            },
            Message::User(FORCE_ANSWER_PROMPT.into()),
        ],
        tools: vec![ToolSpec {
            name: "search".into(),
            description: "Search".into(),
            parameters: json!({"type":"object"}),
        }],
        disable_tools: true,
        ..Default::default()
    }
}

fn outcome() -> AgentOutcome {
    AgentOutcome {
        answer: String::new(),
        time_context: TurnContext::new(chrono::Local::now().fixed_offset()),
        snapshots: vec![],
        steps: 0,
        tool_calls: 1,
        usage: Usage::default(),
        stopped_early: false,
        truncated: false,
    }
}

#[tokio::test]
async fn final_bad_request_retries_without_tools_and_keeps_paired_history() {
    for kind in [ProviderKind::OpenaiCompatible, ProviderKind::Anthropic] {
        let (config, seen) = server(
            kind,
            vec![
                (400, "unsupported tool_choice".into()),
                reply(kind, "Answer", false),
            ],
        )
        .await;
        let mut outcome = outcome();
        let response = complete_step(
            &provider::http_client().unwrap(),
            &config,
            request(),
            true,
            &mut |_| {},
            &Cancellation::default(),
            &mut outcome,
        )
        .await
        .unwrap();
        assert_eq!(response.text, "Answer");
        assert_eq!(outcome.steps, 2);
        assert_eq!(outcome.tool_calls, 1);
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 2);
        assert!(seen[0].get("tools").is_some());
        assert!(seen[0].get("tool_choice").is_some());
        assert!(seen[1].get("tools").is_none());
        assert!(seen[1].get("tool_choice").is_none());
        assert_eq!(seen[0]["messages"], seen[1]["messages"]);
        assert_eq!(config.tool_calling, ToolCalling::Supported);
    }
}

#[tokio::test]
async fn ignored_tool_choice_retries_in_a_new_streaming_step_and_counts_both_requests() {
    for kind in [ProviderKind::OpenaiCompatible, ProviderKind::Anthropic] {
        let (config, seen) = server(
            kind,
            vec![
                reply(kind, "Searching again", true),
                reply(kind, "Final answer", false),
            ],
        )
        .await;
        let mut outcome = outcome();
        let mut steps = Vec::<String>::new();
        let response = complete_step(
            &provider::http_client().unwrap(),
            &config,
            request(),
            true,
            &mut |event| match event {
                AgentEvent::StepStarted { .. } => steps.push(String::new()),
                AgentEvent::TextDelta { text } => steps.last_mut().unwrap().push_str(&text),
                _ => {}
            },
            &Cancellation::default(),
            &mut outcome,
        )
        .await
        .unwrap();
        assert_eq!(response.text, "Final answer");
        assert_eq!(steps, ["Searching again", "Final answer"]);
        assert_eq!(
            outcome.usage,
            Usage {
                input_tokens: 6,
                output_tokens: 4
            }
        );
        assert_eq!(outcome.tool_calls, 1);
        assert_eq!(seen.lock().unwrap().len(), 2);
    }
}

#[tokio::test]
async fn fallback_is_bounded_and_never_accepts_tools_or_an_empty_answer() {
    for kind in [ProviderKind::OpenaiCompatible, ProviderKind::Anthropic] {
        for (text, tool) in [("", true), (" ", false), ("plan", true)] {
            let (config, seen) =
                server(kind, vec![reply(kind, "", true), reply(kind, text, tool)]).await;
            let error = complete_step(
                &provider::http_client().unwrap(),
                &config,
                request(),
                true,
                &mut |_| {},
                &Cancellation::default(),
                &mut outcome(),
            )
            .await
            .unwrap_err();
            assert!(matches!(error, ProviderError::InvalidResponse(_)));
            assert_eq!(seen.lock().unwrap().len(), 2);
        }
        let (config, seen) = server(
            kind,
            vec![(400, "bad tools".into()), (400, "still bad".into())],
        )
        .await;
        let error = complete_step(
            &provider::http_client().unwrap(),
            &config,
            request(),
            true,
            &mut |_| {},
            &Cancellation::default(),
            &mut outcome(),
        )
        .await
        .unwrap_err();
        assert!(matches!(error, ProviderError::BadRequest(_)));
        assert_eq!(seen.lock().unwrap().len(), 2);
    }
}

#[tokio::test]
async fn ordinary_errors_and_successful_answers_do_not_retry() {
    let kind = ProviderKind::OpenaiCompatible;
    for (status, last, expected) in [
        (401, true, "AI_UNAUTHORIZED"),
        (429, true, "AI_RATE_LIMITED"),
        (500, true, "AI_SERVER_ERROR"),
        (400, false, "AI_BAD_REQUEST"),
    ] {
        let (config, seen) = server(kind, vec![(status, "failure".into())]).await;
        let error = complete_step(
            &provider::http_client().unwrap(),
            &config,
            request(),
            last,
            &mut |_| {},
            &Cancellation::default(),
            &mut outcome(),
        )
        .await
        .unwrap_err();
        assert_eq!(error.code(), expected);
        assert_eq!(seen.lock().unwrap().len(), 1);
    }
    let (config, seen) = server(kind, vec![reply(kind, "Answer", false)]).await;
    complete_step(
        &provider::http_client().unwrap(),
        &config,
        request(),
        true,
        &mut |_| {},
        &Cancellation::default(),
        &mut outcome(),
    )
    .await
    .unwrap();
    assert_eq!(seen.lock().unwrap().len(), 1);
    let (config, seen) = server(kind, vec![reply(kind, "", false)]).await;
    let mut plain = request();
    plain.tools.clear();
    plain.disable_tools = false;
    let error = complete_step(
        &provider::http_client().unwrap(),
        &config,
        plain,
        true,
        &mut |_| {},
        &Cancellation::default(),
        &mut outcome(),
    )
    .await
    .unwrap_err();
    assert!(matches!(error, ProviderError::InvalidResponse(_)));
    assert_eq!(seen.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn cancellation_during_the_first_response_prevents_the_fallback() {
    let kind = ProviderKind::OpenaiCompatible;
    let (config, seen) = server(kind, vec![reply(kind, "plan", true)]).await;
    let cancel = Cancellation::default();
    let error = complete_step(
        &provider::http_client().unwrap(),
        &config,
        request(),
        true,
        &mut |event| {
            if matches!(event, AgentEvent::TextDelta { .. }) {
                cancel.cancel();
            }
        },
        &cancel,
        &mut outcome(),
    )
    .await
    .unwrap_err();
    assert_eq!(error, ProviderError::Cancelled);
    assert_eq!(seen.lock().unwrap().len(), 1);
}
