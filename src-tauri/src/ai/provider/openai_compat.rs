//! The Chat Completions protocol used by OpenAI and most compatible hosts,
//! including DeepSeek, Qwen, Kimi, OpenRouter, Ollama and LM Studio.
//!
//! Parsing is deliberately lenient: unknown fields are ignored, missing ones
//! default, and tool-call arguments are assembled from however many fragments
//! the host splits them into.

use futures::StreamExt;
use serde_json::{json, Value};

use super::sse::SseParser;
use super::{
    ChatRequest, ChatResponse, Message, ProviderError, StreamEvent, ToolCall, Usage,
    STREAM_IDLE_TIMEOUT,
};
use crate::ai::config::ResolvedProvider;

pub(super) fn endpoint(base_url: &str) -> String {
    let base = base_url.trim_end_matches('/');
    if base.ends_with("/chat/completions") {
        base.to_string()
    } else {
        format!("{base}/chat/completions")
    }
}

pub(super) fn request_body(model: &str, request: &ChatRequest) -> Value {
    let messages: Vec<Value> = request
        .messages
        .iter()
        .map(|message| match message {
            Message::System(text) => json!({ "role": "system", "content": text }),
            Message::User(text) => json!({ "role": "user", "content": text }),
            Message::Assistant { text, tool_calls } if tool_calls.is_empty() => {
                json!({ "role": "assistant", "content": text })
            }
            Message::Assistant { text, tool_calls } => json!({
                "role": "assistant",
                "content": if text.is_empty() { Value::Null } else { json!(text) },
                "tool_calls": tool_calls.iter().map(|call| json!({
                    "id": call.id,
                    "type": "function",
                    "function": { "name": call.name, "arguments": call.arguments },
                })).collect::<Vec<_>>(),
            }),
            Message::ToolResult { call_id, content } => json!({ "role": "tool", "tool_call_id": call_id, "content": content }),
        })
        .collect();

    let mut body = json!({ "model": model, "messages": messages, "stream": true });
    if !request.tools.is_empty() {
        body["tools"] = request
            .tools
            .iter()
            .map(|tool| {
                json!({
                    "type": "function",
                    "function": {
                        "name": tool.name,
                        "description": tool.description,
                        "parameters": tool.parameters,
                    }
                })
            })
            .collect();
    }
    if let Some(max_tokens) = request.max_tokens {
        body["max_tokens"] = json!(max_tokens);
    }
    if let Some(temperature) = request.temperature {
        body["temperature"] = json!(temperature);
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
        .json(&request_body(&provider.model, request));
    if let Some(key) = provider.api_key.as_deref() {
        http = http.bearer_auth(key);
    }
    let response = tokio::time::timeout(STREAM_IDLE_TIMEOUT, http.send())
        .await
        .map_err(|_| ProviderError::Timeout)??;

    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(ProviderError::from_status(status.as_u16(), &body));
    }

    // A host that ignores `stream: true` answers with one JSON document.
    let is_json = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("application/json"));
    if is_json {
        let body: Value = response.json().await?;
        let response = parse_full_response(&body)?;
        if !response.text.is_empty() {
            on_event(StreamEvent::TextDelta(response.text.clone()));
        }
        return Ok(response);
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

#[derive(Default)]
struct PartialCall {
    id: String,
    name: String,
    arguments: String,
}

#[derive(Default)]
pub(super) struct StreamAssembler {
    text: String,
    calls: Vec<PartialCall>,
    usage: Option<Usage>,
    truncated: bool,
}

impl StreamAssembler {
    /// Applies one `data:` payload. Returns `true` at the end-of-stream marker.
    pub(super) fn apply(
        &mut self,
        data: &str,
        on_event: &mut (dyn FnMut(StreamEvent) + Send),
    ) -> Result<bool, ProviderError> {
        let data = data.trim();
        if data == "[DONE]" {
            return Ok(true);
        }
        if data.is_empty() {
            return Ok(false);
        }
        let chunk: Value = serde_json::from_str(data)
            .map_err(|e| ProviderError::InvalidResponse(format!("stream chunk: {e}")))?;
        if let Some(error) = chunk.get("error") {
            return Err(ProviderError::Server(
                error
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("stream error")
                    .to_string(),
            ));
        }
        if let Some(usage) = parse_usage(chunk.get("usage")) {
            self.usage = Some(usage);
        }
        let Some(choice) = chunk.pointer("/choices/0") else {
            return Ok(false);
        };
        if choice.get("finish_reason").and_then(Value::as_str) == Some("length") {
            self.truncated = true;
        }
        let Some(delta) = choice.get("delta") else {
            return Ok(false);
        };
        if let Some(text) = delta.get("content").and_then(Value::as_str) {
            if !text.is_empty() {
                self.text.push_str(text);
                on_event(StreamEvent::TextDelta(text.to_string()));
            }
        }
        for (position, call) in delta
            .get("tool_calls")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .enumerate()
        {
            let index = call
                .get("index")
                .and_then(Value::as_u64)
                .map(|i| i as usize)
                .unwrap_or(position);
            while self.calls.len() <= index {
                self.calls.push(PartialCall::default());
            }
            let partial = &mut self.calls[index];
            if let Some(id) = call.get("id").and_then(Value::as_str) {
                if !id.is_empty() {
                    partial.id = id.to_string();
                }
            }
            if let Some(function) = call.get("function") {
                if let Some(name) = function.get("name").and_then(Value::as_str) {
                    partial.name.push_str(name);
                }
                match function.get("arguments") {
                    Some(Value::String(fragment)) => partial.arguments.push_str(fragment),
                    // A few hosts send the arguments as an already-parsed object.
                    Some(value @ Value::Object(_)) => partial.arguments = value.to_string(),
                    _ => {}
                }
            }
        }
        Ok(false)
    }

    pub(super) fn finish(self) -> ChatResponse {
        ChatResponse {
            text: self.text,
            tool_calls: self
                .calls
                .into_iter()
                .enumerate()
                .filter(|(_, call)| !call.name.is_empty())
                .map(|(index, call)| ToolCall {
                    id: if call.id.is_empty() {
                        format!("call_{index}")
                    } else {
                        call.id
                    },
                    name: call.name,
                    arguments: call.arguments,
                })
                .collect(),
            usage: self.usage,
            truncated: self.truncated,
        }
    }
}

fn parse_usage(value: Option<&Value>) -> Option<Usage> {
    let usage = value?;
    Some(Usage {
        input_tokens: usage.get("prompt_tokens")?.as_u64()?,
        output_tokens: usage
            .get("completion_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
    })
}

fn parse_full_response(body: &Value) -> Result<ChatResponse, ProviderError> {
    let choice = body
        .pointer("/choices/0")
        .ok_or_else(|| ProviderError::InvalidResponse("no choices in response".into()))?;
    let message = choice.get("message").cloned().unwrap_or(Value::Null);
    let tool_calls = message
        .get("tool_calls")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
        .filter_map(|(index, call)| {
            let function = call.get("function")?;
            let arguments = match function.get("arguments") {
                Some(Value::String(s)) => s.clone(),
                Some(other) => other.to_string(),
                None => String::new(),
            };
            Some(ToolCall {
                id: call
                    .get("id")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("call_{index}")),
                name: function.get("name")?.as_str()?.to_string(),
                arguments,
            })
        })
        .collect();
    Ok(ChatResponse {
        text: message
            .get("content")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        tool_calls,
        usage: parse_usage(body.get("usage")),
        truncated: choice.get("finish_reason").and_then(Value::as_str) == Some("length"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::provider::ToolSpec;

    #[test]
    fn endpoint_accepts_base_urls_with_or_without_the_path() {
        assert_eq!(
            endpoint("https://api.deepseek.com/v1/"),
            "https://api.deepseek.com/v1/chat/completions"
        );
        assert_eq!(
            endpoint("http://localhost:11434/v1/chat/completions"),
            "http://localhost:11434/v1/chat/completions"
        );
    }

    #[test]
    fn request_body_maps_tool_turns_and_omits_empty_tools() {
        let request = ChatRequest {
            messages: vec![
                Message::System("sys".into()),
                Message::Assistant {
                    text: String::new(),
                    tool_calls: vec![ToolCall {
                        id: "c1".into(),
                        name: "search_ocr_text".into(),
                        arguments: "{\"query\":\"x\"}".into(),
                    }],
                },
                Message::ToolResult {
                    call_id: "c1".into(),
                    content: "[]".into(),
                },
            ],
            ..Default::default()
        };
        let body = request_body("m", &request);
        assert!(body.get("tools").is_none());
        assert_eq!(body["messages"][1]["content"], Value::Null);
        assert_eq!(
            body["messages"][1]["tool_calls"][0]["function"]["name"],
            "search_ocr_text"
        );
        assert_eq!(body["messages"][2]["tool_call_id"], "c1");

        let with_tools = ChatRequest {
            tools: vec![ToolSpec {
                name: "t".into(),
                description: "d".into(),
                parameters: json!({"type": "object"}),
            }],
            ..Default::default()
        };
        assert_eq!(
            request_body("m", &with_tools)["tools"][0]["function"]["name"],
            "t"
        );
    }

    #[test]
    fn stream_assembles_text_and_fragmented_tool_calls() {
        let mut texts = Vec::new();
        let mut on_event = |event: StreamEvent| {
            let StreamEvent::TextDelta(text) = event;
            texts.push(text);
        };
        let mut assembler = StreamAssembler::default();
        let chunks = [
            r#"{"choices":[{"delta":{"role":"assistant","content":"Hi"}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"a","function":{"name":"search","arguments":"{\"q\""}}]}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":":\"x\"}"}}]}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":1,"function":{"name":"other","arguments":{"k":1}}}]}}]}"#,
            r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":10,"completion_tokens":3}}"#,
        ];
        for chunk in chunks {
            assert!(!assembler.apply(chunk, &mut on_event).unwrap());
        }
        assert!(assembler.apply("[DONE]", &mut on_event).unwrap());
        let response = assembler.finish();
        assert_eq!(texts, vec!["Hi"]);
        assert_eq!(response.text, "Hi");
        assert_eq!(
            response.tool_calls,
            vec![
                ToolCall {
                    id: "a".into(),
                    name: "search".into(),
                    arguments: "{\"q\":\"x\"}".into()
                },
                ToolCall {
                    id: "call_1".into(),
                    name: "other".into(),
                    arguments: "{\"k\":1}".into()
                },
            ]
        );
        assert_eq!(
            response.usage,
            Some(Usage {
                input_tokens: 10,
                output_tokens: 3
            })
        );
    }

    #[test]
    fn stream_errors_and_non_streaming_replies_are_understood() {
        let mut ignore = |_: StreamEvent| {};
        let mut assembler = StreamAssembler::default();
        assert!(matches!(
            assembler.apply(r#"{"error":{"message":"overloaded"}}"#, &mut ignore),
            Err(ProviderError::Server(m)) if m == "overloaded"
        ));

        let full = json!({
            "choices": [{
                "message": { "content": "done", "tool_calls": [
                    { "id": "x", "function": { "name": "t", "arguments": "{}" } }
                ]},
                "finish_reason": "length"
            }]
        });
        let response = parse_full_response(&full).unwrap();
        assert_eq!(response.text, "done");
        assert_eq!(response.tool_calls[0].name, "t");
        assert!(response.truncated);
    }
}
