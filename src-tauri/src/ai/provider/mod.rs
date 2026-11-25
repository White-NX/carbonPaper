//! A small, protocol-neutral chat interface over the supported endpoint kinds.
//!
//! The agent loop speaks only these types. Each protocol module turns them into
//! its own request body and parses its own stream back into [`ChatResponse`].

mod anthropic;
mod openai_compat;
mod sse;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::Notify;

use super::config::{ImageInput, ProviderKind, ResolvedProvider};

/// How long a stream may stay silent before the request counts as stalled.
/// Reasoning models can think for a while before the first token.
pub(crate) const STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(120);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_ERROR_BODY_CHARS: usize = 400;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum Message {
    // Replayed frontend history cannot supply system instructions.
    #[serde(skip_deserializing)]
    System(String),
    /// Backend-supplied quoted context whose boundary can be cached separately
    /// from the current user input. It carries no system-instruction authority.
    #[serde(skip_deserializing)]
    Context(String),
    User(String),
    Assistant {
        text: String,
        tool_calls: Vec<ToolCall>,
        reasoning: Value,
    },
    ToolResult {
        call_id: String,
        content: String,
    },
    /// Only backend reads can supply pixels. Neither frontend history nor the
    /// returned conversation transcript can serialize image bytes.
    #[serde(skip)]
    SnapshotImage {
        handle: String,
        call_id: String,
        screenshot_id: i64,
        image: ImagePayload,
    },
    /// Opaque backend cache handle, safe to replay through the frontend.
    SnapshotImageRef {
        handle: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ImageDetail {
    #[default]
    High,
    Original,
}

#[derive(Clone, PartialEq)]
pub struct ImagePayload {
    pub data: Arc<str>,
    pub mime_type: String,
    pub width: u32,
    pub height: u32,
    pub detail: ImageDetail,
}

impl std::fmt::Debug for ImagePayload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ImagePayload")
            .field("mime_type", &self.mime_type)
            .field("width", &self.width)
            .field("height", &self.height)
            .field("detail", &self.detail)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    /// Raw JSON text as the model produced it; it may be malformed.
    pub arguments: String,
}

#[derive(Debug, Clone)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

#[derive(Debug, Clone, Default)]
pub struct ChatRequest {
    pub messages: Vec<Message>,
    pub tools: Vec<ToolSpec>,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
    /// Retain tool schemas in the cached prefix when the last step must answer.
    pub disable_tools: bool,
}

/// Use the actual protocol shape when budgeting, including tool schemas and
/// provider-specific reasoning blocks. Credentials are never part of this body.
pub(crate) fn request_body_for_estimate(kind: ProviderKind, request: &ChatRequest) -> Value {
    // Count image dimensions separately; base64 is transport encoding rather
    // than text input. Sharing pixel buffers also keeps tool steps inexpensive.
    let mut request = request.clone();
    for message in &mut request.messages {
        if let Message::SnapshotImage { image, .. } = message {
            image.data = Arc::from("");
        }
    }
    match kind {
        ProviderKind::OpenaiCompatible => openai_compat::request_body("", &request),
        ProviderKind::Anthropic => anthropic::request_body("", &request),
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
}

#[derive(Debug, Clone, Default)]
pub struct ChatResponse {
    /// Optional provider-reported subset of output_tokens; never add it twice.
    pub reasoning_tokens: Option<u64>,
    /// Provider-native reasoning required when continuing a tool turn.
    pub reasoning: Value,
    pub text: String,
    pub tool_calls: Vec<ToolCall>,
    pub usage: Option<Usage>,
    /// The response stopped because it reached the output limit.
    pub truncated: bool,
}

/// Incremental output delivered while a response streams in.
#[derive(Debug, Clone, PartialEq)]
pub enum StreamEvent {
    TextDelta(String),
    ReasoningDelta(String),
}

/// Error categories the interface can explain to the user. The `Display`
/// form is `AI_<CODE>` optionally followed by `: detail`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderError {
    Unauthorized,
    NotFound(String),
    RateLimited,
    BadRequest(String),
    Server(String),
    Network(String),
    Timeout,
    Cancelled,
    ContextLimit,
    ImagesDisabled,
    InvalidResponse(String),
}

impl ProviderError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Unauthorized => "AI_UNAUTHORIZED",
            Self::NotFound(_) => "AI_NOT_FOUND",
            Self::RateLimited => "AI_RATE_LIMITED",
            Self::BadRequest(_) => "AI_BAD_REQUEST",
            Self::Server(_) => "AI_SERVER_ERROR",
            Self::Network(_) => "AI_NETWORK_ERROR",
            Self::Timeout => "AI_TIMEOUT",
            Self::Cancelled => "AI_CANCELLED",
            Self::ContextLimit => "AI_CONTEXT_LIMIT",
            Self::ImagesDisabled => "AI_IMAGES_DISABLED",
            Self::InvalidResponse(_) => "AI_INVALID_RESPONSE",
        }
    }

    fn detail(&self) -> Option<&str> {
        match self {
            Self::NotFound(d)
            | Self::BadRequest(d)
            | Self::Server(d)
            | Self::Network(d)
            | Self::InvalidResponse(d) => Some(d),
            _ => None,
        }
    }

    pub(crate) fn from_status(status: u16, body: &str) -> Self {
        let detail = summarize_error_body(body);
        match status {
            401 | 403 => Self::Unauthorized,
            404 => Self::NotFound(detail),
            408 => Self::Timeout,
            429 => Self::RateLimited,
            400..=499 => Self::BadRequest(detail),
            _ => Self::Server(format!("HTTP {status}: {detail}")),
        }
    }
}

impl std::fmt::Display for ProviderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.detail() {
            Some(detail) if !detail.is_empty() => write!(f, "{}: {}", self.code(), detail),
            _ => f.write_str(self.code()),
        }
    }
}

impl From<reqwest::Error> for ProviderError {
    fn from(e: reqwest::Error) -> Self {
        if e.is_timeout() {
            Self::Timeout
        } else {
            // Drop the URL: it is the user's own endpoint and adds nothing.
            Self::Network(e.without_url().to_string())
        }
    }
}

/// Pulls the human-readable message out of the common JSON error shapes.
fn summarize_error_body(body: &str) -> String {
    let parsed: Option<Value> = serde_json::from_str(body).ok();
    let message = parsed.as_ref().and_then(|v| {
        v.pointer("/error/message")
            .or_else(|| v.get("error").filter(|e| e.is_string()))
            .or_else(|| v.get("message"))
            .or_else(|| v.get("detail"))
            .and_then(Value::as_str)
            .map(str::to_string)
    });
    let text = message.unwrap_or_else(|| body.trim().to_string());
    text.chars().take(MAX_ERROR_BODY_CHARS).collect()
}

/// A cancellation signal shared by the caller and a running request.
#[derive(Default)]
pub struct Cancellation {
    cancelled: AtomicBool,
    notify: Notify,
}

impl Cancellation {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        self.notify.notify_waiters();
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    /// Resolves once [`Self::cancel`] has been called.
    pub async fn cancelled(&self) {
        loop {
            let notified = self.notify.notified();
            if self.is_cancelled() {
                return;
            }
            notified.await;
        }
    }
}

pub fn http_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(CONNECT_TIMEOUT)
        .user_agent(concat!("CarbonPaper/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| format!("Failed to create HTTP client: {e}"))
}

/// Sends one chat request and streams the reply.
pub async fn complete(
    client: &reqwest::Client,
    provider: &ResolvedProvider,
    request: &ChatRequest,
    on_event: &mut (dyn FnMut(StreamEvent) + Send),
    cancel: &Cancellation,
) -> Result<ChatResponse, ProviderError> {
    if cancel.is_cancelled() {
        return Err(ProviderError::Cancelled);
    }
    if request
        .messages
        .iter()
        .any(|m| matches!(m, Message::SnapshotImage { .. }))
        && (!provider.allow_images || provider.image_input != ImageInput::Supported)
    {
        return Err(ProviderError::ImagesDisabled);
    }
    let work = async {
        match provider.kind {
            ProviderKind::OpenaiCompatible => {
                openai_compat::complete(client, provider, request, on_event).await
            }
            ProviderKind::Anthropic => {
                anthropic::complete(client, provider, request, on_event).await
            }
        }
    };
    tokio::select! {
        result = work => result,
        _ = cancel.cancelled() => Err(ProviderError::Cancelled),
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ConnectionTest {
    pub ok: bool,
    pub tool_calling: super::config::ToolCalling,
    pub error: Option<String>,
    pub latency_ms: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ImageConnectionTest {
    pub image_input: ImageInput,
    pub error: Option<String>,
    pub latency_ms: u64,
}

pub async fn test_image_input(provider: &ResolvedProvider) -> ImageConnectionTest {
    let started = std::time::Instant::now();
    let work = async {
        let client = http_client().map_err(ProviderError::Network)?;
        let mut bytes = std::io::Cursor::new(Vec::new());
        let image = image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
            48,
            48,
            image::Rgb([20, 80, 220]),
        ));
        image
            .write_to(&mut bytes, image::ImageFormat::Png)
            .map_err(|_| {
                ProviderError::InvalidResponse("Could not create a synthetic test image.".into())
            })?;
        let mut config = provider.clone();
        // Explicitly testing a generated image does not grant screenshot access.
        config.allow_images = true;
        config.image_input = ImageInput::Supported;
        let request = ChatRequest {
            messages: vec![
                Message::User("Name the dominant color of this image using one color word.".into()),
                Message::SnapshotImage {
                    handle: "image-probe".into(),
                    call_id: "image-probe".into(),
                    screenshot_id: 0,
                    image: ImagePayload {
                        data: Arc::from(base64::Engine::encode(
                            &base64::engine::general_purpose::STANDARD,
                            bytes.into_inner(),
                        )),
                        mime_type: "image/png".into(),
                        width: 48,
                        height: 48,
                        detail: ImageDetail::High,
                    },
                },
            ],
            max_tokens: Some(4096.min(provider.context_tokens / 2)),
            temperature: Some(0.0),
            ..Default::default()
        };
        complete(
            &client,
            &config,
            &request,
            &mut |_| {},
            &Cancellation::default(),
        )
        .await
    };
    let result = tokio::time::timeout(Duration::from_secs(90), work)
        .await
        .unwrap_or(Err(ProviderError::Timeout));
    let (image_input, error) = match result {
        Ok(response)
            if !response.truncated
                && (response.text.to_lowercase().contains("blue")
                    || response.text.contains('蓝')) =>
        {
            (ImageInput::Supported, None)
        }
        Ok(_) => (
            ImageInput::Unknown,
            Some("AI_INVALID_RESPONSE: Image understanding could not be confirmed.".into()),
        ),
        Err(error) => {
            let text = error.to_string().to_lowercase();
            let unsupported = matches!(error, ProviderError::BadRequest(_))
                && (text.contains("image") || text.contains("vision"))
                && [
                    "not supported",
                    "unsupported",
                    "does not support",
                    "only text",
                ]
                .iter()
                .any(|word| text.contains(word));
            (
                if unsupported {
                    ImageInput::Unsupported
                } else {
                    ImageInput::Unknown
                },
                Some(error.to_string()),
            )
        }
    };
    ImageConnectionTest {
        image_input,
        error,
        latency_ms: started.elapsed().as_millis() as u64,
    }
}

/// Checks that the endpoint answers, then whether its model calls tools.
pub async fn test_connection(provider: &ResolvedProvider) -> ConnectionTest {
    use super::config::ToolCalling;

    let started = std::time::Instant::now();
    let finish = |tool_calling, error: Option<ProviderError>| ConnectionTest {
        ok: error.is_none(),
        tool_calling,
        error: error.map(|e| e.to_string()),
        latency_ms: started.elapsed().as_millis() as u64,
    };
    let client = match http_client() {
        Ok(client) => client,
        Err(e) => return finish(ToolCalling::Unknown, Some(ProviderError::Network(e))),
    };
    let cancel = Cancellation::default();
    let mut ignore = |_event: StreamEvent| {};
    // Reasoning tokens count toward the output limit on current cloud models.
    // Leave room for the tool call while respecting small local context budgets.
    let probe_max_tokens = 4096.min(provider.context_tokens / 2);

    let with_tool = ChatRequest {
        messages: vec![Message::User(
            "Call the `ping` tool once with no arguments. Do not reply with text.".into(),
        )],
        tools: vec![ToolSpec {
            name: "ping".into(),
            description: "Connectivity check. Takes no arguments.".into(),
            parameters: serde_json::json!({ "type": "object", "properties": {} }),
        }],
        max_tokens: Some(probe_max_tokens),
        temperature: Some(0.0),
        ..Default::default()
    };
    match complete(&client, provider, &with_tool, &mut ignore, &cancel).await {
        Ok(response) if response.truncated => {
            return finish(
                ToolCalling::Unknown,
                Some(ProviderError::InvalidResponse(
                    "Model reached the output limit during the connection test.".into(),
                )),
            );
        }
        Ok(response) if response.tool_calls.iter().any(|c| c.name == "ping") => {
            return finish(ToolCalling::Supported, None);
        }
        Ok(_) => return finish(ToolCalling::Unsupported, None),
        // Some servers reject the `tools` field outright; retry without it to
        // tell "no tool support" apart from "endpoint unreachable".
        Err(ProviderError::BadRequest(_)) => {}
        Err(e) => return finish(ToolCalling::Unknown, Some(e)),
    }

    let plain = ChatRequest {
        messages: vec![Message::User("Reply with the single word OK.".into())],
        max_tokens: Some(probe_max_tokens),
        temperature: Some(0.0),
        ..Default::default()
    };
    match complete(&client, provider, &plain, &mut ignore, &cancel).await {
        Ok(response) if response.truncated => finish(
            ToolCalling::Unknown,
            Some(ProviderError::InvalidResponse(
                "Model reached the output limit during the connection test.".into(),
            )),
        ),
        Ok(_) => finish(ToolCalling::Unsupported, None),
        Err(e) => finish(ToolCalling::Unknown, Some(e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::config::{ProviderKind, ToolCalling};
    use axum::{http::HeaderMap, routing::post, Router};

    fn image_request(data: &str) -> ChatRequest {
        ChatRequest {
            messages: vec![
                Message::User("Read visual evidence.".into()),
                Message::Assistant {
                    text: String::new(),
                    reasoning: Value::Null,
                    tool_calls: vec![ToolCall {
                        id: "read".into(),
                        name: "view_snapshot".into(),
                        arguments: "{}".into(),
                    }],
                },
                Message::ToolResult {
                    call_id: "read".into(),
                    content: "{\"screenshot_id\":7}".into(),
                },
                Message::SnapshotImage {
                    handle: "opaque-handle".into(),
                    call_id: "read".into(),
                    screenshot_id: 7,
                    image: ImagePayload {
                        data: Arc::from(data),
                        mime_type: "image/png".into(),
                        width: 1920,
                        height: 768,
                        detail: ImageDetail::High,
                    },
                },
            ],
            ..Default::default()
        }
    }

    #[test]
    fn images_follow_paired_tool_results_in_both_protocols_and_base64_is_not_text_budget() {
        let request = image_request("synthetic-pixels");
        let openai = openai_compat::request_body("gpt-6-astra", &request);
        assert_eq!(openai["messages"][2]["role"], "tool");
        assert_eq!(openai["messages"][3]["role"], "user");
        assert_eq!(
            openai["messages"][3]["content"][1]["image_url"]["url"],
            "data:image/png;base64,synthetic-pixels"
        );
        assert_eq!(
            openai["messages"][3]["content"][1]["image_url"]["detail"],
            "high"
        );
        let anthropic = anthropic::request_body("claude", &request);
        let user = anthropic["messages"].as_array().unwrap().last().unwrap();
        assert_eq!(user["content"][0]["type"], "tool_result");
        assert_eq!(user["content"][2]["type"], "image");
        assert_eq!(user["content"][2]["source"]["media_type"], "image/png");
        assert_eq!(user["content"][2]["source"]["data"], "synthetic-pixels");
        for kind in [ProviderKind::OpenaiCompatible, ProviderKind::Anthropic] {
            let estimate = super::super::context::estimated_input_tokens(kind, &request);
            assert!(estimate > super::super::vision::estimated_image_tokens(1920, 768));
            assert_eq!(
                estimate,
                super::super::context::estimated_input_tokens(
                    kind,
                    &image_request(&"x".repeat(100_000))
                )
            );
        }
    }

    #[tokio::test]
    async fn image_requests_require_both_permission_and_confirmed_capability() {
        let request = image_request("synthetic-pixels");
        let mut config = ResolvedProvider {
            base_url: "http://127.0.0.1:1/v1".into(),
            image_input: ImageInput::Supported,
            ..Default::default()
        };
        let client = http_client().unwrap();
        let cancel = Cancellation::default();
        let mut ignore = |_| {};
        assert_eq!(
            complete(&client, &config, &request, &mut ignore, &cancel)
                .await
                .unwrap_err(),
            ProviderError::ImagesDisabled
        );
        config.allow_images = true;
        config.image_input = ImageInput::Unknown;
        assert_eq!(
            complete(&client, &config, &request, &mut ignore, &cancel)
                .await
                .unwrap_err(),
            ProviderError::ImagesDisabled
        );
    }

    #[test]
    fn forced_answers_retain_tool_schemas_and_system_prefix_in_both_protocols() {
        let mut request = ChatRequest {
            messages: vec![
                Message::System("fixed rules".into()),
                Message::User("question".into()),
            ],
            tools: vec![ToolSpec {
                name: "search".into(),
                description: "Search".into(),
                parameters: serde_json::json!({"type":"object"}),
            }],
            ..Default::default()
        };
        for kind in [ProviderKind::OpenaiCompatible, ProviderKind::Anthropic] {
            request.disable_tools = false;
            let before = request_body_for_estimate(kind, &request);
            request.disable_tools = true;
            let after = request_body_for_estimate(kind, &request);
            assert_eq!(before["tools"], after["tools"]);
            assert_eq!(before["messages"], after["messages"]);
            assert_eq!(before["system"], after["system"]);
            let expected = if kind == ProviderKind::Anthropic {
                serde_json::json!({"type":"none"})
            } else {
                serde_json::json!("none")
            };
            assert_eq!(after["tool_choice"], expected);
        }
    }

    /// Serves `body` as an event stream on a loopback port and records the
    /// credential header each request carried.
    async fn serve(
        path: &'static str,
        body: &'static str,
        header: &'static str,
    ) -> (String, Arc<std::sync::Mutex<Vec<String>>>) {
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorder = seen.clone();
        let app = Router::new().route(
            path,
            post(move |headers: HeaderMap| {
                let recorder = recorder.clone();
                async move {
                    let value = headers
                        .get(header)
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or("")
                        .to_string();
                    recorder.lock().unwrap().push(value);
                    ([("content-type", "text/event-stream")], body)
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (format!("http://{addr}"), seen)
    }

    fn provider(kind: ProviderKind, base_url: String) -> ResolvedProvider {
        ResolvedProvider {
            kind,
            base_url,
            model: "m".into(),
            api_key: Some("secret".into()),
            tool_calling: ToolCalling::Unknown,
            context_tokens: super::super::config::DEFAULT_CONTEXT_TOKENS,
            ..Default::default()
        }
    }

    async fn run(provider: &ResolvedProvider) -> (ChatResponse, String) {
        let request = ChatRequest {
            messages: vec![Message::User("hi".into())],
            ..Default::default()
        };
        let mut streamed = String::new();
        let mut on_event = |event: StreamEvent| {
            if let StreamEvent::TextDelta(text) = event {
                streamed.push_str(&text);
            }
        };
        let response = complete(
            &http_client().unwrap(),
            provider,
            &request,
            &mut on_event,
            &Cancellation::default(),
        )
        .await
        .unwrap();
        (response, streamed)
    }

    #[tokio::test]
    async fn openai_compatible_streams_over_http() {
        let body = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"Hel\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"lo\"}}]}\n\n",
            "data: [DONE]\n\n",
        );
        let (base, seen) = serve("/v1/chat/completions", body, "authorization").await;
        let (response, streamed) = run(&provider(
            ProviderKind::OpenaiCompatible,
            format!("{base}/v1"),
        ))
        .await;
        assert_eq!(response.text, "Hello");
        assert_eq!(streamed, "Hello");
        assert_eq!(seen.lock().unwrap().as_slice(), ["Bearer secret"]);
    }

    #[tokio::test]
    async fn anthropic_streams_over_http() {
        let body = concat!(
            "event: message_start\n",
            "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":3,\"output_tokens\":1}}}\n\n",
            "event: content_block_start\n",
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hi\"}}\n\n",
            "event: message_stop\n",
            "data: {\"type\":\"message_stop\"}\n\n",
        );
        let (base, seen) = serve("/v1/messages", body, "x-api-key").await;
        let (response, streamed) = run(&provider(ProviderKind::Anthropic, base)).await;
        assert_eq!(response.text, "Hi");
        assert_eq!(streamed, "Hi");
        assert_eq!(response.usage.map(|u| u.input_tokens), Some(3));
        assert_eq!(seen.lock().unwrap().as_slice(), ["secret"]);
    }

    #[tokio::test]
    async fn truncated_reasoning_does_not_mark_tools_unsupported() {
        let body = concat!(
            "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"thinking\"},\"finish_reason\":\"length\"}]}\n\n",
            "data: [DONE]\n\n",
        );
        let (base, _) = serve("/v1/chat/completions", body, "authorization").await;
        let result = test_connection(&provider(
            ProviderKind::OpenaiCompatible,
            format!("{base}/v1"),
        ))
        .await;
        assert!(!result.ok);
        assert_eq!(result.tool_calling, ToolCalling::Unknown);
        assert!(result.error.unwrap().starts_with("AI_INVALID_RESPONSE"));
    }

    #[tokio::test]
    async fn http_errors_become_provider_errors() {
        let (base, _) = serve("/elsewhere", "", "authorization").await;
        let request = ChatRequest::default();
        let error = complete(
            &http_client().unwrap(),
            &provider(ProviderKind::OpenaiCompatible, base),
            &request,
            &mut |_| {},
            &Cancellation::default(),
        )
        .await
        .unwrap_err();
        assert!(matches!(error, ProviderError::NotFound(_)));
    }

    #[test]
    fn error_bodies_are_reduced_to_their_message() {
        let body = r#"{"error":{"message":"Model not found","type":"invalid_request_error"}}"#;
        assert_eq!(
            ProviderError::from_status(404, body).to_string(),
            "AI_NOT_FOUND: Model not found"
        );
        assert_eq!(
            ProviderError::from_status(401, body).to_string(),
            "AI_UNAUTHORIZED"
        );
        assert_eq!(
            ProviderError::from_status(400, r#"{"error":"bad tools"}"#),
            ProviderError::BadRequest("bad tools".into())
        );
        assert_eq!(
            ProviderError::from_status(502, "gateway down").to_string(),
            "AI_SERVER_ERROR: HTTP 502: gateway down"
        );
    }

    #[tokio::test]
    async fn cancellation_wakes_waiters_and_is_sticky() {
        let cancel = Cancellation::new();
        let waiter = {
            let cancel = cancel.clone();
            tokio::spawn(async move { cancel.cancelled().await })
        };
        tokio::task::yield_now().await;
        cancel.cancel();
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            waiter.await.unwrap();
            cancel.cancelled().await;
        })
        .await
        .expect("cancellation must wake current and future waiters");
    }
}
