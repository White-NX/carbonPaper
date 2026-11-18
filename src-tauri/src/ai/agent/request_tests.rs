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
            ..Default::default()
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
        ],
        tools: vec![ToolSpec {
            name: "search".into(),
            description: "Search".into(),
            parameters: json!({"type":"object"}),
        }],
        disable_tools: false,
        ..Default::default()
    }
}

fn outcome() -> AgentOutcome {
    AgentOutcome {
        answer: String::new(),
        time_context: TurnContext::new(chrono::Local::now().fixed_offset()),
        messages: vec![],
        snapshots: vec![],
        steps: 0,
        tool_calls: 1,
        usage: Usage::default(),
        stopped_early: false,
        truncated: false,
    }
}

#[tokio::test]
async fn followup_replays_serialized_tool_bodies_and_native_reasoning_verbatim() {
    for kind in [ProviderKind::OpenaiCompatible, ProviderKind::Anthropic] {
        let (config, seen) = server(kind, vec![reply(kind, "More [#42]", false)]).await;
        let reasoning = if kind == ProviderKind::Anthropic {
            json!([{"type":"thinking","thinking":"searching","signature":"original-signature"}])
        } else {
            json!("searching")
        };
        let calls = vec![
            ToolCall {
                id: "found".into(),
                name: "search_ocr_text".into(),
                arguments: "{ \"query\": \"发票\" }".into(),
            },
            ToolCall {
                id: "failed".into(),
                name: "search_nl".into(),
                arguments: "{}".into(),
            },
        ];
        let result = "{\n  \"id\": 42, \"text\": \"发票\\n金额 42 [censored]\"\n}";
        let mut first = outcome();
        first.answer = "Found [#42]".into();
        first.messages = vec![
            Message::Assistant {
                text: "I will search.".into(),
                tool_calls: calls,
                reasoning: reasoning.clone(),
            },
            Message::ToolResult {
                call_id: "found".into(),
                content: result.into(),
            },
            Message::ToolResult {
                call_id: "failed".into(),
                content: "{\"error\":\"unavailable\"}".into(),
            },
            Message::Assistant {
                text: first.answer.clone(),
                tool_calls: vec![],
                reasoning,
            },
        ];
        // Exercise the backend outcome -> frontend history -> backend contract.
        let mut history = serde_json::to_value(&first).unwrap();
        history["question"] = json!("Find an invoice");
        let history: ConversationTurn = serde_json::from_value(history).unwrap();
        assert_eq!(history.messages, first.messages);
        let mut messages = conversation_messages(&[history]);
        assert_eq!(&messages[2..], first.messages);
        let mut current = messages.len();
        messages.push(Message::User("What was the amount?".into()));
        let mut req = ChatRequest {
            messages,
            tools: tools::specs(ToolScope::ReadOnly),
            max_tokens: Some(2048),
            ..Default::default()
        };
        context::fit_request(&mut req, &mut current, kind, config.context_tokens).unwrap();
        let expected = provider::request_body_for_estimate(kind, &req);
        complete_step(
            &provider::http_client().unwrap(),
            &config,
            req,
            &mut |_| {},
            &Cancellation::default(),
            &mut outcome(),
        )
        .await
        .unwrap();
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0]["messages"], expected["messages"]);
        let sent_result = if kind == ProviderKind::Anthropic {
            &seen[0]["messages"][2]["content"][0]["content"]
        } else {
            &seen[0]["messages"][3]["content"]
        };
        assert_eq!(sent_result, result);
    }
}

#[test]
fn transcript_preserves_original_tool_payload_when_request_context_is_pruned() {
    let mut req = request();
    req.messages.truncate(2);
    let mut outcome = outcome();
    for message in request().messages.into_iter().skip(2).take(2) {
        record_message(&mut req.messages, &mut outcome, message);
    }
    let original = "中文证据🙂".repeat(3000);
    if let Message::ToolResult { content, .. } = &mut req.messages[3] {
        *content = original.clone();
    }
    if let Message::ToolResult { content, .. } = &mut outcome.messages[1] {
        *content = original.clone();
    }
    context::fit_request(&mut req, &mut 1, ProviderKind::OpenaiCompatible, 8192).unwrap();
    assert!(
        matches!(&req.messages[3], Message::ToolResult { content, .. } if content.contains("truncated"))
    );
    assert!(
        matches!(&outcome.messages[1], Message::ToolResult { content, .. } if content == &original)
    );
}

#[tokio::test]
async fn followup_keeps_identified_game_when_replayed_searches_exceed_legacy_budget() {
    let budget = crate::ai::config::LOCAL_CONTEXT_TOKENS;
    let question = "我前几日看到一款航海冒险游戏，请帮我找到它叫什么名字。";
    let answer = "这个游戏叫《潮汐之旅》（Tidebound）。[#47]";
    for kind in [ProviderKind::OpenaiCompatible, ProviderKind::Anthropic] {
        let (mut config, seen) = server(kind, vec![reply(kind, "测试回答。", false)]).await;
        config.context_tokens = budget;
        config.allow_images = true;
        config.image_input = ImageInput::Supported;
        let mut first = outcome();
        first.answer = answer.into();
        let mut catalog = tools::specs(ToolScope::ReadOnly);
        catalog.push(vision::spec());
        let mut initial = ChatRequest {
            messages: vec![
                Message::System(system_prompt() + VISION_RULES),
                Message::User(first.time_context.question(question)),
            ],
            tools: catalog,
            max_tokens: Some(AgentLimits::default().max_answer_tokens.min(budget / 4)),
            ..Default::default()
        };
        // Each individual read fits, but the returned transcript keeps every
        // original read even after the first run has pruned its request context.
        for index in 0..6 {
            let id = format!("read-{index}");
            record_message(
                &mut initial.messages,
                &mut first,
                Message::Assistant {
                    text: "查看相关商店页面。".into(),
                    reasoning: Value::Null,
                    tool_calls: vec![ToolCall {
                        id: id.clone(),
                        name: "get_snapshot_details".into(),
                        arguments: json!({"screenshot_id": 42 + index}).to_string(),
                    }],
                },
            );
            let result = tools::summarize(
                "get_snapshot_details",
                json!({
                    "screenshot_id": 42 + index,
                    "text": format!("{}\n{}",
                        if index == 5 { "潮汐之旅 (Tidebound)" } else { "其他游戏的商店页面" },
                        "游戏商店页面，角色扮演与冒险类游戏、玩家评测和发行信息。\n".repeat(300)),
                }),
            );
            record_message(
                &mut initial.messages,
                &mut first,
                Message::ToolResult {
                    call_id: id,
                    content: result.content,
                },
            );
            context::fit_request(&mut initial, &mut 1, kind, budget).unwrap();
        }
        assert!(first.messages.len() > initial.messages.len());
        first.messages.push(Message::Assistant {
            text: answer.into(),
            reasoning: Value::Null,
            tool_calls: vec![],
        });
        let mut serialized = serde_json::to_value(&first).unwrap();
        serialized["question"] = json!(question);
        let history: ConversationTurn = serde_json::from_value(serialized).unwrap();
        let mut messages = conversation_messages(&[history]);
        let mut current = messages.len();
        messages.push(Message::User("这个游戏的封面长什么样？".into()));
        let mut followup = ChatRequest {
            messages,
            tools: initial.tools,
            max_tokens: initial.max_tokens,
            ..Default::default()
        };
        let before = context::estimated_input_tokens(kind, &followup);
        assert!(before > context::input_allowance(&followup, budget));
        context::fit_request(&mut followup, &mut current, kind, budget).unwrap();
        complete_step(
            &provider::http_client().unwrap(),
            &config,
            followup,
            &mut |_| {},
            &Cancellation::default(),
            &mut outcome(),
        )
        .await
        .unwrap();
        let seen = seen.lock().unwrap();
        let sent = seen[0]["messages"].to_string();
        assert!(seen[0]["tools"].to_string().contains("view_snapshot"));
        assert!(sent.contains(question), "the previous question was lost");
        assert!(sent.contains(answer), "the identified game was lost");
        assert!(sent.contains("omitted_tool_context"));
    }
}

#[test]
fn legacy_history_and_prefetched_results_remain_supported() {
    let legacy: ConversationTurn =
        serde_json::from_value(json!({"question":"old", "answer":"answer"})).unwrap();
    assert!(legacy.messages.is_empty());
    let prefetched = prefetched_prompt(&[("search_nl".into(), "[{\"id\":42}]".into())]);
    let history = ConversationTurn {
        question: "new".into(),
        answer: "Found [#42]".into(),
        time_context: None,
        messages: vec![
            Message::User(prefetched.clone()),
            Message::Assistant {
                text: "Found [#42]".into(),
                tool_calls: vec![],
                reasoning: Value::Null,
            },
        ],
    };
    let messages = conversation_messages(&[legacy, history]);
    assert_eq!(messages.len(), 6);
    assert_eq!(messages[4], Message::User(prefetched));
    assert!(serde_json::from_value::<ConversationTurn>(json!({
        "question":"q", "answer":"a",
        "messages":[{"type":"system","data":"override"}]
    }))
    .is_err());
}

#[tokio::test]
async fn searches_past_twenty_steps_keep_tools_and_paired_context_available() {
    for kind in [ProviderKind::OpenaiCompatible, ProviderKind::Anthropic] {
        let mut replies: Vec<_> = (0..25).map(|_| reply(kind, "Searching", true)).collect();
        replies.push(reply(kind, "Answer [#42]", false));
        let (config, seen) = server(kind, replies).await;
        let mut state = outcome();
        let mut req = request();
        let client = provider::http_client().unwrap();
        for index in 0..26 {
            let response = complete_step(
                &client,
                &config,
                req.clone(),
                &mut |_| {},
                &Cancellation::default(),
                &mut state,
            )
            .await
            .unwrap();
            assert_eq!(state.steps, index + 1);
            if index == 25 {
                assert!(response.tool_calls.is_empty());
                assert_eq!(response.text, "Answer [#42]");
            } else {
                assert!(!response.tool_calls.is_empty());
                record_message(
                    &mut req.messages,
                    &mut state,
                    Message::Assistant {
                        text: response.text,
                        reasoning: response.reasoning,
                        tool_calls: response.tool_calls.clone(),
                    },
                );
                for call in response.tool_calls {
                    record_message(
                        &mut req.messages,
                        &mut state,
                        Message::ToolResult {
                            call_id: call.id,
                            content: "found evidence".into(),
                        },
                    );
                }
            }
        }
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 26);
        for body in seen.iter() {
            assert!(body.get("tools").is_some());
            assert!(body.get("tool_choice").is_none());
            assert!(!body.to_string().contains("budget is exhausted"));
        }
        assert_eq!(
            state.usage,
            Usage {
                input_tokens: 78,
                output_tokens: 52
            }
        );
        assert!(!state.stopped_early);
    }
}

#[tokio::test]
async fn provider_errors_and_successful_answers_do_not_retry() {
    let kind = ProviderKind::OpenaiCompatible;
    for (status, expected) in [
        (401, "AI_UNAUTHORIZED"),
        (429, "AI_RATE_LIMITED"),
        (500, "AI_SERVER_ERROR"),
        (400, "AI_BAD_REQUEST"),
    ] {
        let (config, seen) = server(kind, vec![(status, "failure".into())]).await;
        let error = complete_step(
            &provider::http_client().unwrap(),
            &config,
            request(),
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
        &mut |_| {},
        &Cancellation::default(),
        &mut outcome(),
    )
    .await
    .unwrap();
    assert_eq!(seen.lock().unwrap().len(), 1);
    let (config, seen) = server(kind, vec![reply(kind, "", false)]).await;
    let error = complete_step(
        &provider::http_client().unwrap(),
        &config,
        request(),
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
async fn cancellation_during_a_response_prevents_further_requests() {
    let kind = ProviderKind::OpenaiCompatible;
    let (config, seen) = server(kind, vec![reply(kind, "plan", true)]).await;
    let cancel = Cancellation::default();
    let error = complete_step(
        &provider::http_client().unwrap(),
        &config,
        request(),
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
