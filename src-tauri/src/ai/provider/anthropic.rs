//! The Anthropic Messages protocol.
//!
//! Differences from Chat Completions that matter here: the system prompt is a
//! top-level field, `max_tokens` is required, tool calls and results are
//! content blocks, and all tool results for one step go in a single user
//! message. Sampling parameters are left out because current Claude models
//! reject them.

use futures::StreamExt;
use serde_json::{json, Value};

use super::sse::SseParser;
use super::{
    ChatRequest, ChatResponse, Message, ProviderError, StreamEvent, ToolCall, Usage,
    STREAM_IDLE_TIMEOUT,
};
use crate::ai::config::ResolvedProvider;

const API_VERSION: &str = "2023-06-01";
const DEFAULT_MAX_TOKENS: u32 = 8_192;

pub(super) fn endpoint(base_url: &str) -> String {
    let base = base_url.trim_end_matches('/');
    if base.ends_with("/messages") {
        base.to_string()
    } else if base.ends_with("/v1") {
        format!("{base}/messages")
    } else {
        format!("{base}/v1/messages")
    }
}

pub(super) fn request_body(model: &str, request: &ChatRequest) -> Value {
    let mut system = Vec::new();
    let mut messages: Vec<Value> = Vec::new();
    // Appends content to the last message when it has the same role, so tool
    // results and a follow-up instruction share one user turn.
    let mut push = |role: &str, blocks: Vec<Value>| {
        if let Some(last) = messages.last_mut() {
            if last["role"] == role {
                if let Some(content) = last["content"].as_array_mut() {
                    content.extend(blocks);
                    return;
                }
            }
        }
        messages.push(json!({ "role": role, "content": blocks }));
    };

    for message in &request.messages {
        match message {
            Message::System(text) => system.push(text.as_str()),
            Message::User(text) => push("user", vec![json!({ "type": "text", "text": text })]),
            Message::Assistant { text, tool_calls } => {
                let mut blocks = Vec::new();
                if !text.is_empty() {
                    blocks.push(json!({ "type": "text", "text": text }));
                }
                for call in tool_calls {
                    let input: Value = serde_json::from_str(&call.arguments)
                        .ok()
                        .filter(Value::is_object)
                        .unwrap_or_else(|| json!({}));
                    blocks.push(json!({
                        "type": "tool_use", "id": call.id, "name": call.name, "input": input,
                    }));
                }
                if blocks.is_empty() {
                    blocks.push(json!({ "type": "text", "text": "…" }));
                }
                push("assistant", blocks);
            }
            Message::ToolResult { call_id, content } => push(
                "user",
                vec![json!({ "type": "tool_result", "tool_use_id": call_id, "content": content })],
            ),
        }
    }

    let mut body = json!({
        "model": model,
        "max_tokens": request.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS),
        "messages": messages,
        "stream": true,
    });
    if !system.is_empty() {
        body["system"] = json!(system.join("\n\n"));
    }
    if !request.tools.is_empty() {
        body["tools"] = request
            .tools
            .iter()
            .map(|tool| {
                json!({
                    "name": tool.name,
                    "description": tool.description,
                    "input_schema": tool.parameters,
                })
            })
            .collect();
    }
    body
}

pub(super) async fn complete(
    client: &reqwest::Client,
    provider: &ResolvedProvider,
    request: &ChatRequest,
    on_event: &mut (dyn FnMut(StreamEvent) + Send),
) -> Result<ChatResponse, ProviderError> {
    let mut http = client
        .post(endpoint(&provider.base_url))
        .header("anthropic-version", API_VERSION)
        .json(&request_body(&provider.model, request));
    if let Some(key) = provider.api_key.as_deref() {
        http = http.header("x-api-key", key);
    }
    let response = tokio::time::timeout(STREAM_IDLE_TIMEOUT, http.send())
        .await
        .map_err(|_| ProviderError::Timeout)??;

    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(ProviderError::from_status(status.as_u16(), &body));
    }

    let mut stream = response.bytes_stream();
    let mut parser = SseParser::default();
    let mut assembler = StreamAssembler::default();
    loop {
        let chunk = tokio::time::timeout(STREAM_IDLE_TIMEOUT, stream.next())
            .await
            .map_err(|_| ProviderError::Timeout)?;
        let ended = chunk.is_none();
        let events = match chunk {
            Some(bytes) => parser.push(&bytes?),
            None => parser.finish(),
        };
        for event in events {
            if assembler.apply(&event.data, on_event)? {
                return Ok(assembler.finish());
            }
        }
        if ended {
            return Ok(assembler.finish());
        }
    }
}

enum Block {
    Text,
    ToolUse {
        id: String,
        name: String,
        input: String,
    },
    Other,
}

#[derive(Default)]
pub(super) struct StreamAssembler {
    text: String,
    blocks: Vec<(u64, Block)>,
    usage: Usage,
    saw_usage: bool,
    truncated: bool,
}

impl StreamAssembler {
    fn block_mut(&mut self, index: u64) -> Option<&mut Block> {
        self.blocks
            .iter_mut()
            .find(|(i, _)| *i == index)
            .map(|(_, block)| block)
    }

    /// Applies one event payload. Returns `true` at `message_stop`.
    pub(super) fn apply(
        &mut self,
        data: &str,
        on_event: &mut (dyn FnMut(StreamEvent) + Send),
    ) -> Result<bool, ProviderError> {
        let event: Value = serde_json::from_str(data)
            .map_err(|e| ProviderError::InvalidResponse(format!("stream event: {e}")))?;
        let index = event.get("index").and_then(Value::as_u64).unwrap_or(0);
        match event.get("type").and_then(Value::as_str).unwrap_or("") {
            "message_start" => {
                if let Some(usage) = event.pointer("/message/usage") {
                    self.read_usage(usage);
                }
            }
            "content_block_start" => {
                let block = event.get("content_block").cloned().unwrap_or(Value::Null);
                let parsed = match block.get("type").and_then(Value::as_str) {
                    Some("text") => {
                        if let Some(text) = block.get("text").and_then(Value::as_str) {
                            self.push_text(text, on_event);
                        }
                        Block::Text
                    }
                    Some("tool_use") => Block::ToolUse {
                        id: block
                            .get("id")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string(),
                        name: block
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string(),
                        input: String::new(),
                    },
                    _ => Block::Other,
                };
                self.blocks.push((index, parsed));
            }
            "content_block_delta" => {
                let delta = event.get("delta").cloned().unwrap_or(Value::Null);
                match delta.get("type").and_then(Value::as_str) {
                    Some("text_delta") => {
                        if let Some(text) = delta.get("text").and_then(Value::as_str) {
                            self.push_text(text, on_event);
                        }
                    }
                    Some("input_json_delta") => {
                        let fragment = delta
                            .get("partial_json")
                            .and_then(Value::as_str)
                            .unwrap_or("");
                        if let Some(Block::ToolUse { input, .. }) = self.block_mut(index) {
                            input.push_str(fragment);
                        }
                    }
                    _ => {}
                }
            }
            "message_delta" => {
                if event.pointer("/delta/stop_reason").and_then(Value::as_str) == Some("max_tokens")
                {
                    self.truncated = true;
                }
                if let Some(usage) = event.get("usage") {
                    self.read_usage(usage);
                }
            }
            "message_stop" => return Ok(true),
            "error" => {
                let message = event
                    .pointer("/error/message")
                    .and_then(Value::as_str)
                    .unwrap_or("stream error")
                    .to_string();
                return Err(match event.pointer("/error/type").and_then(Value::as_str) {
                    Some("rate_limit_error") => ProviderError::RateLimited,
                    Some("authentication_error" | "permission_error") => {
                        ProviderError::Unauthorized
                    }
                    _ => ProviderError::Server(message),
                });
            }
            _ => {}
        }
        Ok(false)
    }

    fn push_text(&mut self, text: &str, on_event: &mut (dyn FnMut(StreamEvent) + Send)) {
        if !text.is_empty() {
            self.text.push_str(text);
            on_event(StreamEvent::TextDelta(text.to_string()));
        }
    }

    /// Usage arrives in pieces: input on `message_start`, running output
    /// totals on `message_delta`. Later values replace earlier ones.
    fn read_usage(&mut self, usage: &Value) {
        let field = |name: &str| usage.get(name).and_then(Value::as_u64);
        let input = field("input_tokens").unwrap_or(0)
            + field("cache_read_input_tokens").unwrap_or(0)
            + field("cache_creation_input_tokens").unwrap_or(0);
        if input > 0 {
            self.usage.input_tokens = input;
            self.saw_usage = true;
        }
        if let Some(output) = field("output_tokens") {
            self.usage.output_tokens = output;
            self.saw_usage = true;
        }
    }

    pub(super) fn finish(self) -> ChatResponse {
        ChatResponse {
            text: self.text,
            tool_calls: self
                .blocks
                .into_iter()
                .filter_map(|(_, block)| match block {
                    Block::ToolUse { id, name, input } if !name.is_empty() => Some(ToolCall {
                        id,
                        name,
                        arguments: if input.trim().is_empty() {
                            "{}".into()
                        } else {
                            input
                        },
                    }),
                    _ => None,
                })
                .collect(),
            usage: self.saw_usage.then_some(self.usage),
            truncated: self.truncated,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::provider::ToolSpec;

    #[test]
    fn endpoint_accepts_the_common_base_url_forms() {
        assert_eq!(
            endpoint("https://api.anthropic.com"),
            "https://api.anthropic.com/v1/messages"
        );
        assert_eq!(
            endpoint("https://api.anthropic.com/v1/"),
            "https://api.anthropic.com/v1/messages"
        );
        assert_eq!(
            endpoint("https://proxy/v1/messages"),
            "https://proxy/v1/messages"
        );
    }

    #[test]
    fn request_body_groups_tool_results_into_one_user_turn() {
        let request = ChatRequest {
            messages: vec![
                Message::System("sys".into()),
                Message::User("q".into()),
                Message::Assistant {
                    text: String::new(),
                    tool_calls: vec![
                        ToolCall {
                            id: "a".into(),
                            name: "t".into(),
                            arguments: "{\"x\":1}".into(),
                        },
                        ToolCall {
                            id: "b".into(),
                            name: "t".into(),
                            arguments: "not json".into(),
                        },
                    ],
                },
                Message::ToolResult {
                    call_id: "a".into(),
                    content: "1".into(),
                },
                Message::ToolResult {
                    call_id: "b".into(),
                    content: "2".into(),
                },
                Message::User("answer now".into()),
            ],
            tools: vec![ToolSpec {
                name: "t".into(),
                description: "d".into(),
                parameters: json!({ "type": "object" }),
            }],
            max_tokens: Some(100),
            temperature: Some(0.2),
        };
        let body = request_body("claude-sonnet-5", &request);
        assert_eq!(body["system"], "sys");
        assert!(body.get("temperature").is_none());
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[1]["content"][0]["input"], json!({ "x": 1 }));
        assert_eq!(messages[1]["content"][1]["input"], json!({}));
        let last = messages[2]["content"].as_array().unwrap();
        assert_eq!(last.len(), 3);
        assert_eq!(last[0]["tool_use_id"], "a");
        assert_eq!(last[2]["type"], "text");
        assert_eq!(
            body["tools"][0]["input_schema"],
            json!({ "type": "object" })
        );
    }

    #[test]
    fn stream_assembles_text_tool_input_and_usage() {
        let mut texts = Vec::new();
        let mut on_event = |event: StreamEvent| {
            let StreamEvent::TextDelta(text) = event;
            texts.push(text);
        };
        let mut assembler = StreamAssembler::default();
        let events = [
            r#"{"type":"message_start","message":{"usage":{"input_tokens":20,"cache_read_input_tokens":5,"output_tokens":1}}}"#,
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Looking"}}"#,
            r#"{"type":"content_block_stop","index":0}"#,
            r#"{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"tu","name":"search_nl","input":{}}}"#,
            r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"query\":"}}"#,
            r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"\"moon\"}"}}"#,
            r#"{"type":"content_block_start","index":2,"content_block":{"type":"tool_use","id":"tu2","name":"get_smart_clusters","input":{}}}"#,
            r#"{"type":"ping"}"#,
            r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":42}}"#,
        ];
        for event in events {
            assert!(!assembler.apply(event, &mut on_event).unwrap());
        }
        assert!(assembler
            .apply(r#"{"type":"message_stop"}"#, &mut on_event)
            .unwrap());
        let response = assembler.finish();
        assert_eq!(texts, vec!["Looking"]);
        assert_eq!(response.tool_calls.len(), 2);
        assert_eq!(response.tool_calls[0].arguments, "{\"query\":\"moon\"}");
        assert_eq!(response.tool_calls[1].arguments, "{}");
        assert_eq!(
            response.usage,
            Some(Usage {
                input_tokens: 25,
                output_tokens: 42
            })
        );
        assert!(!response.truncated);
    }

    #[test]
    fn stream_errors_map_to_provider_errors() {
        let mut ignore = |_: StreamEvent| {};
        let mut assembler = StreamAssembler::default();
        assert_eq!(
            assembler
                .apply(r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#, &mut ignore)
                .unwrap_err(),
            ProviderError::Server("Overloaded".into())
        );
    }
}
