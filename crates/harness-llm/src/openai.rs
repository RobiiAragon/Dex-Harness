use std::collections::HashMap;
use std::time::Duration;
use async_trait::async_trait;
use futures::StreamExt;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tracing::{debug, instrument};

use harness_core::{
    CompletionRequest, CompletionResponse, LlmProvider, ProviderError, Role, ToolCall,
};

#[derive(Debug, Clone)]
pub struct OpenAiProvider {
    base_url: String,
    api_key: Option<String>,
    model: String,
    client: Client,
}

impl OpenAiProvider {
    pub const DEFAULT_BASE_URL: &'static str = "https://api.openai.com/v1";
    pub const LM_STUDIO_BASE_URL: &'static str = "http://127.0.0.1:1234/v1";

    pub fn new(model: impl Into<String>) -> Self {
        Self {
            base_url: Self::DEFAULT_BASE_URL.to_string(),
            api_key: std::env::var("OPENAI_API_KEY").ok(),
            model: model.into(),
            client: Client::builder()
                .timeout(Duration::from_secs(120))
                .build()
                .unwrap_or_else(|_| Client::new()),
        }
    }

    /// Creates a provider configured for a local LM Studio / Bionic OpenAI-compatible server (`:1234/v1`).
    pub fn lm_studio(model: impl Into<String>) -> Self {
        Self {
            base_url: Self::LM_STUDIO_BASE_URL.to_string(),
            api_key: Some("lm-studio".to_string()),
            model: model.into(),
            client: Client::builder()
                .timeout(Duration::from_secs(180))
                .build()
                .unwrap_or_else(|_| Client::new()),
        }
    }

    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    pub fn with_api_key(mut self, api_key: impl Into<String>) -> Self {
        self.api_key = Some(api_key.into());
        self
    }
}

#[derive(Serialize)]
struct ChatCompletionRequest<'a> {
    model: &'a str,
    messages: Vec<ChatMessage>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<ChatTool>,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
}

#[derive(Serialize, Deserialize)]
struct ChatMessage {
    role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<ChatToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct ChatTool {
    #[serde(rename = "type")]
    tool_type: String,
    function: ChatFunction,
}

#[derive(Serialize, Deserialize)]
struct ChatFunction {
    name: String,
    description: String,
    parameters: serde_json::Value,
}

#[derive(Serialize, Deserialize)]
struct ChatToolCall {
    id: String,
    #[serde(rename = "type")]
    call_type: String,
    function: ChatCallFunction,
}

#[derive(Serialize, Deserialize)]
struct ChatCallFunction {
    name: String,
    arguments: String,
}

#[derive(Deserialize)]
struct ChatCompletionResponse {
    choices: Vec<ChatChoice>,
}

#[derive(Deserialize)]
struct ChatChoice {
    message: ChatMessage,
    finish_reason: Option<String>,
}

// Streaming chunk types
#[derive(Deserialize)]
struct ChatStreamChunk {
    choices: Vec<ChatStreamChoice>,
}

#[derive(Deserialize)]
struct ChatStreamChoice {
    delta: ChatStreamDelta,
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct ChatStreamDelta {
    content: Option<String>,
    tool_calls: Option<Vec<ChatStreamToolCall>>,
}

#[derive(Deserialize)]
struct ChatStreamToolCall {
    index: usize,
    id: Option<String>,
    function: Option<ChatStreamFunction>,
}

#[derive(Deserialize)]
struct ChatStreamFunction {
    name: Option<String>,
    arguments: Option<String>,
}

#[async_trait]
impl LlmProvider for OpenAiProvider {
    fn name(&self) -> &str {
        "openai"
    }

    #[instrument(skip(self, request), fields(model = %self.model))]
    async fn complete(&self, request: CompletionRequest) -> Result<CompletionResponse, ProviderError> {
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        let is_streaming = request.token_callback.is_some();
        let token_callback = request.token_callback.clone();

        let messages: Vec<ChatMessage> = request
            .messages
            .into_iter()
            .map(|m| {
                let role = match m.role {
                    Role::System => "system",
                    Role::User => "user",
                    Role::Assistant => "assistant",
                    Role::Tool => "tool",
                };
                ChatMessage {
                    role: role.to_string(),
                    content: m.content,
                    tool_calls: None,
                    tool_call_id: m.tool_call_id,
                }
            })
            .collect();

        let tools: Vec<ChatTool> = request
            .tools
            .into_iter()
            .map(|t| ChatTool {
                tool_type: "function".to_string(),
                function: ChatFunction {
                    name: t.name,
                    description: t.description,
                    parameters: t.parameters,
                },
            })
            .collect();

        let payload = ChatCompletionRequest {
            model: &self.model,
            messages,
            tools,
            stream: is_streaming,
            temperature: request.temperature,
            max_tokens: request.max_tokens,
        };

        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        if let Some(ref key) = self.api_key {
            let auth_val = format!("Bearer {key}");
            if let Ok(hv) = HeaderValue::from_str(&auth_val) {
                headers.insert(AUTHORIZATION, hv);
            }
        }

        debug!(url = %url, streaming = is_streaming, "Sending request to OpenAI chat completions");

        let response = self
            .client
            .post(&url)
            .headers(headers)
            .json(&payload)
            .send()
            .await
            .map_err(|e| ProviderError::Http(format!("Failed to reach API at {url}: {e}")))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(ProviderError::Custom {
                provider: "openai".to_string(),
                message: format!("HTTP {status}: {body}"),
            });
        }

        // Handle SSE streaming
        if is_streaming {
            let mut byte_stream = response.bytes_stream();
            let mut line_buffer = String::new();
            let mut accumulated_content = String::new();
            let mut tool_calls_map: HashMap<usize, (String, String, String)> = HashMap::new(); // index -> (id, name, args)
            let mut finish_reason = None;

            while let Some(chunk_result) = byte_stream.next().await {
                let chunk = chunk_result.map_err(|e| ProviderError::Http(e.to_string()))?;
                line_buffer.push_str(&String::from_utf8_lossy(&chunk));

                while let Some(pos) = line_buffer.find('\n') {
                    let line = line_buffer[..pos].trim().to_string();
                    line_buffer = line_buffer[pos + 1..].to_string();

                    if line.is_empty() {
                        continue;
                    }

                    if let Some(data_str) = line.strip_prefix("data: ") {
                        let trimmed_data = data_str.trim();
                        if trimmed_data == "[DONE]" {
                            break;
                        }

                        if let Ok(chunk_payload) = serde_json::from_str::<ChatStreamChunk>(trimmed_data) {
                            for choice in chunk_payload.choices {
                                if let Some(ref reason) = choice.finish_reason {
                                    finish_reason = Some(reason.clone());
                                }

                                if let Some(ref content) = choice.delta.content {
                                    if !content.is_empty() {
                                        if let Some(ref cb) = token_callback {
                                            cb(content);
                                        }
                                        accumulated_content.push_str(content);
                                    }
                                }

                                if let Some(tcs) = choice.delta.tool_calls {
                                    for tc in tcs {
                                        let entry = tool_calls_map.entry(tc.index).or_insert_with(|| {
                                            (
                                                tc.id.clone().unwrap_or_default(),
                                                String::new(),
                                                String::new(),
                                            )
                                        });

                                        if let Some(id) = tc.id {
                                            entry.0 = id;
                                        }
                                        if let Some(func) = tc.function {
                                            if let Some(name) = func.name {
                                                entry.1.push_str(&name);
                                            }
                                            if let Some(args) = func.arguments {
                                                entry.2.push_str(&args);
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            let mut tool_calls = Vec::new();
            let mut sorted_keys: Vec<_> = tool_calls_map.keys().copied().collect();
            sorted_keys.sort_unstable();

            for idx in sorted_keys {
                if let Some((id, name, args_str)) = tool_calls_map.remove(&idx) {
                    let parsed_args = serde_json::from_str(&args_str)
                        .unwrap_or(serde_json::Value::String(args_str));
                    tool_calls.push(ToolCall {
                        id: if id.is_empty() { format!("call_{idx}_{name}") } else { id },
                        name,
                        arguments: parsed_args,
                    });
                }
            }

            return Ok(CompletionResponse {
                content: if accumulated_content.is_empty() {
                    None
                } else {
                    Some(accumulated_content)
                },
                tool_calls,
                finish_reason,
            });
        }

        // Non-streaming response
        let resp: ChatCompletionResponse = response
            .json()
            .await
            .map_err(|e| ProviderError::ParsingResponse(e.to_string()))?;

        let choice = resp.choices.into_iter().next().ok_or_else(|| {
            ProviderError::ParsingResponse("No choices returned in API response".to_string())
        })?;

        let tool_calls: Vec<ToolCall> = choice
            .message
            .tool_calls
            .unwrap_or_default()
            .into_iter()
            .map(|tc| {
                let parsed_args = serde_json::from_str(&tc.function.arguments)
                    .unwrap_or(serde_json::Value::String(tc.function.arguments));
                ToolCall {
                    id: tc.id,
                    name: tc.function.name,
                    arguments: parsed_args,
                }
            })
            .collect();

        Ok(CompletionResponse {
            content: choice.message.content,
            tool_calls,
            finish_reason: choice.finish_reason,
        })
    }
}
