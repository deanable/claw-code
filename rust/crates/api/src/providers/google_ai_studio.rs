use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Deserialize;
use serde_json::{json, Map, Value};

use crate::error::ApiError;
use crate::http_client::build_http_client_or_default;
use crate::types::{
    ContentBlockDelta, ContentBlockDeltaEvent, ContentBlockStartEvent, ContentBlockStopEvent,
    InputContentBlock, InputMessage, MessageDelta, MessageDeltaEvent, MessageRequest,
    MessageResponse, MessageStartEvent, MessageStopEvent, OutputContentBlock, StreamEvent,
    ToolChoice, ToolDefinition, ToolResultContentBlock, Usage,
};

use super::{preflight_message_request, Provider, ProviderFuture};

pub const DEFAULT_GOOGLE_BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta";
const GOOGLE_ENV_VARS: &[&str] = &["GOOGLE_API_KEY"];
static RESPONSE_ID_COUNTER: AtomicU64 = AtomicU64::new(1);
const GOOGLE_FREE_TIER_MAX_REQUESTS_PER_MINUTE: u64 = 10;
const RATE_LIMIT_WINDOW_SECS: u64 = 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GoogleAiStudioConfig {
    pub api_key_env: &'static str,
    pub base_url_env: &'static str,
    pub default_base_url: &'static str,
}

impl GoogleAiStudioConfig {
    #[must_use]
    pub const fn google() -> Self {
        Self {
            api_key_env: "GOOGLE_API_KEY",
            base_url_env: "GOOGLE_BASE_URL",
            default_base_url: DEFAULT_GOOGLE_BASE_URL,
        }
    }

    #[must_use]
    pub const fn credential_env_vars(self) -> &'static [&'static str] {
        GOOGLE_ENV_VARS
    }
}

#[derive(Debug)]
pub struct GoogleAiStudioClient {
    http: reqwest::Client,
    api_key: String,
    config: GoogleAiStudioConfig,
    base_url: String,
    respect_rate_limits: bool,
    request_count: AtomicU64,
    window_start: Mutex<SystemTime>,
}

impl Clone for GoogleAiStudioClient {
    fn clone(&self) -> Self {
        Self {
            http: self.http.clone(),
            api_key: self.api_key.clone(),
            config: self.config,
            base_url: self.base_url.clone(),
            respect_rate_limits: self.respect_rate_limits,
            request_count: AtomicU64::new(0),
            window_start: Mutex::new(SystemTime::now()),
        }
    }
}

impl GoogleAiStudioClient {
    #[must_use]
    pub fn new(api_key: impl Into<String>, config: GoogleAiStudioConfig) -> Self {
        Self {
            http: build_http_client_or_default(),
            api_key: api_key.into(),
            config,
            base_url: read_base_url(config),
            respect_rate_limits: std::env::var("CLAW_RESPECT_RATE_LIMITS")
                .map(|v| v == "true")
                .unwrap_or(true),
            request_count: AtomicU64::new(0),
            window_start: Mutex::new(SystemTime::now()),
        }
    }

    pub fn from_env(config: GoogleAiStudioConfig) -> Result<Self, ApiError> {
        let Some(api_key) = read_env_non_empty(config.api_key_env)? else {
            return Err(ApiError::missing_credentials(
                "Google AI Studio",
                config.credential_env_vars(),
            ));
        };
        Ok(Self::new(api_key, config))
    }

    #[must_use]
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub async fn send_message(
        &self,
        request: &MessageRequest,
    ) -> Result<MessageResponse, ApiError> {
        let request = MessageRequest {
            stream: false,
            ..request.clone()
        };
        preflight_message_request(&request)?;
        let wire_model = strip_routing_prefix(&request.model);
        let endpoint = generate_content_endpoint(&self.base_url, &wire_model);
        let payload = build_generate_content_request(&request);
        if self.respect_rate_limits {
            self.wait_for_rate_limit().await;
        }

        let response = self
            .http
            .post(endpoint)
            .header("content-type", "application/json")
            .header("x-goog-api-key", self.api_key.trim())
            .json(&payload)
            .send()
            .await
            .map_err(ApiError::from)?;
        if self.respect_rate_limits && response.status().is_success() {
            self.record_request();
        }

        let status = response.status();
        let body = response.text().await.map_err(ApiError::from)?;
        if !status.is_success() {
            return Err(parse_google_error(status, &body));
        }

        let parsed = serde_json::from_str::<GenerateContentResponse>(&body).map_err(|error| {
            ApiError::json_deserialize("Google AI Studio", &request.model, &body, error)
        })?;
        normalize_response(&request.model, parsed)
    }

    pub async fn stream_message(
        &self,
        request: &MessageRequest,
    ) -> Result<MessageStream, ApiError> {
        let response = self.send_message(request).await?;
        Ok(MessageStream::from_response(response))
    }

    async fn wait_for_rate_limit(&self) {
        loop {
            let now = SystemTime::now();
            let elapsed = {
                let mut window = self
                    .window_start
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let elapsed = now
                    .duration_since(*window)
                    .map(|d| d.as_secs())
                    .unwrap_or(RATE_LIMIT_WINDOW_SECS + 1);

                if elapsed >= RATE_LIMIT_WINDOW_SECS {
                    *window = now;
                    self.request_count.store(0, Ordering::SeqCst);
                    return;
                }
                elapsed
            };

            if self.request_count.load(Ordering::SeqCst) < GOOGLE_FREE_TIER_MAX_REQUESTS_PER_MINUTE
            {
                return;
            }
            tokio::time::sleep(Duration::from_secs(
                RATE_LIMIT_WINDOW_SECS.saturating_sub(elapsed),
            ))
            .await;
        }
    }

    fn record_request(&self) {
        self.request_count.fetch_add(1, Ordering::SeqCst);
    }
}

impl Provider for GoogleAiStudioClient {
    type Stream = MessageStream;

    fn send_message<'a>(
        &'a self,
        request: &'a MessageRequest,
    ) -> ProviderFuture<'a, MessageResponse> {
        Box::pin(async move { self.send_message(request).await })
    }

    fn stream_message<'a>(
        &'a self,
        request: &'a MessageRequest,
    ) -> ProviderFuture<'a, Self::Stream> {
        Box::pin(async move { self.stream_message(request).await })
    }
}

#[derive(Debug)]
pub struct MessageStream {
    request_id: Option<String>,
    pending: VecDeque<StreamEvent>,
}

impl MessageStream {
    fn from_response(response: MessageResponse) -> Self {
        let mut pending = VecDeque::new();
        pending.push_back(StreamEvent::MessageStart(MessageStartEvent {
            message: MessageResponse {
                id: response.id.clone(),
                kind: response.kind.clone(),
                role: response.role.clone(),
                content: Vec::new(),
                model: response.model.clone(),
                stop_reason: None,
                stop_sequence: None,
                usage: Usage::default(),
                request_id: response.request_id.clone(),
            },
        }));

        for (index, block) in response.content.iter().enumerate() {
            let index = index as u32;
            match block {
                OutputContentBlock::Text { text } => {
                    pending.push_back(StreamEvent::ContentBlockStart(ContentBlockStartEvent {
                        index,
                        content_block: OutputContentBlock::Text {
                            text: String::new(),
                        },
                    }));
                    if !text.is_empty() {
                        pending.push_back(StreamEvent::ContentBlockDelta(ContentBlockDeltaEvent {
                            index,
                            delta: ContentBlockDelta::TextDelta { text: text.clone() },
                        }));
                    }
                    pending.push_back(StreamEvent::ContentBlockStop(ContentBlockStopEvent {
                        index,
                    }));
                }
                OutputContentBlock::ToolUse { id, name, input } => {
                    pending.push_back(StreamEvent::ContentBlockStart(ContentBlockStartEvent {
                        index,
                        content_block: OutputContentBlock::ToolUse {
                            id: id.clone(),
                            name: name.clone(),
                            input: json!({}),
                        },
                    }));
                    let serialized = input.to_string();
                    if !serialized.is_empty() {
                        pending.push_back(StreamEvent::ContentBlockDelta(ContentBlockDeltaEvent {
                            index,
                            delta: ContentBlockDelta::InputJsonDelta {
                                partial_json: serialized,
                            },
                        }));
                    }
                    pending.push_back(StreamEvent::ContentBlockStop(ContentBlockStopEvent {
                        index,
                    }));
                }
                OutputContentBlock::Thinking {
                    thinking,
                    signature,
                } => {
                    pending.push_back(StreamEvent::ContentBlockStart(ContentBlockStartEvent {
                        index,
                        content_block: OutputContentBlock::Thinking {
                            thinking: String::new(),
                            signature: None,
                        },
                    }));
                    if !thinking.is_empty() {
                        pending.push_back(StreamEvent::ContentBlockDelta(ContentBlockDeltaEvent {
                            index,
                            delta: ContentBlockDelta::ThinkingDelta {
                                thinking: thinking.clone(),
                            },
                        }));
                    }
                    if let Some(signature) = signature {
                        pending.push_back(StreamEvent::ContentBlockDelta(ContentBlockDeltaEvent {
                            index,
                            delta: ContentBlockDelta::SignatureDelta {
                                signature: signature.clone(),
                            },
                        }));
                    }
                    pending.push_back(StreamEvent::ContentBlockStop(ContentBlockStopEvent {
                        index,
                    }));
                }
                OutputContentBlock::RedactedThinking { .. } => {}
            }
        }

        pending.push_back(StreamEvent::MessageDelta(MessageDeltaEvent {
            delta: MessageDelta {
                stop_reason: response.stop_reason.clone(),
                stop_sequence: response.stop_sequence.clone(),
            },
            usage: response.usage.clone(),
        }));
        pending.push_back(StreamEvent::MessageStop(MessageStopEvent {}));

        Self {
            request_id: response.request_id,
            pending,
        }
    }

    #[must_use]
    pub fn request_id(&self) -> Option<&str> {
        self.request_id.as_deref()
    }

    pub async fn next_event(&mut self) -> Result<Option<StreamEvent>, ApiError> {
        Ok(self.pending.pop_front())
    }
}

#[derive(Debug, Deserialize)]
struct GenerateContentResponse {
    #[serde(default)]
    candidates: Vec<GoogleCandidate>,
    #[serde(default, rename = "usageMetadata", alias = "usage_metadata")]
    usage_metadata: Option<GoogleUsageMetadata>,
    #[serde(default, rename = "modelVersion", alias = "model_version")]
    model_version: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GoogleCandidate {
    #[serde(default)]
    content: Option<GoogleContent>,
    #[serde(default, rename = "finishReason", alias = "finish_reason")]
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GoogleContent {
    #[serde(default)]
    parts: Vec<GooglePart>,
}

#[derive(Debug, Deserialize)]
struct GooglePart {
    #[serde(default)]
    text: Option<String>,
    #[serde(default, rename = "functionCall", alias = "function_call")]
    function_call: Option<GoogleFunctionCall>,
}

#[derive(Debug, Deserialize)]
struct GoogleFunctionCall {
    name: String,
    #[serde(default)]
    args: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct GoogleUsageMetadata {
    #[serde(default, rename = "promptTokenCount", alias = "prompt_token_count")]
    prompt_token_count: u32,
    #[serde(
        default,
        rename = "candidatesTokenCount",
        alias = "candidates_token_count"
    )]
    candidates_token_count: u32,
    #[serde(default, rename = "totalTokenCount", alias = "total_token_count")]
    total_token_count: u32,
}

fn build_generate_content_request(request: &MessageRequest) -> Value {
    let mut payload = Map::new();
    let mut tool_name_by_id = BTreeMap::new();
    let mut contents = Vec::new();
    for message in &request.messages {
        if message.role == "assistant" {
            for block in &message.content {
                if let InputContentBlock::ToolUse { id, name, .. } = block {
                    tool_name_by_id.insert(id.clone(), name.clone());
                }
            }
        }
        if let Some(content) = translate_message(message, &tool_name_by_id) {
            contents.push(content);
        }
    }
    payload.insert("contents".to_string(), Value::Array(contents));

    if let Some(system) = request
        .system
        .as_ref()
        .filter(|value| !value.trim().is_empty())
    {
        payload.insert(
            "systemInstruction".to_string(),
            json!({ "parts": [{ "text": system }] }),
        );
    }

    if let Some(tools) = &request.tools {
        if !tools.is_empty() {
            payload.insert(
                "tools".to_string(),
                json!([{
                    "functionDeclarations": tools.iter().map(google_tool_definition).collect::<Vec<_>>()
                }]),
            );
        }
    }

    if let Some(tool_choice) = &request.tool_choice {
        payload.insert("toolConfig".to_string(), google_tool_config(tool_choice));
    }

    let mut generation_config = Map::new();
    generation_config.insert("maxOutputTokens".to_string(), json!(request.max_tokens));
    if let Some(temperature) = request.temperature {
        generation_config.insert("temperature".to_string(), json!(temperature));
    }
    if let Some(top_p) = request.top_p {
        generation_config.insert("topP".to_string(), json!(top_p));
    }
    if let Some(stop) = &request.stop {
        if !stop.is_empty() {
            generation_config.insert("stopSequences".to_string(), json!(stop));
        }
    }
    payload.insert(
        "generationConfig".to_string(),
        Value::Object(generation_config),
    );

    Value::Object(payload)
}

fn translate_message(
    message: &InputMessage,
    tool_name_by_id: &BTreeMap<String, String>,
) -> Option<Value> {
    let role = if message.role == "assistant" {
        "model"
    } else {
        "user"
    };
    let mut parts = Vec::new();
    for block in &message.content {
        match block {
            InputContentBlock::Text { text } if !text.is_empty() => {
                parts.push(json!({ "text": text }));
            }
            InputContentBlock::Text { .. } => {}
            InputContentBlock::ToolUse { name, input, .. } if message.role == "assistant" => {
                parts.push(json!({
                    "functionCall": {
                        "name": name,
                        "args": input,
                    }
                }));
            }
            InputContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => {
                let response = google_tool_result_value(content, *is_error);
                let name = tool_name_by_id
                    .get(tool_use_id)
                    .cloned()
                    .or_else(|| infer_tool_response_name(content))
                    .unwrap_or_else(|| "tool".to_string());
                parts.push(json!({
                    "functionResponse": {
                        "name": name,
                        "response": response,
                    }
                }));
            }
            InputContentBlock::ToolUse { .. } => {}
        }
    }

    (!parts.is_empty()).then_some(json!({
        "role": role,
        "parts": parts,
    }))
}

fn google_tool_definition(tool: &ToolDefinition) -> Value {
    let mut parameters = tool.input_schema.clone();
    strip_unsupported_google_schema_fields(&mut parameters);
    json!({
        "name": tool.name,
        "description": tool.description,
        "parameters": parameters,
    })
}

fn google_tool_config(tool_choice: &ToolChoice) -> Value {
    match tool_choice {
        ToolChoice::Auto => json!({
            "functionCallingConfig": { "mode": "AUTO" }
        }),
        ToolChoice::Any => json!({
            "functionCallingConfig": { "mode": "ANY" }
        }),
        ToolChoice::Tool { name } => json!({
            "functionCallingConfig": {
                "mode": "ANY",
                "allowedFunctionNames": [name],
            }
        }),
    }
}

fn google_tool_result_value(content: &[ToolResultContentBlock], is_error: bool) -> Value {
    let flattened = flatten_tool_result_content(content);
    let mut object = Map::new();
    object.insert("content".to_string(), Value::String(flattened));
    if is_error {
        object.insert("is_error".to_string(), Value::Bool(true));
    }
    Value::Object(object)
}

fn infer_tool_response_name(content: &[ToolResultContentBlock]) -> Option<String> {
    content.iter().find_map(|block| match block {
        ToolResultContentBlock::Json { value } => value
            .as_object()
            .and_then(|object| object.get("tool_name"))
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        ToolResultContentBlock::Text { .. } => None,
    })
}

fn flatten_tool_result_content(content: &[ToolResultContentBlock]) -> String {
    content
        .iter()
        .map(|block| match block {
            ToolResultContentBlock::Text { text } => text.clone(),
            ToolResultContentBlock::Json { value } => value.to_string(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn strip_unsupported_google_schema_fields(schema: &mut Value) {
    match schema {
        Value::Object(object) => {
            object.remove("additionalProperties");
            object.remove("$schema");
            object.remove("unevaluatedProperties");
            for value in object.values_mut() {
                strip_unsupported_google_schema_fields(value);
            }
        }
        Value::Array(items) => {
            for item in items {
                strip_unsupported_google_schema_fields(item);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

fn normalize_response(
    requested_model: &str,
    response: GenerateContentResponse,
) -> Result<MessageResponse, ApiError> {
    let candidate = response
        .candidates
        .into_iter()
        .next()
        .ok_or(ApiError::InvalidSseFrame(
            "google generateContent response missing candidates",
        ))?;

    let mut content = Vec::new();
    if let Some(message) = candidate.content {
        for part in message.parts {
            if let Some(text) = part.text.filter(|value| !value.is_empty()) {
                content.push(OutputContentBlock::Text { text });
            }
            if let Some(function_call) = part.function_call {
                content.push(OutputContentBlock::ToolUse {
                    id: next_response_id("google-tool"),
                    name: function_call.name,
                    input: function_call.args.unwrap_or_else(|| json!({})),
                });
            }
        }
    }

    let usage = response
        .usage_metadata
        .map(|usage| Usage {
            input_tokens: usage.prompt_token_count,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
            output_tokens: if usage.candidates_token_count == 0 {
                usage
                    .total_token_count
                    .saturating_sub(usage.prompt_token_count)
            } else {
                usage.candidates_token_count
            },
        })
        .unwrap_or_default();

    Ok(MessageResponse {
        id: next_response_id("google-msg"),
        kind: "message".to_string(),
        role: "assistant".to_string(),
        content,
        model: response
            .model_version
            .unwrap_or_else(|| strip_routing_prefix(requested_model)),
        stop_reason: candidate.finish_reason.map(normalize_finish_reason),
        stop_sequence: None,
        usage,
        request_id: None,
    })
}

fn parse_google_error(status: reqwest::StatusCode, body: &str) -> ApiError {
    let parsed = serde_json::from_str::<Value>(body).ok();
    let error_object = parsed
        .as_ref()
        .and_then(|value| value.get("error"))
        .and_then(Value::as_object);

    let message = error_object
        .and_then(|object| object.get("message"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let error_type = error_object
        .and_then(|object| object.get("status"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let retryable = matches!(status.as_u16(), 408 | 429 | 500 | 502 | 503 | 504)
        || error_type
            .as_deref()
            .is_some_and(|value| value.eq_ignore_ascii_case("RESOURCE_EXHAUSTED"))
        || message
            .as_deref()
            .is_some_and(looks_like_google_rate_limit_message)
        || looks_like_google_rate_limit_message(body);

    ApiError::Api {
        status,
        error_type,
        message,
        request_id: None,
        body: body.to_string(),
        retryable,
        suggested_action: None,
    }
}

fn looks_like_google_rate_limit_message(message: &str) -> bool {
    let lowered = message.to_ascii_lowercase();
    [
        "quota",
        "resource_exhausted",
        "rate limit",
        "too many requests",
        "exhausted",
    ]
    .iter()
    .any(|marker| lowered.contains(marker))
}

fn generate_content_endpoint(base_url: &str, model: &str) -> String {
    let normalized_base = normalize_google_base_url(base_url);
    format!(
        "{}/models/{}:generateContent",
        normalized_base.trim_end_matches('/'),
        model
    )
}

fn normalize_google_base_url(base_url: &str) -> String {
    let trimmed = base_url.trim().trim_end_matches('/');
    if trimmed.ends_with("/v1") {
        format!("{}{}", trimmed.trim_end_matches("/v1"), "/v1beta")
    } else {
        trimmed.to_string()
    }
}

fn strip_routing_prefix(model: &str) -> String {
    model
        .trim()
        .strip_prefix("google/")
        .unwrap_or(model.trim())
        .to_string()
}

fn normalize_finish_reason(reason: String) -> String {
    match reason.as_str() {
        "STOP" => "end_turn".to_string(),
        "MAX_TOKENS" => "max_tokens".to_string(),
        "SAFETY" => "stop_sequence".to_string(),
        "MALFORMED_FUNCTION_CALL" => "tool_use".to_string(),
        _ => reason.to_ascii_lowercase(),
    }
}

fn next_response_id(prefix: &str) -> String {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .map_or(0, |duration| duration.as_millis());
    let counter = RESPONSE_ID_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{prefix}-{millis}-{counter}")
}

fn read_env_non_empty(key: &str) -> Result<Option<String>, ApiError> {
    match std::env::var(key) {
        Ok(value) if !value.is_empty() => Ok(Some(value)),
        Ok(_) | Err(std::env::VarError::NotPresent) => Ok(super::dotenv_value(key)),
        Err(error) => Err(ApiError::from(error)),
    }
}

#[must_use]
pub fn has_api_key(key: &str) -> bool {
    read_env_non_empty(key)
        .ok()
        .and_then(std::convert::identity)
        .is_some()
}

#[must_use]
pub fn read_base_url(config: GoogleAiStudioConfig) -> String {
    std::env::var(config.base_url_env).unwrap_or_else(|_| config.default_base_url.to_string())
}
